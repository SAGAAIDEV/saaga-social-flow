//! Live face tracking: keeping the subject centred in the file as it records,
//! not in an edit afterwards.
//!
//! ```text
//! camera frame ─► sample ─► detect ─► smooth ─► AnchorCell ─► every composite
//!                (every N)  (MediaPipe)         (shared)      (their own slot)
//! ```
//!
//! Each stage has its own file because each fails for its own reasons:
//! [`sample`] is a pixel-format problem, [`detect`] is a model problem,
//! [`smooth`] is a motion-design problem, and mixing them is how a tremor in
//! the footage ends up being debugged in the colour conversion.
//!
//! ## Why this is worth doing live rather than offline
//!
//! There is already an offline answer — `screencast`'s edit stage runs the
//! sibling `face-track` binary over a finished chapter and writes
//! `face_track.json` for a compositor to consume. That is strictly more
//! accurate: it can see the whole take, smooth acausally, and cost whatever it
//! likes.
//!
//! What it cannot do is tell you, while you are still in the chair, that the
//! framing is wrong. The recorder composites Split · Horizontal down to a
//! 522-pixel column out of 1920 — 73% of the width discarded — and that column
//! is written to disk as `horizontal.mp4`. If your head is outside it, no
//! downstream stage can put it back. Tracking live means the preview shows the
//! crop that is being recorded, aimed where you actually are, and the file is
//! right the first time.
//!
//! ## One tracker, one detection, every output
//!
//! The tracker is **session-scoped**, not per chapter and not per output. Two
//! things follow, and both are the point:
//!
//! - The camera is detected on **once** per sampled frame, and the horizontal
//!   and vertical composites read the same answer. Putting detection inside
//!   each composite would double the cost and let the two outputs disagree
//!   about where the subject is.
//! - Smoothing state survives a layout change. Switching pair mid-session
//!   rebuilds the preview graph, and a per-graph smoother would restart from
//!   nothing and swoop in from centre on the next take.
//!
//! ## The one-time download, and why it is forced to happen here
//!
//! The `mediapipe` crate `dlopen`s a ~34 MB `libmediapipe.dylib` and fetches it
//! from Google's own PyPI wheel on first use. Building a detector therefore
//! takes seconds the first time and a few hundred milliseconds after that —
//! which is fine, and would be catastrophic on the capture queue or at a
//! chapter cut, where the recorder's whole job is to not stall.
//!
//! So [`FaceTracker::spawn`] does it on a background thread and reports back
//! when the tracker is ready. Nothing on a capture queue ever constructs a
//! detector; the op is handed one that already exists, and until it does, the
//! composites frame exactly as they did before this module was written.

pub mod detect;
pub mod sample;

use std::sync::{Arc, Mutex};

use objc2_core_video::CVImageBuffer;

use crate::config::FaceTracking;
use crate::ops::render::Renderer;
use crate::region::framing::{Anchor, AnchorCell};

use crate::track::smooth::Smoother;
use detect::Detector;
use sample::Sampler;

/// What one chapter's worth of tracking did. Reported so a take that framed
/// badly can be diagnosed from numbers rather than from re-watching it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Frames the op saw, sampled or not.
    pub frames: u64,
    /// Frames a detection actually ran on.
    pub sampled: u64,
    /// Of those, how many found a face.
    pub found: u64,
    /// Detections discarded by the deadband as detector tremor.
    pub rejected_as_noise: u64,
    /// Detections discarded as unconfirmed jumps.
    pub rejected_as_jump: u64,
    /// Sampled frames the pixel readback could not produce bytes for.
    pub sample_failures: u64,
}

impl Stats {
    /// Share of sampled frames that found a face. `None` before anything has
    /// been sampled, which is not the same as zero.
    pub fn hit_rate(&self) -> Option<f64> {
        (self.sampled > 0).then(|| self.found as f64 / self.sampled as f64)
    }
}

