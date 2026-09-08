//! A single number easing toward a target, at a rate measured in seconds.
//!
//! [`Smoother`](super::smooth::Smoother) does this for a point, wrapped in the
//! four filters a *detector's* output needs — deadband, speed clamp, jump
//! confirmation, hold on loss. None of those apply to a number that comes from
//! a key being held: there is no noise to reject, no false positive to
//! disbelieve, and no such thing as losing it. What is left is the glide, and
//! this is the glide on its own.
//!
//! ## Why `exp` and not a fixed fraction
//!
//! The same reason [`super::smooth`] gives, restated because it is the only
//! interesting decision in the file: `current += (target - current) * k` with a
//! constant `k` silently couples the motion to the frame rate, easing twice as
//! fast at 60fps as at 30. Solving the same curve against elapsed time —
//! `alpha = 1 - exp(-dt / tau)` — makes `tau` an actual duration, so "about
//! four tenths of a second to punch in" stays true when the camera changes,
//! when a frame is dropped, and when the capture queue stalls.
//!
//! ## Why it never quite arrives, and why that is fine
//!
//! An exponential approach is asymptotic: it closes a fixed *proportion* of the
//! remaining distance each step, so it gets arbitrarily close and never lands
//! exactly. That matters here because the value drives a crop, and a crop that
//! is 0.001 away from fully wide is not the authored rect — it is a rect a
//! thousandth of a pixel off it, which snaps to the same integer origin and
//! renders identically. Callers that need to know whether it has *settled* ask
//! [`Glide::is_at`], which is what the latch uses to decide it may let go.

/// One eased scalar.
#[derive(Debug, Clone, Copy)]
pub struct Glide {
    current: f64,
    /// Roughly how long to close most of the distance to a new target.
    tau: f64,
    last: Option<f64>,
}

/// How close counts as arrived, for [`Glide::is_at`].
///
/// A thousandth of the 0..1 punch range. Below this the crop rounds to the
/// same whole pixel it would at the target, so the difference cannot reach a
/// frame.
const SETTLED: f64 = 1e-3;

impl Glide {
    pub fn new(start: f64, tau: f64) -> Glide {
        Glide {
            current: start,
            tau,
            last: None,
        }
    }

    /// Whether it has effectively reached `target`.
    ///
    /// The only reason a caller ever needs to inspect the value rather than
    /// use what [`advance`](Glide::advance) returned — which is why there is
    /// no plain accessor beside it.
    pub fn is_at(&self, target: f64) -> bool {
        (self.current - target).abs() < SETTLED
    }

    /// Ease toward `target`, given the current host-clock second.
    ///
    /// `t` is absolute rather than a delta, matching
    /// [`Smoother::advance`](super::smooth::Smoother::advance) so both can be
    /// driven from one frame timestamp without the caller keeping its own
    /// clock.
    pub fn advance(&mut self, target: f64, t: f64) -> f64 {
        let dt = match self.last {
            // Clamped at both ends for the reason `smooth` documents: a
            // non-monotonic or absurd timestamp should cost one frame of
            // motion, never a jump to the target or a division that puts NaN
            // into a crop rect.
            Some(last) => (t - last).clamp(0.0, 0.5),
            None => 0.0,
        };
        self.last = Some(t);

        if !target.is_finite() {
            return self.current;
        }
        if dt <= 0.0 {
            return self.current;
        }
        // `tau` at or below zero means "no easing", which is a legitimate
        // setting for anyone who wants the raw target, and would otherwise be
        // a division by zero.
        let alpha = if self.tau > 0.0 {
            1.0 - (-dt / self.tau).exp()
        } else {
            1.0
        };
        self.current += (target - self.current) * alpha;
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 30.0;

    /// Run `frames` frames toward `target` and return where it ended.
    ///
    /// The priming call matters and is not ceremony: the first `advance` has
    /// no previous timestamp to subtract, so it establishes the clock and
    /// moves nothing. Without it a run of `n` frames covers `n - 1` steps of
    /// elapsed time, and two runs at different frame rates would compare
    /// different durations — which looks exactly like the frame-rate coupling
    /// the test below exists to rule out.
    fn run(glide: &mut Glide, target: f64, frames: u32, step: f64) -> f64 {
        let mut t = 0.0;
        // The priming call returns the current value as well as setting the
        // clock, so it does the work an accessor would have.
        let mut value = glide.advance(target, t);
        for _ in 0..frames {
            t += step;
            value = glide.advance(target, t);
        }
        value
    }

    /// The property the `exp` form exists for: the same `tau` produces the
    /// same curve whatever the frame rate.
    ///
    /// A fixed-fraction glide passes every other test in this file and fails
    /// this one, and the failure in the product is a punch-in that is visibly
    /// faster on a 60fps camera than a 30fps one — which reads as the feature
    /// being inconsistent rather than as a frame-rate bug.
    #[test]
    fn the_glide_is_the_same_curve_at_any_frame_rate() {
        let mut slow = Glide::new(0.0, 0.4);
        let mut fast = Glide::new(0.0, 0.4);
        // One second of each.
        let at_30 = run(&mut slow, 1.0, 30, FRAME);
        let at_120 = run(&mut fast, 1.0, 120, FRAME / 4.0);
        assert!(
            (at_30 - at_120).abs() < 1e-6,
            "one second of easing reached {at_30} at 30fps but {at_120} at 120fps",
        );
    }

    /// It gets there, and `is_at` agrees that it has.
    #[test]
    fn it_arrives_and_says_so() {
        let mut glide = Glide::new(0.0, 0.4);
        let ended = run(&mut glide, 1.0, 90, FRAME);
        assert!(ended > 0.99, "three seconds of easing only reached {ended}");
        assert!(
            glide.is_at(1.0),
            "arrived at {ended} but is_at says otherwise"
        );
        assert!(!glide.is_at(0.0), "claims to be at both ends at once");
    }

    /// A stalled capture queue must cost one frame of motion, not a jump.
    ///
    /// The failure this prevents is the punch snapping fully in the instant
    /// the queue recovers, which on a recording reads as a cut rather than a
    /// move.
    #[test]
    fn a_stalled_queue_costs_one_frame_not_a_jump() {
        let mut glide = Glide::new(0.0, 0.4);
        glide.advance(1.0, 0.0);
        // Ten seconds later, as if the queue had hung.
        let after = glide.advance(1.0, 10.0);
        assert!(
            after < 0.75,
            "a ten-second gap moved the glide to {after} — dt was not clamped",
        );
    }

    /// Nonsense targets leave it where it is rather than poisoning the crop.
    #[test]
    fn a_nonsense_target_does_not_move_it() {
        let mut glide = Glide::new(0.5, 0.4);
        let before = glide.advance(1.0, 0.0);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let after = glide.advance(bad, 1.0);
            assert_eq!(after, before, "target {bad} moved the glide");
        }
    }

    /// `tau` at zero is "no easing", not a division by zero.
    #[test]
    fn a_zero_tau_snaps_rather_than_dividing_by_zero() {
        let mut glide = Glide::new(0.0, 0.0);
        glide.advance(1.0, 0.0);
        let after = glide.advance(1.0, FRAME);
        assert!((after - 1.0).abs() < 1e-12, "expected a snap, got {after}");
    }
}
