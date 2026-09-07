//! Turning a jittery per-detection face position into a camera move worth
//! watching.
//!
//! A face detector's box is not stable. Run BlazeFace on a person sitting
//! perfectly still and its centre wanders by one to two percent of the frame,
//! every sample, forever — the model is re-deciding from scratch each time and
//! has no memory of where it put the box before. Aim a crop straight at that
//! and the result is a camera with a tremor: not obviously "tracking", just
//! subtly, expensively wrong in a way viewers read as cheap.
//!
//! So nothing here aims at the detection. The detection sets a *target*, and a
//! separate, slower process walks the framing toward it. Five filters sit
//! between the two, each answering a different failure:
//!
//! | filter | what it kills |
//! |---|---|
//! | **deadband** | the resting tremor — a target move too small to be real is not a move |
//! | **glide** | steps — the framing eases toward the target instead of arriving |
//! | **speed clamp** | whip pans — however far the target jumped, the camera moves at a human rate |
//! | **jump confirmation** | a second face, or a false positive, yanking the frame away for one sample |
//! | **hold on loss** | the recentre lurch when you turn your head or reach off-camera |
//!
//! ## Why sampling every N frames does not make it steppy
//!
//! [`observe`](Smoother::observe) and [`advance`](Smoother::advance) are
//! separate calls on purpose. Detection is expensive so it runs every few
//! frames; the glide is arithmetic so it runs on *every* frame. The target
//! updates at 10 Hz and the framing still moves at the capture's full frame
//! rate, which is why a low detection cadence costs responsiveness — how
//! quickly the camera notices you moved — and not smoothness. Those are
//! usually confused, and conflating them is how detection cadence ends up
//! tuned by how the motion looks rather than by what it costs.
//!
//! ## Why the glide is exponential in `dt` rather than a fixed fraction
//!
//! `current += (target - current) * k` with a constant `k` is the obvious
//! smoothing, and it silently couples the motion to the frame rate: the same
//! `k` glides twice as fast at 60fps as at 30. Solving the same curve for
//! elapsed time instead — `alpha = 1 - exp(-dt / tau)` — makes `tau` an actual
//! duration, so "settles in about six tenths of a second" stays true when the
//! camera changes, when a frame is dropped, and when the capture queue stalls.

use crate::region::framing::Anchor;

/// How hard the framing resists the detector. Every field is a duration, a
/// distance in normalized frame widths, or a count — nothing here is in pixels,
/// so the same numbers hold across cameras.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Damping {
    /// Roughly how long the framing takes to close most of the distance to a
    /// new target. The single knob for "lazy camera" against "attentive one".
    pub smoothing_s: f64,
    /// A target move smaller than this, in normalized frame widths, is treated
    /// as detector noise and discarded. The resting tremor is around 0.01–0.02,
    /// so this is the number that decides whether a still subject holds a still
    /// frame.
    pub deadband: f64,
    /// Ceiling on how fast the framing may travel, in normalized widths per
    /// second, whatever the glide asks for.
    pub max_speed: f64,
    /// A target move larger than this is not believed on sight — see
    /// [`Damping::confirm`].
    pub jump: f64,
    /// How many consecutive detections have to agree before a move larger than
    /// [`jump`](Damping::jump) is accepted. Two samples at a 10 Hz cadence is
    /// a fifth of a second of "yes, really" — long enough to reject a
    /// single-sample false positive, short enough that genuinely swapping
    /// seats is not a visible stall.
    pub confirm: u32,
    /// Ease back to centre after this long with no face. `None` holds the last
    /// framing indefinitely, which is the default and usually what you want:
    /// looking away from the lens, or reaching off-camera for a coffee, reads
    /// as a lost face, and a camera that drifts to centre every time is worse
    /// than one that waits.
    pub recenter_after_s: Option<f64>,
}

impl Default for Damping {
    fn default() -> Self {
        Damping {
            smoothing_s: 0.6,
            deadband: 0.02,
            max_speed: 0.35,
            jump: 0.30,
            confirm: 2,
            recenter_after_s: None,
        }
    }
}

/// The framing's own idea of where to point, updated by detections and walked
/// forward by the clock.
#[derive(Debug)]
pub struct Smoother {
    damping: Damping,
    /// Where the framing is now. `None` until the first face is found, which is
    /// what keeps a session with no detections framed exactly as it would be
    /// with tracking switched off.
    current: Option<Anchor>,
    /// Where it is heading.
    target: Option<Anchor>,
    /// A large move waiting to be believed: the point, and how many detections
    /// have backed it so far.
    pending: Option<(Anchor, u32)>,
    /// Host-clock seconds of the last detection that found something.
    last_seen: Option<f64>,
    /// Host-clock seconds of the last [`advance`](Smoother::advance).
    last_step: Option<f64>,
    /// Counted for the sidecar, so a chapter can say how much of what the
    /// detector reported was thrown away rather than leaving it a mystery.
    pub rejected_as_noise: u64,
    pub rejected_as_jump: u64,
}

