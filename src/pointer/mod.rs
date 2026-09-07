//! Following the pointer: where it is, and whether it has settled there.
//!
//! The screen half of a Split recording is a document, and until now it was
//! framed once and left alone — `ops::composite`'s own comment says why the
//! camera's answer was wrong for it: "it is a document, not a subject, and
//! following a face across a slide deck is nobody's idea of a feature."
//!
//! A pointer is not a face. It is not something that happens to be in the
//! frame; it is where the operator has *put* attention, and because
//! `SCStreamConfiguration::setShowsCursor` is on, it is drawn into the capture
//! for the viewer to follow too. So the thing this module tracks is the one
//! signal on a screencast worth aiming at.
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | [`PointerTracker`] — the motion state, and the cell it publishes to |
//! | [`sample`] | the sensor — the pointer's position, and whether it was clicked |
//!
//! What is deliberately *not* here: the smoothing, which is shared with face
//! tracking and lives in [`crate::track::smooth`], and the geometry, which is
//! [`crate::region::track`]. This module answers "where is the pointer and
//! what is it doing"; those two answer "so where should the frame be".
//!
//! ## Why the tracker takes an anchor and not a pointer
//!
//! [`PointerTracker::track`] is handed a position already normalized to the
//! operator's region — it never touches [`sample`] itself, and it has never
//! heard of a display. The conversion lives in `ops::mouse_track`, which is
//! rebuilt whenever the region or the display changes, so it always holds
//! current geometry.
//!
//! That split is what makes the feature survive a drag. `install_preview`
//! builds a **new** graph and drops the old one on every region release,
//! layout change and display switch, so anything owned by the op is destroyed
//! several times a session. The smoothing state cannot live there: it would
//! reset mid-take and the frame would snap to the region's centre and glide
//! back in, every time the operator nudged anything. It lives here instead,
//! behind an `Arc` the app owns — exactly the arrangement `face::FaceTracker`
//! uses, and for exactly the same reason.
//!
//! Normalizing to the region rather than to the buffer is the other half of
//! that: the region's pixel rect changes underneath a drag, but "three
//! quarters of the way across the box" survives it.

pub mod sample;

use std::sync::{Arc, Mutex};

use crate::config::MouseTracking;
use crate::region::framing::{Anchor, Track, TrackCell};
use crate::track::glide::Glide;
use crate::track::smooth::Smoother;

/// What one chapter's tracking did, for the record.
///
/// Printed when the switch goes off, which is deliberately not a sidecar:
/// `close_graph` runs only on the chapter's own av and screen graphs, so
/// nothing on the preview graph — `Composite`, `Cover`, `FaceTrack` — has ever
/// written one. Rather than add a fourth op that appears to report and does
/// not, these go to stdout, where they answer the one question the operator
/// cannot otherwise ask: did the tracker run at all?
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub frames: u64,
    /// Samples where the pointer was on the captured display.
    pub found: u64,
    /// Samples where it was not — another monitor, or no window server.
    pub lost: u64,
    pub rejected_as_noise: u64,
    pub rejected_as_jump: u64,
    /// How many times the operator punched in.
    pub punches: u64,
}

struct Inner {
    smoother: Smoother,
    /// How far punched in, eased rather than switched, so pressing the combo
    /// is a move and not a cut.
    punch: Glide,
    /// Where the frame is pinned while the combo is held.
    ///
    /// The whole reason the punch-in is usable: once you have punched in on
    /// something you almost always want to *point at parts of it*, and a frame
    /// that chased the pointer while you did would be unwatchable. So the
    /// press captures a position and the frame stays there; the pointer is
    /// then free to roam inside it.
    latched: Option<Anchor>,
    /// Previous frame's key state, so the latch can fire on the press edge
    /// rather than on every frame the key is down.
    was_held: bool,
    stats: Stats,
}

/// Smooths the pointer's position and publishes the framing that follows.
pub struct PointerTracker {
    inner: Mutex<Inner>,
    /// Outside the lock on purpose: a composite reading the framing must never
    /// block behind a sample, however briefly.
    track: Arc<TrackCell>,
}

