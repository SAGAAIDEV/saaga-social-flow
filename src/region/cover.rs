//! `object-fit: cover` and `object-position`, restated once.
//!
//! Every layout drops its camera into a fixed hole with `object-fit: cover` and
//! places it with `object-position` — see any of the compositions under
//! `screencast/components/compositions/`. The recorder has to answer the same
//! question ("which part of the camera does that hole actually show?") to
//! composite a preview that means anything, so the arithmetic is reproduced
//! here rather than approximated.
//!
//! **Reproduced, not reinvented.** The point is that the preview and the render
//! agree exactly: an offset shown here is the same number
//! `screencast/src/screencast/workers/chapterdoc.py` writes as that chapter's
//! `cameraPosition`, and a preview that used its own framing rule would be a
//! confident picture of something that will not happen.
//!
//! The rule, in three lines:
//!
//! ```text
//! scale   = max(slot.w / src.w, slot.h / src.h)   // cover: fill, never letterbox
//! visible = (slot.w / scale, slot.h / scale)      // of the source, in source pixels
//! origin  = offset * (src - visible)              // object-position, offset in 0..1
//! ```
//!
//! `cover` scales by the *larger* ratio, which is what guarantees the slot is
//! filled and what makes the overflow — the part `object-position` slides
//! around — appear on at most one axis.

/// What a slot shows of a source image under `object-fit: cover`.
// Nothing renders a composite yet, so nothing calls this outside its own tests
// — the same shape as `ops::tap`, which was written a stage before the join
// that reads it. The arithmetic is the part worth settling first: it is fully
// provable without hardware, and the composite is where it stops being
// provable.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cover {
    /// The visible window's size, in **source** pixels.
    pub visible: (f64, f64),
    /// That window's top-left corner in the source, placed by the offset.
    pub origin: (f64, f64),
    /// The uniform scale from source pixels to slot pixels. Above 1.0 the
    /// source is being blown up to fill the slot, which is worth surfacing:
    /// it means the camera is lower resolution than the hole it is filling.
    pub scale: f64,
}

#[allow(dead_code)]
impl Cover {
    /// How much of the source is cropped away, per axis, in source pixels.
    ///
    /// At most one of these is nonzero — `cover` scales by a single factor, so
    /// the axis that determined it fits exactly and only the other overflows.
    pub fn overflow(&self, src: (f64, f64)) -> (f64, f64) {
        (
            (src.0 - self.visible.0).max(0.0),
            (src.1 - self.visible.1).max(0.0),
        )
    }
}

/// Where `offset` puts the slot's view of a `src`-sized image.
///
/// `offset` is normalized per axis: `0.0` is flush against the left/top edge of
/// the source, `1.0` flush right/bottom, `0.5` centred — exactly CSS's
/// percentage form of `object-position`. Out-of-range values are clamped rather
/// than rejected, because a saved offset outliving a change of camera
/// resolution should slide to the edge, not refuse to render.
#[allow(dead_code)]
pub fn cover(src: (f64, f64), slot: (f64, f64), offset: (f64, f64)) -> Cover {
    cover_zoom(src, slot, offset, 1.0)
}

