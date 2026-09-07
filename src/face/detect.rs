//! MediaPipe, and the one number this recorder wants out of it.
//!
//! The model is BlazeFace short-range, vendored at `models/` and
//! `include_bytes!`d in — see that directory's README for why it is checked in
//! rather than fetched. It reports, per face, a pixel bounding box and six
//! normalized keypoints: right eye, left eye, nose tip, mouth centre, and the
//! two ear tragions, in that order.
//!
//! ## Video mode, not image mode
//!
//! `build_for_video` rather than `build`, which costs a strictly increasing
//! timestamp per call and buys MediaPipe's own frame-to-frame association: the
//! task keeps the previous frame's boxes and matches this frame's against them.
//! It is the cheapest smoothing available — it happens inside a model that is
//! already running — and it composes with, rather than replaces, the damping in
//! [`super::smooth`], which handles the tremor association cannot.
//!
//! Timestamps must increase or MediaPipe rejects the call outright, and the
//! host clock is monotonic but the *sampled* frames are not guaranteed to be:
//! two frames can share a millisecond after rounding. So the ratchet below
//! forces a strictly greater value rather than trusting the input, because the
//! alternative is a detector that silently stops answering after one collision.
//!
//! ## Why the anchor is not the middle of the box
//!
//! Framing a head on the centre of its bounding box puts the eyes halfway down
//! the frame, which reads as a security camera. Every framing convention worth
//! copying puts the eyes high — around a third from the top — with the space
//! below carrying the shoulders.
//!
//! So the anchor is built from the eye keypoints, then pushed *down* by a
//! fraction of the face's own height. Aiming the crop below the eyes is what
//! lifts the eyes above centre in the result, and expressing the push in face
//! heights rather than frame heights makes it hold as you lean toward the lens
//! and the face grows.

use anyhow::{Context, Result};
use mediapipe::{Confidence, FaceDetector, Image, ModelSource, Size, Timestamp};

use crate::config::FaceTracking;
use crate::region::framing::Anchor;

use super::sample::Rgba;

/// BlazeFace short-range, baked into the binary. 224 KB.
const MODEL: &[u8] = include_bytes!("../../models/blaze_face_short_range.tflite");

/// Keypoint indices, in the order MediaPipe's face detector emits them.
const RIGHT_EYE: usize = 0;
const LEFT_EYE: usize = 1;

pub struct Detector {
    detector: FaceDetector,
    /// How far below the eyes to aim, in multiples of the face box's height.
    headroom: f64,
    /// Detections below this score are not faces as far as framing is
    /// concerned. Separate from the model's own threshold because this one is
    /// about whether to *move the camera*, which deserves to be stricter than
    /// whether to report a box.
    min_confidence: f64,
    /// The timestamp ratchet. Milliseconds, monotonically increasing.
    last_ms: i64,
}

impl Detector {
    pub fn new(config: &FaceTracking) -> Result<Detector> {
        let confidence = Confidence::new(config.min_confidence.clamp(0.0, 1.0))
            .context("the configured face-tracking confidence is not in 0..1")?;
        let detector = FaceDetector::builder(ModelSource::bytes(MODEL))
            .min_detection_confidence(confidence)
            .build_for_video()
            .context(
                "building the MediaPipe face detector — if this is the first run it was \
                 also fetching libmediapipe (~34 MB) into ~/.cache/mediapipe-rs/, which \
                 needs a network connection",
            )?;
        Ok(Detector {
            detector,
            headroom: config.headroom,
            min_confidence: f64::from(config.min_confidence),
            last_ms: 0,
        })
    }