impl PointerTracker {
    /// Ready immediately.
    ///
    /// Pointedly unlike `FaceTracker::spawn`, which needs a background thread
    /// and an Off→Loading→On state machine in `app::face` because it fetches a
    /// 34 MB library and compiles a model graph. There is nothing to load
    /// here, so the switch is a plain boolean and there is no loading state for
    /// the operator to see or for the app to track.
    pub fn new(config: &MouseTracking) -> PointerTracker {
        PointerTracker {
            inner: Mutex::new(Inner {
                smoother: Smoother::new(config.damping()),
                // Starts fully out: switching tracking on mid-session must not
                // punch in on its own.
                punch: Glide::new(0.0, config.punch_s),
                latched: None,
                was_held: false,
                stats: Stats::default(),
            }),
            track: Arc::new(TrackCell::new()),
        }
    }

    /// The cell the composites read. Cloned into each graph; the tracker
    /// outlives every graph, so the framing survives a rebuild.
    pub fn cell(&self) -> Arc<TrackCell> {
        Arc::clone(&self.track)
    }

    pub fn stats(&self) -> Stats {
        let inner = self.lock();
        let mut stats = inner.stats;
        stats.rejected_as_noise = inner.smoother.rejected_as_noise;
        stats.rejected_as_jump = inner.smoother.rejected_as_jump;
        stats
    }

    /// One frame. `anchor` is the pointer normalized to the tracked region, or
    /// `None` when it is not on the captured display at all.
    ///
    /// `None` holds the framing rather than re-centring it — reaching across
    /// to a second monitor is not a request to reframe the take. Whether it
    /// eventually eases back is `recenter_after_s`, which defaults to never.
    ///
    /// `held` is whether the punch-in combo is down this frame.
    ///
    /// Returns nothing and fails at nothing: this is called from an op on the
    /// capture queue, where a tracking failure must cost the framing and never
    /// the recording.
    pub fn track(&self, anchor: Option<Anchor>, held: bool, seconds: f64) {
        let mut inner = self.lock();
        inner.stats.frames += 1;
        match anchor {
            Some(anchor) => {
                inner.stats.found += 1;
                inner.smoother.observe(anchor, seconds);
            }
            None => {
                inner.stats.lost += 1;
                inner.smoother.lost(seconds);
            }
        }
        // Every frame, observed or not — the glide runs at the capture's rate
        // even when the pointer has not moved, which is what makes a settling
        // frame settle smoothly instead of in steps.
        let live = inner.smoother.advance(seconds);

        // Latch on the press *edge*. Doing it on every held frame would pin
        // the frame to wherever the pointer had drifted to, one frame at a
        // time, which is precisely the chasing the latch exists to prevent.
        if held && !inner.was_held {
            inner.latched = live;
            inner.stats.punches += 1;
        }
        inner.was_held = held;

        let punch = inner.punch.advance(if held { 1.0 } else { 0.0 }, seconds);

        // Let go of the latch only once the frame is all the way back out.
        //
        // Releasing it on the key-up instead would make the frame widen *and*
        // slide back toward the pointer at the same time — two motions where
        // the operator asked for one. Held until the punch is spent, the
        // widening is a straight zoom out from where it was, and at zero punch
        // the anchor stops mattering anyway.
        if !held && inner.punch.is_at(0.0) {
            inner.latched = None;
        }

        let aimed = inner.latched.or(live);
        drop(inner);

        self.track.set(aimed.map(|anchor| Track { anchor, punch }));
    }

    /// The recovering lock convention, for the reason [`TrackCell`] documents:
    /// the state behind it is a smoother and some counters, with no invariant
    /// a panicking writer could leave half-applied, and silently disabling
    /// tracking for the session is a worse outcome than carrying on.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for PointerTracker {
    /// Hand-written because `Smoother` is not `Debug` from behind the lock
    /// without taking it, and a `Debug` impl that can block is a trap in a
    /// log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PointerTracker")
            .field("track", &self.track.get())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
