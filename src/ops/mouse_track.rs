//! The graph node that watches the pointer, and changes nothing about the
//! frame it is handed.
//!
//! A pure analyzer, like [`FaceTrack`](super::face_track::FaceTrack) and
//! [`Stats`](super::stats::Stats): it never calls [`Frame::replace`], so the
//! pixels leaving it are the pixels that arrived, and whether it is in the
//! chain at all is invisible to every op downstream. What it produces is a
//! *reading* — where the pointer is inside the operator's region — published
//! into the cell the vertical composite consults when it decides what to crop.
//!
//! ## Why it is a node at all, when it does not touch pixels
//!
//! Because the reading has to be ordered against the frame that uses it. Graph
//! nodes run in topological order by construction, so a node sitting above the
//! horizontal/vertical fork has published this frame's answer before either
//! composite reads it. Sampling from the app's 60 Hz tick instead would put
//! the pointer on a different clock from the frames, and the crop would
//! sometimes use a reading from the future and sometimes one from the past.
//!
//! It is also where the *coordinates* live, and they have to be here rather
//! than in `pointer::PointerTracker` for a lifecycle reason. This op is
//! rebuilt by `install_preview` on every region drag, layout change and
//! display switch, which is exactly when the geometry it holds goes stale —
//! so holding it here means it is never stale. The tracker holds the
//! *smoothing*, which must survive those same rebuilds, and so cannot be here.
//!
//! ## What it costs
//!
//! One pointer read per frame: **131 ns**, measured off the main thread on an
//! M1 Max (`pointer::sample`). `FaceTrack` next door spends ~1.5 ms every
//! third frame and documents why that is affordable against a 33 ms budget;
//! this is four ten-thousandths of that, which is why it samples every frame
//! and has no cadence to configure.

use std::sync::Arc;

use anyhow::Result;

use super::graph::{Flow, StreamCtx};
use super::{Frame, VideoOp};
use crate::pointer::{sample, PointerTracker};
use crate::region::framing::Anchor;
use crate::region::{DisplayGeometry, PointRect};

/// Publishes where the pointer is inside `rest`; passes pixels through
/// untouched.
pub struct MouseTrack {
    tracker: Arc<PointerTracker>,
    /// The display the region is on, for turning a Core Graphics global point
    /// into a display-local one.
    geometry: DisplayGeometry,
    /// What the screen stream is capturing, in display-local points — the
    /// frame the published anchor is normalized against. The union of both
    /// authored regions on Split, so one reading serves both crops.
    capture: PointRect,
}

impl MouseTrack {
    pub fn new(
        tracker: Arc<PointerTracker>,
        geometry: DisplayGeometry,
        capture: PointRect,
    ) -> MouseTrack {
        MouseTrack {
            tracker,
            geometry,
            capture,
        }
    }

    /// The pointer as a fraction of the captured screen.
    ///
    /// Deliberately **not** clamped to `0..1`. A pointer outside the capture
    /// is the normal case — the capture is a crop of a much larger display —
    /// and the honest reading is "off to the left", which each composite then
    /// turns into a frame pushed as far left as it may go. Clamping here would
    /// make a pointer one inch outside and one metre outside produce the same
    /// anchor, and the deadband would then treat a real move as noise.
    fn anchor(&self) -> Option<Anchor> {
        if !(self.capture.w > 0.0 && self.capture.h > 0.0) {
            return None;
        }
        let (x, y) = sample::on_display(&self.geometry)?;
        Some((
            (x - self.capture.x) / self.capture.w,
            (y - self.capture.y) / self.capture.h,
        ))
    }
}

impl VideoOp for MouseTrack {
    fn name(&self) -> &'static str {
        "mouse-track"
    }

    /// Nothing to allocate — and nothing that *could* be, which is the
    /// difference between this and every op that renders. `open` runs on the
    /// main thread at a chapter boundary, the one moment in a recording that
    /// must not stall.
    fn open(&mut self, _ctx: &StreamCtx) -> Result<()> {
        Ok(())
    }

    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        self.tracker.track(
            self.anchor(),
            sample::zoom_held(),
            crate::timesync::seconds(frame.pts()),
        );
        // Never `Err`, never `Drop`, never `replace`.
        //
        // The `Err` is the one that would really hurt: `Graph::bypass` latches
        // on the first error and the preview graph is *session*-scoped, not
        // chapter-scoped, so one bad frame here would stop both composed
        // recordings for the rest of the session — and the message it prints
        // would say "for the rest of this chapter", which for this graph is
        // not true. A tracking failure costs the framing. It does not get to
        // cost the take.
        Ok(Flow::Continue)
    }

    // No `close`. Its sidecar would never be written: `close_graph` runs only
    // on the chapter's own av and screen graphs, while the preview graph that
    // owns this node is replaced by `set_preview` and dropped without being
    // closed. `Composite::close` and `FaceTrack::close` are already in that
    // position and have never produced a file. Rather than add a third,
    // `PointerTracker::stats` is readable from the app whenever the lifecycle
    // is worth fixing properly.
}
