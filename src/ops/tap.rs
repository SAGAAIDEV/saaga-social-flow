//! The cross-stream edge: a shared latest-frame cell.
//!
//! Camera frames arrive on `stream-recorder.av-video` and screen frames on the
//! `stream-recorder.screen` serial queue, timestamped against two different
//! clocks, and ScreenCaptureKit is change-driven — a static screen delivers no
//! frames at all, which is why `ScreenDelegate::seed_chapter` exists. A join
//! node that blocked waiting for both of its inputs would stall capture on
//! whichever queue got there first, so the join cannot be synchronous and the
//! design says so rather than pretending otherwise: one side publishes its
//! newest frame here, the other reads it when its own frame arrives.
//!
//! Pairing error is then bounded by one frame interval plus clock drift, which
//! `Router::anchors` already documents as ~2 ms over a ten-minute chapter.
//!
//! Nothing publishes in stage 1. `Node::Tap` is built by the builder and
//! skipped by `Graph::run`, so wiring the join in stage 4 is one line rather
//! than a new type.

use std::sync::{Arc, Mutex};

use objc2_core_foundation::CFRetained;
use objc2_core_media::CMTime;
use objc2_core_video::CVImageBuffer;

use super::frame::{Frame, StreamId};

/// A pixel buffer handle that may be moved to, and read from, another capture
/// queue.
///
/// This is the **one** place in the design where a CoreVideo handle genuinely
/// crosses a thread boundary — the screen queue publishes, the camera queue
/// reads — so it is the one place that carries an `unsafe impl` instead of the
/// `VideoOp` trait carrying a blanket `Send` bound. The justification:
///
/// - `objc2-core-video` 0.3.2 emits `Send`/`Sync` for no CoreFoundation object
///   type at all. `cf_type!` generates `Type`/`Deref`/`Eq`/`Hash`/`Debug` and
///   nothing else, so the bound is absent *by default*, not by any analysis of
///   this type. (Contrast `objc2-core-media`, which does mark `CMClock` and
///   friends `Send` — the omission here is the macro's, not a finding.)
/// - `CFRetain`/`CFRelease` are thread safe, which is precisely the reasoning
///   `objc2-core-foundation` itself uses for
///   `CFRetained<T>: Send where T: Send + Sync`.
/// - The published buffer is a capture buffer that nothing writes to after
///   publication. This type hands out `&CVImageBuffer` only, and this crate
///   never locks a tapped buffer for write.
///
/// **The justification does not extend to a buffer being rendered into.** Two
/// threads touching one pixel buffer while it is locked for write is a genuine
/// data race, not a missing marker trait, and stage 2's pool buffers must never
/// travel this way.
#[derive(Clone)]
#[allow(dead_code)] // nothing reads a tap until stage 4.
pub struct SharedPixels(CFRetained<CVImageBuffer>);

// SAFETY: see the type's docs — the handle is reference-counted with thread
// safe retain/release, the buffer behind it is read-only after publication, and
// only `&CVImageBuffer` is ever handed out.
unsafe impl Send for SharedPixels {}
// SAFETY: as above; `&SharedPixels` grants read-only access to an immutable
// capture buffer.
unsafe impl Sync for SharedPixels {}

#[allow(dead_code)] // ditto.
impl SharedPixels {
    pub(crate) fn from_retained(pixels: CFRetained<CVImageBuffer>) -> SharedPixels {
        SharedPixels(pixels)
    }

    pub fn get(&self) -> &CVImageBuffer {
        &self.0
    }
}

/// The most recent frame one stream published, as another stream sees it.
#[derive(Clone)]
#[allow(dead_code)] // ditto.
pub struct TapFrame {
    pub pixels: SharedPixels,
    /// On the host clock, like every [`Frame`] PTS — which is what makes it
    /// comparable with the reading stream's own time at all.
    pub pts: CMTime,
    pub stream: StreamId,
}

/// A latest-frame cell shared between two streams' graphs.
#[allow(dead_code)] // built by the builder; wired up in stage 4.
pub struct Tap {
    latest: Mutex<Option<TapFrame>>,
}

#[allow(dead_code)] // ditto.
impl Tap {
    pub fn new() -> Arc<Tap> {
        Arc::new(Tap {
            latest: Mutex::new(None),
        })
    }

    /// Publish this frame as the newest one on this tap. Cheap by design: a
    /// retain and a store, on the publishing stream's own queue.
    ///
    /// Poison-tolerant, deliberately. `publish` runs inside an Objective-C
    /// callback, and a panic elsewhere that poisoned this lock must not turn
    /// the tap into a second panic site on a capture queue — `Graph::run`
    /// contains its own unwinds precisely so no lock on this path is ever left
    /// poisoned, and this is the belt to that pair of braces.
    pub fn publish(&self, frame: &Frame) {
        self.publish_buffer(frame.pixels_retained(), frame.pts(), frame.stream());
    }

    /// Publish a finished capture buffer. Used by the screen delegate so a
    /// preview composite can read the screen without a chapter graph.
    pub fn publish_buffer(
        &self,
        pixels: CFRetained<CVImageBuffer>,
        pts: CMTime,
        stream: StreamId,
    ) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        *latest = Some(TapFrame {
            pixels: SharedPixels::from_retained(pixels),
            pts,
            stream,
        });
    }

    /// The newest published frame, or `None` if nothing has published yet —
    /// which is the normal state for the first frames of a chapter, and the
    /// permanent state of a chapter recorded against a still screen.
    pub fn latest(&self) -> Option<TapFrame> {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_reads_none_before_the_first_publish() {
        let tap = Tap::new();
        assert!(
            tap.latest().is_none(),
            "an unpublished tap must not hand out a frame"
        );
    }
}
