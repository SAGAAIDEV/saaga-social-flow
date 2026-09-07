//! What the tracked crop is *for*, one failure per test.
//!
//! Every case here is a thing that visibly ruins a take, and every one of them
//! produces a picture rather than an error: a stretched frame, a crawling line
//! of text, a black wedge down one side, a punch-in that quietly upscales a
//! screenshot, or a frame that never comes home when the key is released. None
//! of that trips a dimension assertion anywhere downstream —
//! `crop_rect_into_slot` will happily scale whatever it is given into the slot
//! — so this file is the only place a regression in the geometry can be caught
//! before it reaches the edit.
//!
//! The fixtures are the operator's real framing, in buffer pixels on a 2×
//! display: a vertical region sitting inside a wider horizontal one, which is
//! also the capture. That is the shape the travel exists for.
//!
//! Split out under the repo's file-size rule, same sibling-file convention as
//! `smooth_tests.rs` and `crop_tests.rs`.

use super::*;

/// The vertical layout's screen slot, which is also the 1:1 floor.
const SLOT: (f64, f64) = (1080.0, 1280.0);

/// The capture: the horizontal region, which the vertical sits inside.
fn capture() -> BufferRect {
    (0.0, 0.0, 2394.0, 1843.2)
}

/// The vertical region at rest — narrower than the capture, so it has room to
/// travel, and larger than the slot, so it has room to tighten.
fn roomy() -> BufferRect {
    // The slot at the operator's 1.44× effective zoom. Derived from the slot
    // rather than typed as a round number, because `base_size` guarantees a
    // region carries its slot's aspect *exactly* and a fixture that does not
    // tests a shape the pipeline cannot produce.
    (108.0, 0.0, SLOT.0 * 1.44, SLOT.1 * 1.44)
}

/// Points across the capture, including well outside the resting region on
/// both sides — which is the case the travel exists for.
fn probes() -> [(f64, f64); 7] {
    let c = capture();
    [
        (c.2 * 0.5, c.3 * 0.5),
        (0.0, 0.0),
        (c.2, c.3),
        (c.2 * 0.95, c.3 * 0.5),
        (c.2 * 0.05, c.3 * 0.5),
        (-500.0, c.3 * 0.5),
        (c.2 + 500.0, c.3 * 0.5),
    ]
}

const PUNCHES: [f64; 7] = [0.0, 0.1, 0.5, 0.9, 1.0, -0.4, 1.7];

/// Switching tracking on while nothing is held must change nothing at all.
///
/// Not "close enough": bit-for-bit. This is what lets the feature be enabled
/// mid-session without the framing twitching, and it is why an effectively
/// spent punch short-circuits rather than rounding its way back.
#[test]
fn at_rest_the_crop_is_the_authored_rect_bit_for_bit() {
    let rest = roomy();
    for pointer in probes() {
        assert_eq!(
            tracked_crop(rest, capture(), pointer, 0.0, SLOT),
            rest,
            "an unpunched frame moved for a pointer at {pointer:?} — enabling \
             tracking would visibly jump the frame",
        );
    }
}

/// Releasing brings the frame home, however far it travelled.
///
/// The ease-out is exponential and never reaches zero exactly, so without the
/// settled floor the frame would park a fraction of a pixel off its resting
/// rect forever and the overlay would draw its punched outline permanently.
#[test]
fn releasing_returns_the_frame_to_rest() {
    let rest = roomy();
    let far = (capture().2 * 0.95, 900.0);
    let travelled = tracked_crop(rest, capture(), far, 1.0, SLOT);
    assert_ne!(travelled, rest, "fixture is useless: nothing travelled");

    for spent in [0.0, 1e-6, 9e-4] {
        assert_eq!(
            tracked_crop(rest, capture(), far, spent, SLOT),
            rest,
            "a spent punch of {spent} left the frame away from rest",
        );
    }
}

/// **The travel.** A punched-in frame must be able to leave the region it was
/// framed in — that is the whole point, showing something the resting frame
/// does not cover.
#[test]
fn a_punched_in_frame_travels_outside_its_own_region() {
    let rest = roomy();
    let right_edge = rest.0 + rest.2;
    let far_right = (capture().2 - 10.0, 900.0);

    let (x, _, w, _) = tracked_crop(rest, capture(), far_right, 1.0, SLOT);
    assert!(
        x + w > right_edge,
        "the frame stopped at its own region's edge ({right_edge}) instead of \
         travelling to the pointer: got {x}..{}",
        x + w,
    );

    let far_left = (10.0, 900.0);
    let (x, _, _, _) = tracked_crop(rest, capture(), far_left, 1.0, SLOT);
    assert!(
        x < rest.0,
        "the frame would not travel left of its region: got {x}, region starts \
         at {}",
        rest.0,
    );
}