/// [`cover`], punched in by `zoom`.
///
/// `zoom` multiplies the scale, so the visible window shrinks by the same
/// factor on *both* axes and the source is blown back up to fill the slot. The
/// point is not magnification: it is that a window smaller than the source has
/// somewhere to slide.
///
/// That distinction is the whole reason this parameter exists. Talking Head ·
/// Horizontal drops a 16:9 camera into a 16:9 full-bleed slot, so `cover` at
/// zoom 1.0 keeps every pixel, the overflow is zero on both axes, and
/// `offset` is arithmetically inert — face tracking there can compute a
/// perfect target and move nothing. A zoom above 1.0 is what buys the margin
/// back, at the cost of rendering the slot from fewer source pixels: at 1.14 a
/// 1920-wide camera fills 1920 from about 1685 columns.
///
/// Values below 1.0 are clamped away rather than honoured. Zooming *out* would
/// ask for a window larger than the source, which `cover` cannot express — the
/// slot would have to letterbox, and a cover that letterboxes is not a cover.
#[allow(dead_code)]
pub fn cover_zoom(src: (f64, f64), slot: (f64, f64), offset: (f64, f64), zoom: f64) -> Cover {
    // A zero-sized source is what a camera reports before its first frame, and
    // the caller cannot always know that has not happened yet. Degrade to a
    // no-op cover rather than dividing by zero and propagating NaN into a
    // render.
    if !(src.0 > 0.0 && src.1 > 0.0 && slot.0 > 0.0 && slot.1 > 0.0) {
        return Cover {
            visible: (src.0.max(0.0), src.1.max(0.0)),
            origin: (0.0, 0.0),
            scale: 1.0,
        };
    }

    let scale = (slot.0 / src.0).max(slot.1 / src.1) * clamp_zoom(zoom);
    let visible = (slot.0 / scale, slot.1 / scale);
    let overflow = ((src.0 - visible.0).max(0.0), (src.1 - visible.1).max(0.0));
    Cover {
        visible,
        origin: (
            clamp01(offset.0) * overflow.0,
            clamp01(offset.1) * overflow.1,
        ),
        scale,
    }
}

/// The offset that puts `point` at the centre of the slot's view — the inverse
/// of [`cover_zoom`].
///
/// `point` is normalized against the *source*, the same space
/// [`Cover::origin`] divided by `src` would give: `(0.5, 0.5)` is the middle of
/// the camera frame. The answer is a normalized offset, ready to hand straight
/// back to [`cover_zoom`], and it is what turns a detected face position into a
/// framing decision.
///
/// Two properties are worth stating because both are load-bearing and neither
/// is obvious:
///
/// - **It clamps rather than fails.** A face near the edge asks for a window
///   that would hang off the source; the clamp slides it flush instead, which
///   is the same thing a human operator does. So a tracked face stops moving
///   before it reaches the frame edge, and that is correct — the alternative is
///   black bars.
/// - **An axis with no overflow answers `0.5`.** Not because the face is
///   centred but because nothing on that axis can move, and 0.5 is the only
///   value `cover` treats identically to every other. This is what makes
///   tracking a no-op at zoom 1.0 in a full-bleed slot rather than a source of
///   NaN.
#[allow(dead_code)]
pub fn offset_for_point(
    src: (f64, f64),
    slot: (f64, f64),
    zoom: f64,
    point: (f64, f64),
) -> (f64, f64) {
    // The visible window's size does not depend on the offset, so any offset
    // will do to ask for it — and asking is cheaper than restating `cover`'s
    // scale arithmetic here, where it could drift out of agreement.
    let fit = cover_zoom(src, slot, (0.5, 0.5), zoom);
    let overflow = fit.overflow(src);
    let axis = |point: f64, span: f64, visible: f64, overflow: f64| {
        // A comparison rather than `<= 0.0`: a NaN overflow must read as none.
        let overflowing = overflow > 0.0;
        if !overflowing || !point.is_finite() {
            return 0.5;
        }
        // Where the window's *top-left* has to sit for its centre to land on
        // the point, in source pixels, then as a fraction of the travel.
        ((clamp01(point) * span - visible / 2.0) / overflow).clamp(0.0, 1.0)
    };
    (
        axis(point.0, src.0, fit.visible.0, overflow.0),
        axis(point.1, src.1, fit.visible.1, overflow.1),
    )
}

/// Zoom below 1.0 would ask `cover` for a window bigger than the source. NaN
/// falls back to "no zoom" for the same reason [`clamp01`] falls back to
/// centred: a bad number should cost the feature, not the recording.
fn clamp_zoom(zoom: f64) -> f64 {
    if zoom.is_finite() {
        zoom.max(1.0)
    } else {
        1.0
    }
}