/// The session's face tracker: a detector, the smoothing that tames it, and the
/// cell every composite reads.
pub struct FaceTracker {
    /// Everything that needs `&mut` per frame, behind one lock.
    ///
    /// One lock rather than three because they are only ever taken together, on
    /// one queue, in one call — and a single uncontended lock is cheaper to
    /// reason about than three that must always be taken in the same order.
    inner: Mutex<Inner>,
    /// Read by the composites, written by [`FaceTracker::track`]. Separate from
    /// `inner` on purpose: a reader must never be able to block behind a
    /// detection, and this is the only state a reader needs.
    anchor: Arc<AnchorCell>,
    /// How often to detect. `1` is every frame.
    detect_every: u32,
}

struct Inner {
    detector: Detector,
    sampler: Sampler,
    smoother: Smoother,
    stats: Stats,
}

impl FaceTracker {
    /// Build a tracker on a background thread and hand it to `ready`.
    ///
    /// Returns immediately. `ready` is called on the worker thread with the
    /// finished tracker, or with the error that stopped it — most likely the
    /// one-time `libmediapipe` download failing on a machine with no network.
    ///
    /// A failure here must never be fatal: face tracking is an option, and a
    /// recorder that will not start because a model could not be fetched has
    /// traded a feature for the whole job.
    pub fn spawn(
        config: FaceTracking,
        renderer: Renderer,
        ready: impl FnOnce(anyhow::Result<Arc<FaceTracker>>) + Send + 'static,
    ) {
        std::thread::Builder::new()
            .name("stream-recorder.face-track-build".into())
            .spawn(move || ready(FaceTracker::build(config, renderer).map(Arc::new)))
            .map(|_| ())
            .unwrap_or_else(|e| {
                eprintln!("stream-recorder: could not start the face-tracking build thread: {e}")
            });
    }

    /// Build one synchronously. Seconds on first use — see the module docs.
    pub fn build(config: FaceTracking, renderer: Renderer) -> anyhow::Result<FaceTracker> {
        let detector = Detector::new(&config)?;
        let sampler = Sampler::new(renderer, config.detect_width.max(64));
        Ok(FaceTracker {
            inner: Mutex::new(Inner {
                detector,
                sampler,
                smoother: Smoother::new(config.damping()),
                stats: Stats::default(),
            }),
            anchor: Arc::new(AnchorCell::new()),
            detect_every: config.detect_every.max(1),
        })
    }

    /// The cell the composites read. Cloning it is an `Arc` bump.
    pub fn anchor_cell(&self) -> Arc<AnchorCell> {
        Arc::clone(&self.anchor)
    }

    /// The latest smoothed anchor, for the UI's readout.
    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor.get()
    }

    pub fn stats(&self) -> Stats {
        self.inner
            .lock()
            .map(|inner| {
                let mut stats = inner.stats;
                stats.rejected_as_noise = inner.smoother.rejected_as_noise;
                stats.rejected_as_jump = inner.smoother.rejected_as_jump;
                stats
            })
            .unwrap_or_default()
    }

    /// One camera frame, on the capture queue.
    ///
    /// Detects on every `detect_every`-th frame and advances the smoothing on
    /// all of them, then publishes. Errors are swallowed into counters rather
    /// than returned: the caller is an op inside a capture callback, and a
    /// tracking failure must cost the framing, never the recording.
    ///
    /// A poisoned lock means a previous call panicked mid-detection. The frame
    /// is passed over and the last published anchor stands, which degrades to
    /// "the framing stopped following" rather than to a dead capture queue.
    pub fn track(&self, pixels: &CVImageBuffer, seconds: f64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let inner = &mut *inner;

        inner.stats.frames += 1;
        if inner.stats.frames % u64::from(self.detect_every) == 0 {
            inner.stats.sampled += 1;
            match inner.sampler.rgba(pixels) {
                Some(frame) => match inner.detector.anchor(frame, seconds) {
                    Some(point) => {
                        inner.stats.found += 1;
                        inner.smoother.observe(point, seconds);
                    }
                    None => inner.smoother.lost(seconds),
                },
                None => {
                    inner.stats.sample_failures += 1;
                    inner.smoother.lost(seconds);
                }
            }
        }

        // Every frame, sampled or not — this is what keeps a 10 Hz detection
        // cadence from looking like 10 Hz motion.
        let anchor = inner.smoother.advance(seconds);
        self.anchor.set(anchor);
    }
}

