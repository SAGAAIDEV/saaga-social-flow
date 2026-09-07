//! The graph node that watches the camera, and changes nothing about it.
//!
//! A pure analyzer, like [`Stats`](super::stats::Stats): it never calls
//! [`Frame::replace`], so the pixels leaving it are the pixels that arrived and
//! any op downstream is unaffected by whether this one is in the chain at all.
//! What it produces is a *reading* — the smoothed position of the subject —
//! published into the shared cell the composites consult when they decide where
//! to aim.
//!
//! ## Why it is a node and not a step inside the composites
//!
//! The preview graph forks: one camera source feeds a horizontal composite and
//! a vertical one, and both write files. Detecting inside each composite would
//! run the model twice per frame for one camera, and — worse — let the two
//! outputs disagree about where the subject is, so the same moment would be
//! framed differently in `horizontal.mp4` and `vertical.mp4`.
//!
//! As its own node upstream of the fork, it runs once, and the two composites
//! read one answer. Node order in a graph is topological by construction, so
//! the reading the composites see is the one this frame produced, not the
//! previous frame's.
//!
//! ## Why detection happens inline on the capture queue
//!
//! The op contract is explicit that `apply` runs on a capture queue that the
//! audio callback contends for and must not block. Detection is not free, so
//! this deserves a number rather than a shrug: measured on an M1 Max,
//! MediaPipe's BlazeFace costs **~1.5 ms** per call, and the downscaled
//! readback that feeds it costs a fraction of a millisecond more. At the
//! default cadence of one frame in three, that is under 2 ms every 100 ms
//! against a 33 ms frame budget the composites are already spending two GPU
//! renders out of.
//!
//! A detector thread would remove even that, at the cost of a channel, a
//! thread lifecycle and a `CVPixelBuffer` crossing a thread boundary. The
//! seam for it exists — [`FaceTracker`] owns the detector and the cell is
//! already shared — so it can be moved later without touching this file or the
//! composites. It has not been moved now because 2 ms is not a stall, and
//! machinery built against a cost nobody has measured is machinery built
//! against a guess.

use std::sync::Arc;

use anyhow::Result;

use super::graph::{Flow, StreamCtx};
use super::{Frame, Sidecar, VideoOp};
use crate::face::FaceTracker;

/// Publishes the subject's smoothed position; passes pixels through untouched.
pub struct FaceTrack {
    tracker: Arc<FaceTracker>,
}

impl FaceTrack {
    pub fn new(tracker: Arc<FaceTracker>) -> FaceTrack {
        FaceTrack { tracker }
    }
}

impl VideoOp for FaceTrack {
    fn name(&self) -> &'static str {
        "face-track"
    }

    /// Nothing to allocate.
    ///
    /// Pointedly so: the detector and its readback buffer were built when the
    /// tracker was, on a background thread, precisely because building them
    /// takes seconds on a cold machine — see [`crate::face`]. `open` runs on
    /// the main thread at a chapter boundary, which is the one moment in a
    /// recording that must not stall, so an op that allocated a model here
    /// would drop frames at every cut.
    fn open(&mut self, _ctx: &StreamCtx) -> Result<()> {
        Ok(())
    }

    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        self.tracker
            .track(frame.pixels(), crate::timesync::seconds(frame.pts()));
        // Never `Drop`, and never `replace`. A tracker that fails has cost the
        // framing its aim; it must not also cost the frame.
        Ok(Flow::Continue)
    }

    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        let stats = self.tracker.stats();
        sidecar.record(
            self.name(),
            serde_json::json!({
                "frames": stats.frames,
                "sampled": stats.sampled,
                "found": stats.found,
                "hit_rate": stats.hit_rate(),
                "rejected_as_noise": stats.rejected_as_noise,
                "rejected_as_jump": stats.rejected_as_jump,
                "sample_failures": stats.sample_failures,
                "final_anchor": self.tracker.anchor().map(|a| [a.0, a.1]),
            }),
        );
        Ok(())
    }
}