impl Smoother {
    pub fn new(damping: Damping) -> Smoother {
        Smoother {
            damping,
            current: None,
            target: None,
            pending: None,
            last_seen: None,
            last_step: None,
            rejected_as_noise: 0,
            rejected_as_jump: 0,
        }
    }

    /// The framing's current aim, or `None` before the first face lands.
    ///
    /// Read by the tests rather than by the tracker, which takes the same value
    /// as [`advance`](Smoother::advance)'s return so that reading it and
    /// stepping it cannot come apart.
    #[cfg(test)]
    pub fn anchor(&self) -> Option<Anchor> {
        self.current
    }

    /// A detection found a face at `point`, at host-clock second `t`.
    pub fn observe(&mut self, point: Anchor, t: f64) {
        if !point.0.is_finite() || !point.1.is_finite() {
            return;
        }
        self.last_seen = Some(t);

        // First acquisition snaps rather than glides. There is nothing to glide
        // *from* — the alternative is a swoop in from dead centre at the top of
        // every session, which looks like an effect rather than a camera.
        let Some(target) = self.target else {
            self.current = Some(point);
            self.target = Some(point);
            return;
        };

        let moved = distance(point, target);

        if moved > self.damping.jump {
            // Big moves are provisional. A second person stepping into frame,
            // or a one-sample false positive on a bookshelf, both look exactly
            // like this and both would otherwise yank the camera away and back.
            let agreed = match self.pending {
                Some((pending, seen)) if distance(point, pending) <= self.damping.jump => seen + 1,
                _ => 1,
            };
            if agreed >= self.damping.confirm.max(1) {
                self.pending = None;
                self.target = Some(point);
            } else {
                self.pending = Some((point, agreed));
                self.rejected_as_jump += 1;
            }
            return;
        }

        // A believable move cancels any half-confirmed jump: the subject is
        // demonstrably still here.
        self.pending = None;

        if moved < self.damping.deadband {
            self.rejected_as_noise += 1;
            return;
        }
        self.target = Some(point);
    }

    /// A detection ran and found nothing, at host-clock second `t`.
    ///
    /// Deliberately not the same as "no detection ran". The distinction is what
    /// makes [`Damping::recenter_after_s`] mean "the face has been gone this
    /// long" rather than "the detector has been idle this long", which are very
    /// different claims when the cadence is one sample in three frames.
    pub fn lost(&mut self, t: f64) {
        let _ = t;
        // The jump candidate does not survive the subject leaving: whatever it
        // was, it is not a continuation of anything on screen now.
        self.pending = None;
    }

    /// Walk the framing toward its target for the time elapsed since the last
    /// call, and return where it now points.
    ///
    /// Runs every frame, including frames no detection touched.
    pub fn advance(&mut self, t: f64) -> Option<Anchor> {
        let dt = match self.last_step {
            // Clamped at both ends: a non-monotonic or absurd timestamp — a
            // stalled queue, a clock the caller got wrong — should cost one
            // frame of motion, never a jump to the target or a division that
            // propagates NaN into a crop rect.
            Some(last) => (t - last).clamp(0.0, 0.5),
            None => 0.0,
        };
        self.last_step = Some(t);

        if let (Some(after), Some(seen)) = (self.damping.recenter_after_s, self.last_seen) {
            if t - seen > after {
                self.target = Some((0.5, 0.5));
            }
        }

        let (Some(current), Some(target)) = (self.current, self.target) else {
            return self.current;
        };
        if dt <= 0.0 {
            return Some(current);
        }

        // `tau` at or below zero means "no smoothing", which is a legitimate
        // setting for anyone who wants the raw target — and would otherwise be
        // a division by zero.
        let alpha = if self.damping.smoothing_s > 0.0 {
            1.0 - (-dt / self.damping.smoothing_s).exp()
        } else {
            1.0
        };
        // The clamp is on the *vector*, not on each axis. Clamping per axis
        // lets a diagonal move travel `sqrt(2)` times the configured ceiling —
        // 41% faster than asked for, on exactly the moves that are most visible
        // — and makes `max_speed` mean something different depending on the
        // direction of travel, which is not a speed limit.
        let ceiling = self.damping.max_speed.max(0.0) * dt;
        let delta = (
            (target.0 - current.0) * alpha,
            (target.1 - current.1) * alpha,
        );
        let length = (delta.0 * delta.0 + delta.1 * delta.1).sqrt();
        let scale = if length > ceiling && length > 0.0 {
            ceiling / length
        } else {
            1.0
        };
        let moved = (
            current.0 + delta.0 * scale,
            current.1 + delta.1 * scale,
        );
        self.current = Some(moved);
        Some(moved)
    }
}

fn distance(a: Anchor, b: Anchor) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

#[cfg(test)]
#[path = "smooth_tests.rs"]
mod tests;