#[allow(dead_code)]
fn clamp01(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a camera actually delivers, and what the four layout slots are.
    const CAMERA: (f64, f64) = (1920.0, 1080.0);
    const SPLIT_H_SLOT: (f64, f64) = (522.0, 1080.0);
    const SPLIT_V_SLOT: (f64, f64) = (1080.0, 640.0);
    const TALKING_H_SLOT: (f64, f64) = (1920.0, 1080.0);
    const TALKING_V_SLOT: (f64, f64) = (1080.0, 1920.0);

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// The extreme case, and the reason the camera needs framing at all: a
    /// 16:9 camera into Split's 0.483:1 column shows 522 of its 1920 columns
    /// and throws away 1398 — 73% of the width. Which 522 is entirely the
    /// operator's choice, and entirely invisible without a preview.
    #[test]
    fn the_split_column_shows_a_quarter_of_the_cameras_width() {
        let fit = cover(CAMERA, SPLIT_H_SLOT, (0.5, 0.5));
        assert!(close(fit.scale, 1.0), "height already fits, so no scaling");
        assert!(close(fit.visible.0, 522.0));
        assert!(close(fit.visible.1, 1080.0));
        assert_eq!(fit.overflow(CAMERA), (1398.0, 0.0));
        assert!(close(fit.origin.0, 699.0), "centred is half the overflow");
        assert!(close(fit.origin.1, 0.0), "the fitting axis cannot overflow");
    }

    /// The degenerate case worth pinning: a 16:9 camera into a 16:9 slot has
    /// nothing to place, so the offset must be inert rather than nudging the
    /// image off its own edge.
    #[test]
    fn a_matching_aspect_leaves_the_offset_with_nothing_to_do() {
        for offset in [(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)] {
            let fit = cover(CAMERA, TALKING_H_SLOT, offset);
            assert_eq!(fit.visible, CAMERA);
            assert_eq!(
                fit.origin,
                (0.0, 0.0),
                "offset {offset:?} moved a full frame"
            );
            assert!(close(fit.scale, 1.0));
        }
    }

    #[test]
    fn the_vertical_panel_crops_only_a_little() {
        let fit = cover(CAMERA, SPLIT_V_SLOT, (0.5, 0.5));
        // 640/1080 is the larger ratio, so height drives the scale.
        assert!(close(fit.scale, 640.0 / 1080.0));
        assert!(
            close(fit.visible.1, 1080.0),
            "the driving axis fits exactly"
        );
        let overflow = fit.overflow(CAMERA);
        assert!(overflow.0 > 0.0 && overflow.0 < 120.0, "got {overflow:?}");
        assert!(close(overflow.1, 0.0));
    }

    #[test]
    fn the_vertical_talking_head_crops_hardest() {
        let fit = cover(CAMERA, TALKING_V_SLOT, (0.5, 0.5));
        assert!(
            close(fit.scale, 1920.0 / 1080.0),
            "height drives a 9:16 slot"
        );
        assert!(close(fit.visible.0, 607.5));
        assert!(close(fit.visible.1, 1080.0));
        assert!(close(fit.overflow(CAMERA).0, 1312.5));
    }

    /// The offset's whole contract: 0 is flush left, 1 flush right, and the
    /// window never leaves the source.
    #[test]
    fn the_offset_slides_the_window_across_the_overflow_and_no_further() {
        for slot in [SPLIT_H_SLOT, SPLIT_V_SLOT, TALKING_V_SLOT] {
            let flush_left = cover(CAMERA, slot, (0.0, 0.0));
            let centred = cover(CAMERA, slot, (0.5, 0.5));
            let flush_right = cover(CAMERA, slot, (1.0, 1.0));

            assert_eq!(flush_left.origin, (0.0, 0.0), "{slot:?}");
            let overflow = flush_right.overflow(CAMERA);
            assert!(close(flush_right.origin.0, overflow.0), "{slot:?}");
            assert!(close(centred.origin.0, overflow.0 / 2.0), "{slot:?}");

            for fit in [flush_left, centred, flush_right] {
                assert!(fit.origin.0 >= 0.0 && fit.origin.0 + fit.visible.0 <= CAMERA.0 + 1e-9);
                assert!(fit.origin.1 >= 0.0 && fit.origin.1 + fit.visible.1 <= CAMERA.1 + 1e-9);
            }
        }
    }

    #[test]
    fn an_out_of_range_or_nonsense_offset_is_clamped_rather_than_refused() {
        let overflow = cover(CAMERA, SPLIT_H_SLOT, (0.0, 0.0)).overflow(CAMERA).0;
        assert!(close(
            cover(CAMERA, SPLIT_H_SLOT, (-5.0, 0.0)).origin.0,
            0.0
        ));
        assert!(close(
            cover(CAMERA, SPLIT_H_SLOT, (5.0, 0.0)).origin.0,
            overflow
        ));
        // NaN falls back to centred, not to an edge and not to NaN.
        let nan = cover(CAMERA, SPLIT_H_SLOT, (f64::NAN, f64::NAN));
        assert!(close(nan.origin.0, overflow / 2.0));
    }

    /// `cover` fills by definition, so the visible window scaled by `scale` is
    /// the slot — never letterboxed, never pillarboxed.
    #[test]
    fn the_visible_window_always_fills_the_slot_exactly() {
        for slot in [SPLIT_H_SLOT, SPLIT_V_SLOT, TALKING_H_SLOT, TALKING_V_SLOT] {
            let fit = cover(CAMERA, slot, (0.5, 0.5));
            assert!(close(fit.visible.0 * fit.scale, slot.0), "{slot:?} width");
            assert!(close(fit.visible.1 * fit.scale, slot.1), "{slot:?} height");
        }
    }

    #[test]
    fn a_camera_that_has_not_delivered_a_frame_yet_does_not_produce_nan() {
        let fit = cover((0.0, 0.0), SPLIT_H_SLOT, (0.5, 0.5));
        assert!(fit.scale.is_finite() && fit.origin.0.is_finite());
        assert_eq!(fit.origin, (0.0, 0.0));
    }

    /// The reason `cover_zoom` exists, stated as a test: at zoom 1.0 the
    /// full-bleed longform slot has no travel at all, and a punch-in is the
    /// only thing that gives face tracking somewhere to move to.
    #[test]
    fn a_punch_in_is_what_gives_the_full_bleed_longform_any_travel() {
        let flat = cover_zoom(CAMERA, TALKING_H_SLOT, (0.5, 0.5), 1.0);
        assert_eq!(
            flat.overflow(CAMERA),
            (0.0, 0.0),
            "a 16:9 camera in a 16:9 slot has nothing to slide"
        );

        let punched = cover_zoom(CAMERA, TALKING_H_SLOT, (0.5, 0.5), 1.14);
        let overflow = punched.overflow(CAMERA);
        assert!(overflow.0 > 0.0 && overflow.1 > 0.0, "got {overflow:?}");
        // 1920 / 1.14 = 1684.2 visible columns, so 235.8 of travel.
        assert!(close(punched.visible.0, CAMERA.0 / 1.14));
        assert!(close(punched.visible.1, CAMERA.1 / 1.14));
        assert!(close(overflow.0, CAMERA.0 - CAMERA.0 / 1.14));
    }

    #[test]
    fn zooming_out_is_clamped_away_rather_than_letterboxing_the_slot() {
        for zoom in [0.5, 0.0, -3.0, f64::NAN] {
            let fit = cover_zoom(CAMERA, TALKING_V_SLOT, (0.5, 0.5), zoom);
            assert_eq!(
                fit,
                cover(CAMERA, TALKING_V_SLOT, (0.5, 0.5)),
                "zoom {zoom} should be inert, not inverted"
            );
        }
    }

    /// The round trip that makes tracking correct: ask where a point has to be
    /// framed, feed that offset back to `cover`, and the point lands in the
    /// middle of the visible window.
    ///
    /// The test point is derived from each slot's own travel rather than
    /// shared, because the four slots differ by more than an order of
    /// magnitude in how far they *can* slide — Split · Horizontal has 1398px
    /// of it and Split · Vertical has 97 — and a point one slot can reach is a
    /// point another clamps. Clamping is correct behaviour, proved separately
    /// below; what this test is about is exactness inside the reachable band.
    #[test]
    fn an_offset_derived_for_a_point_puts_that_point_in_the_middle() {
        for (slot, zoom) in [
            (SPLIT_H_SLOT, 1.0),
            (TALKING_V_SLOT, 1.0),
            (SPLIT_V_SLOT, 1.0),
            (TALKING_H_SLOT, 1.14),
        ] {
            let reach = cover_zoom(CAMERA, slot, (0.5, 0.5), zoom).overflow(CAMERA);
            // A quarter of the way out from centre, on whichever axes move.
            let point = (
                0.5 - reach.0 / CAMERA.0 * 0.25,
                0.5 + reach.1 / CAMERA.1 * 0.25,
            );
            let offset = offset_for_point(CAMERA, slot, zoom, point);
            let fit = cover_zoom(CAMERA, slot, offset, zoom);
            let centre = (
                (fit.origin.0 + fit.visible.0 / 2.0) / CAMERA.0,
                (fit.origin.1 + fit.visible.1 / 2.0) / CAMERA.1,
            );
            if reach.0 > 0.0 {
                assert!(
                    close(centre.0, point.0),
                    "{slot:?} x: {centre:?} vs {point:?}"
                );
                assert!(offset.0 < 0.5, "{slot:?} should have slid left: {offset:?}");
            }
            if reach.1 > 0.0 {
                assert!(
                    close(centre.1, point.1),
                    "{slot:?} y: {centre:?} vs {point:?}"
                );
                assert!(offset.1 > 0.5, "{slot:?} should have slid down: {offset:?}");
            }
        }
    }

    /// A face at the very edge cannot be centred without showing what is not
    /// there, so the window goes flush and stops. Tracking that "fails" this
    /// way is tracking behaving correctly.
    #[test]
    fn a_point_near_the_edge_slides_the_window_flush_and_no_further() {
        let hard_left = offset_for_point(CAMERA, SPLIT_H_SLOT, 1.0, (0.01, 0.5));
        assert!(close(hard_left.0, 0.0), "got {hard_left:?}");
        let hard_right = offset_for_point(CAMERA, SPLIT_H_SLOT, 1.0, (0.99, 0.5));
        assert!(close(hard_right.0, 1.0), "got {hard_right:?}");

        // And the window it produces is still entirely inside the source.
        for offset in [hard_left, hard_right] {
            let fit = cover(CAMERA, SPLIT_H_SLOT, offset);
            assert!(fit.origin.0 >= 0.0);
            assert!(fit.origin.0 + fit.visible.0 <= CAMERA.0 + 1e-9);
        }
    }

    /// An axis with no travel answers 0.5 rather than 0, 1 or NaN — 0.5 being
    /// the one value `cover` treats identically to every other when the
    /// overflow is zero.
    #[test]
    fn an_axis_with_nothing_to_slide_answers_centred() {
        // Talking Head · Horizontal at zoom 1.0: both axes are pinned.
        let both = offset_for_point(CAMERA, TALKING_H_SLOT, 1.0, (0.1, 0.9));
        assert_eq!(both, (0.5, 0.5));

        // Split · Horizontal: x has 1398px of travel, y has none.
        let one = offset_for_point(CAMERA, SPLIT_H_SLOT, 1.0, (0.1, 0.9));
        assert!(one.0 < 0.5, "x should have moved: {one:?}");
        assert_eq!(one.1, 0.5, "y has no overflow to move within");
    }

    #[test]
    fn a_camera_with_no_frames_yet_still_answers_an_offset() {
        let offset = offset_for_point((0.0, 0.0), SPLIT_H_SLOT, 1.0, (0.3, 0.3));
        assert_eq!(offset, (0.5, 0.5));
    }
}