    /// Where to aim for this frame, or `None` if no face worth following is in
    /// it.
    ///
    /// `seconds` is the frame's host-clock time, used only to keep MediaPipe's
    /// video-mode timestamps increasing.
    pub fn anchor(&mut self, frame: Rgba<'_>, seconds: f64) -> Option<Anchor> {
        let size = Size {
            width: frame.width,
            height: frame.height,
        };
        let image = Image::from_rgba(size, frame.bytes).ok()?;

        let ms = (seconds * 1000.0) as i64;
        self.last_ms = ms.max(self.last_ms + 1);
        let faces = self
            .detector
            .detect_for_video(&image, Timestamp::from_millis(self.last_ms))
            .ok()?;

        // The biggest face, not the most confident one. With two people in
        // frame the presenter is the one nearest the lens, and "nearest" is
        // what box area measures; picking on score would hand the framing to
        // whoever the model happened to like better this millisecond, and swap
        // between them.
        let face = faces
            .into_iter()
            .filter(|face| {
                face.score()
                    .map(|score| f64::from(score.get()) >= self.min_confidence)
                    .unwrap_or(false)
            })
            .max_by(|a, b| area(a).total_cmp(&area(b)))?;

        let box_ = &face.bounding_box;
        let width = f64::from(frame.width);
        let height = f64::from(frame.height);
        if !(width > 0.0 && height > 0.0) {
            return None;
        }

        let centre_x = (f64::from(box_.left()) + f64::from(box_.right())) / 2.0 / width;
        let box_height = (f64::from(box_.bottom()) - f64::from(box_.top())) / height;

        // Eyes if the model gave them, box centre otherwise. Both eyes or
        // neither: one eye means a profile the box already describes better.
        let eye_y = match (face.keypoints.get(RIGHT_EYE), face.keypoints.get(LEFT_EYE)) {
            (Some(right), Some(left)) => {
                f64::from(right.point.y() + left.point.y()) / 2.0
            }
            _ => (f64::from(box_.top()) + f64::from(box_.bottom())) / 2.0 / height,
        };

        // No y correction. `render:toBitmap:` keeps the capture's row order
        // despite Core Image's bottom-left origin, so the detection is already
        // in the top-left space every rect in this recorder uses — see
        // `super::sample`'s `the_rendered_bitmap_keeps_the_captures_row_order`,
        // which is the measurement rather than the assumption.
        Some((
            centre_x,
            // Down from the eyes, so the eyes ride up in the finished frame.
            eye_y + box_height * self.headroom,
        ))
    }
}

fn area(face: &mediapipe::Detection) -> f64 {
    let b = &face.bounding_box;
    f64::from(b.right() - b.left()).max(0.0) * f64::from(b.bottom() - b.top()).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face::sample::Sampler;
    use crate::ops::render::{Pool, Renderer};

    /// The whole chain on a real photograph: file → capture-shaped pixel buffer
    /// → `Sampler` → `Detector` → anchor.
    ///
    /// `#[ignore]`d because it needs two things no other test in this crate
    /// does: a portrait to look at, and — on a machine that has never run face
    /// tracking — a ~34 MB `libmediapipe` download. Neither belongs on every
    /// `cargo test`. It is here because it is the only test that exercises the
    /// coordinate agreement between `sample` and `detect` end to end, which is
    /// the seam most likely to be silently mirrored.
    ///
    /// ```sh
    /// curl -o /tmp/portrait.jpg https://storage.googleapis.com/mediapipe-assets/portrait.jpg
    /// FACE_TEST_IMAGE=/tmp/portrait.jpg cargo test --bin stream-recorder \
    ///     face::detect -- --ignored --nocapture
    /// ```
    ///
    /// The expectation is loose on purpose. What is being checked is not the
    /// model's precision — that is Google's problem — but that the answer is
    /// the right way up and the right way round: a face in the upper-middle of
    /// the frame must produce an anchor in the upper-middle of the frame.
    #[test]
    #[ignore = "needs $FACE_TEST_IMAGE and, on a cold machine, a libmediapipe download"]
    fn a_real_portrait_anchors_where_the_face_actually_is() {
        let Ok(path) = std::env::var("FACE_TEST_IMAGE") else {
            panic!("set FACE_TEST_IMAGE to a portrait photograph");
        };
        let Ok(renderer) = Renderer::new() else {
            println!("skipping: no Metal device on this machine");
            return;
        };

        // Decode through Core Image and land it in a capture-shaped BGRA
        // buffer, so the detector is fed by exactly the path a camera frame
        // takes rather than by a shortcut that could hide a flip.
        let url = objc2_foundation::NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(
            &path,
        ));
        let image = unsafe { objc2_core_image::CIImage::imageWithContentsOfURL(&url) }
            .expect("Core Image could not decode the image");
        let extent = unsafe { image.extent() };
        let (w, h) = (
            extent.size.width.round() as usize,
            extent.size.height.round() as usize,
        );
        println!("source image {w}x{h}");

        let pool = Pool::create(w, h).expect("pool");
        let buffer = pool.take().expect("buffer");
        renderer.render(&image, &buffer);

        let mut sampler = Sampler::new(renderer, 320);
        let frame = sampler.rgba(&buffer).expect("sampled");
        println!("detector input {}x{}", frame.width, frame.height);

        let config = crate::config::FaceTracking::default();
        let mut detector = Detector::new(&config).expect("detector");
        let anchor = detector.anchor(frame, 1.0).expect("a face in the portrait");
        println!("anchor {anchor:?}");

        assert!(
            (0.25..0.75).contains(&anchor.0),
            "the face is roughly centred horizontally, got {anchor:?}"
        );
        assert!(
            anchor.1 < 0.5,
            "the face sits in the upper half; an anchor below centre means the \
             frame reached the detector upside down: {anchor:?}"
        );
        assert!(
            anchor.1 > 0.05,
            "an anchor jammed against the top edge means the headroom push went \
             the wrong way: {anchor:?}"
        );
    }
}