/// It travels, but never off the captured screen.
///
/// The damage if it did is not cosmetic. A crop outside the buffer is
/// composited with black fill *and* the wrong magnification, because
/// `crop_rect_into_slot` derives its scale from the requested width and does
/// not clamp. Nothing returns an error and nothing looks obviously broken.
#[test]
fn the_crop_never_leaves_the_capture() {
    let rest = roomy();
    let cap = capture();
    for pointer in probes() {
        for punch in PUNCHES {
            let (x, y, w, h) = tracked_crop(rest, cap, pointer, punch, SLOT);
            assert!(
                x >= cap.0 - 1e-9
                    && y >= cap.1 - 1e-9
                    && x + w <= cap.0 + cap.2 + 1e-9
                    && y + h <= cap.1 + cap.3 + 1e-9,
                "pointer {pointer:?} at punch {punch} escaped the capture: got \
                 ({x}, {y}, {w}, {h}) out of {cap:?}",
            );
        }
    }
}

/// A drifting aspect does not letterbox or crash — it *stretches*, because
/// `crop_rect_into_slot` scales the two axes independently. That is a picture
/// of the wrong thing at exactly the right dimensions, which is the failure
/// class nothing else in the pipeline can catch.
#[test]
fn every_punch_keeps_the_authored_aspect() {
    let rest = roomy();
    let want = rest.2 / rest.3;
    for pointer in probes() {
        for punch in PUNCHES {
            let (_, _, w, h) = tracked_crop(rest, capture(), pointer, punch, SLOT);
            assert!(
                (w / h - want).abs() < 1e-12,
                "pointer {pointer:?} at punch {punch} changed the aspect: \
                 {want} -> {}",
                w / h,
            );
        }
    }
}

/// The punch-in stops when one buffer pixel is one output pixel, so the
/// feature can never upscale a screenshot of text.
#[test]
fn the_punch_in_stops_at_one_buffer_pixel_per_output_pixel() {
    let rest = roomy();
    for punch in [1.0, 1.5, 4.0, f64::MAX] {
        let (_, _, w, h) = tracked_crop(rest, capture(), (1200.0, 900.0), punch, SLOT);
        // Neither axis below the floor — that is the upscale this prevents...
        assert!(
            w >= SLOT.0 - 1e-6 && h >= SLOT.1 - 1e-6,
            "punch {punch} reached {w}×{h}, under the {}×{} floor — the take \
             would be upscaled",
            SLOT.0,
            SLOT.1,
        );
        // ...and one axis exactly on it, or the punch stopped early and gave
        // away sharpness it had. Only one, because a region whose aspect is a
        // hair off its slot's binds on the tighter axis and clears the other.
        assert!(
            (w - SLOT.0).abs() < 1e-6 || (h - SLOT.1).abs() < 1e-6,
            "punch {punch} stopped at {w}×{h}, short of the {}×{} floor on \
             both axes",
            SLOT.0,
            SLOT.1,
        );
    }
}

/// A 1× 1080p display cannot give the vertical region its 1280 points of
/// height, so `resolve` hands it over already smaller than its own slot.
///
/// There is no room to tighten there — but there is still room to *travel*,
/// and the frame should still go where the pointer is. Zoom is what that
/// display cannot afford, not tracking.
#[test]
fn a_region_with_no_room_to_tighten_can_still_travel() {
    let cramped = (400.0, 0.0, 911.0, 1080.0);
    let cap = (0.0, 0.0, 1920.0, 1080.0);
    let (x, _, w, h) = tracked_crop(cramped, cap, (1800.0, 540.0), 1.0, SLOT);

    assert!(
        (w - cramped.2).abs() < 1e-9 && (h - cramped.3).abs() < 1e-9,
        "a pre-clamped region tightened to {w}×{h} — that is an upscale",
    );
    assert!(
        x > cramped.0,
        "a pre-clamped region refused to travel: still at {x}",
    );
}