impl std::fmt::Debug for FaceTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaceTracker")
            .field("anchor", &self.anchor.get())
            .field("detect_every", &self.detect_every)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::render::Pool;

    /// The whole live path, minus the camera: a real frame through a real
    /// tracker, into the cell, out as a framing offset.
    ///
    /// `#[ignore]`d for the same reasons as `detect`'s end-to-end test — it
    /// needs a portrait and, on a cold machine, a library download.
    ///
    /// ```sh
    /// curl -o /tmp/portrait.jpg https://storage.googleapis.com/mediapipe-assets/portrait.jpg
    /// FACE_TEST_IMAGE=/tmp/portrait.jpg cargo test --bin stream-recorder \
    ///     face::tests -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs $FACE_TEST_IMAGE and, on a cold machine, a libmediapipe download"]
    fn tracking_a_real_frame_moves_the_framing_off_centre() {
        let Ok(path) = std::env::var("FACE_TEST_IMAGE") else {
            panic!("set FACE_TEST_IMAGE to a portrait photograph");
        };
        let Ok(renderer) = Renderer::new() else {
            println!("skipping: no Metal device on this machine");
            return;
        };

        let url =
            objc2_foundation::NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(&path));
        let image = unsafe { objc2_core_image::CIImage::imageWithContentsOfURL(&url) }
            .expect("Core Image could not decode the image");
        let extent = unsafe { image.extent() };
        let (w, h) = (
            extent.size.width.round() as usize,
            extent.size.height.round() as usize,
        );
        let pool = Pool::create(w, h).expect("pool");
        let buffer = pool.take().expect("buffer");
        renderer.render(&image, &buffer);

        let config = crate::config::FaceTracking {
            // Every frame, so a short run is a fair test of the cadence-free
            // parts rather than of the cadence.
            detect_every: 1,
            ..Default::default()
        };
        let tracker = FaceTracker::build(config.clone(), renderer).expect("tracker");
        let cell = tracker.anchor_cell();
        assert_eq!(
            cell.get(),
            None,
            "nothing is published before the first frame"
        );

        // Two seconds at 30fps of the same frame: long enough for the glide to
        // settle, though the first detection snaps anyway.
        for frame in 0..60 {
            tracker.track(&buffer, f64::from(frame) / 30.0);
        }

        let stats = tracker.stats();
        println!("{stats:?} anchor={:?}", tracker.anchor());
        assert_eq!(stats.frames, 60);
        assert_eq!(stats.sampled, 60, "detect_every 1 samples every frame");
        assert!(
            stats.hit_rate().unwrap_or(0.0) > 0.9,
            "a still portrait should be found nearly every frame: {stats:?}"
        );
        assert_eq!(stats.sample_failures, 0);
        // The still frame is the case the deadband exists for.
        assert!(
            stats.rejected_as_noise > 50,
            "a frozen frame should be almost entirely deadbanded: {stats:?}"
        );

        let anchor = cell.get().expect("an anchor was published");
        assert!(
            anchor.1 < 0.5,
            "the portrait's face is in the upper half: {anchor:?}"
        );

        // And that anchor reaches a framing as a real offset. Split ·
        // Horizontal, whose 522-column camera slot is the case tracking matters
        // most for.
        let framing = crate::region::framing::Framing::tracked(
            config.zoom_for("screen-camera-split"),
            Some(tracker.anchor_cell()),
        );
        let camera = (1920.0, 1080.0);
        let offset = framing.offset_into(camera, (522.0, 1080.0));
        println!("split-horizontal offset {offset:?}");
        assert!(
            (0.0..=1.0).contains(&offset.0),
            "the offset must stay on the frame: {offset:?}"
        );
        // The full-bleed longform, which only moves because of the punch-in.
        let longform = crate::region::framing::Framing::tracked(
            config.zoom_for("talking-head-horizontal"),
            Some(tracker.anchor_cell()),
        );
        let offset = longform.offset_into(camera, (1920.0, 1080.0));
        println!("talking-head-horizontal offset {offset:?}");
        assert!(
            offset.1 < 0.5,
            "the punch-in should have slid up toward the face: {offset:?}"
        );
    }
}