/// The crawl test.
///
/// A crop whose origin moves by a fraction of a pixel resamples every line of
/// text at a different subpixel phase on every frame, which reads as the whole
/// image creeping while the frame moves. Integer origins stop it, and at 30fps
/// a one-pixel step is invisible.
#[test]
fn the_origin_lands_on_whole_pixels() {
    let rest = roomy();
    // Deliberately untidy positions: a pointer never lands on a round number,
    // and rounding that only works for round numbers is not rounding.
    for step in 0..40 {
        let pointer = (300.0 + step as f64 * 37.317, 500.0 + step as f64 * 11.71);
        let (x, y, _, _) = tracked_crop(rest, capture(), pointer, 1.0, SLOT);
        assert!(
            x.fract().abs() < 1e-9 && y.fract().abs() < 1e-9,
            "pointer {pointer:?} produced a fractional origin ({x}, {y}) — \
             text would crawl as the frame moves",
        );
    }
}

/// A pointer past the edge of the capture shows the edge, not half a frame of
/// nothing.
#[test]
fn a_pointer_beyond_the_capture_shows_its_edge() {
    let rest = roomy();
    let cap = capture();

    let (x, _, _, _) = tracked_crop(rest, cap, (-4000.0, 900.0), 1.0, SLOT);
    assert!((x - cap.0).abs() < 1e-9, "did not stop at the left edge: {x}");

    let (x, _, w, _) = tracked_crop(rest, cap, (cap.2 + 4000.0, 900.0), 1.0, SLOT);
    assert!(
        ((x + w) - (cap.0 + cap.2)).abs() < 1.0,
        "did not stop at the right edge: {} vs {}",
        x + w,
        cap.0 + cap.2,
    );
}

/// Nonsense in, the operator's own framing out.
///
/// Every one of these is reachable: a pointer read that failed, a smoother
/// dividing by a zero `dt`, a hand-edited config. The house rule on a capture
/// queue is that a tracking failure costs the framing, never the recording, so
/// none of them may produce a rect at all.
#[test]
fn a_nonsense_pointer_or_punch_returns_the_authored_rect() {
    let rest = roomy();
    let bad = f64::NAN;
    for (pointer, punch) in [
        ((bad, 500.0), 0.5),
        ((500.0, bad), 0.5),
        ((f64::INFINITY, 500.0), 0.5),
        ((500.0, 500.0), bad),
        ((500.0, 500.0), f64::NEG_INFINITY),
    ] {
        assert_eq!(
            tracked_crop(rest, capture(), pointer, punch, SLOT),
            rest,
            "pointer {pointer:?} at punch {punch} produced a rect instead of \
             falling back to the authored one",
        );
    }
    for degenerate in [(0.0, 0.0, 0.0, 100.0), (0.0, 0.0, 100.0, -5.0)] {
        assert_eq!(
            tracked_crop(degenerate, capture(), (500.0, 500.0), 0.5, SLOT),
            degenerate,
            "a zero-or-negative region produced a rect",
        );
    }
}

/// The bug this file did not catch the first time: a frame that tracks the
/// pointer perfectly and renders the identical rect every time.
///
/// Travel and tightening are the same quantity — at `punch == 0.0` the frame
/// is the authored rect and following the pointer moves it nowhere. Stage 1
/// shipped with the punch pinned at the wide end and was inert, and the test
/// that existed asserted that inertness correctly while saying nothing about
/// the case that mattered. This is that case.
#[test]
fn a_punched_in_frame_actually_follows_the_pointer() {
    let rest = roomy();
    let cap = capture();
    let y = 900.0;
    let left = tracked_crop(rest, cap, (100.0, y), 1.0, SLOT);
    let middle = tracked_crop(rest, cap, (cap.2 / 2.0, y), 1.0, SLOT);
    let right = tracked_crop(rest, cap, (cap.2 - 100.0, y), 1.0, SLOT);

    assert!(
        left.0 < middle.0 && middle.0 < right.0,
        "the frame did not move with the pointer: {} then {} then {}",
        left.0,
        middle.0,
        right.0,
    );
    // And it spans the whole capture, not just its own region: 2394 wide
    // punching to a 1080 floor leaves 1314 pixels of travel.
    assert!(
        (right.0 - left.0 - (cap.2 - SLOT.0)).abs() < 1.0,
        "travel was {} pixels, not the {} the capture affords",
        right.0 - left.0,
        cap.2 - SLOT.0,
    );
}
