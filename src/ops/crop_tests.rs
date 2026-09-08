//! What `crop.rs`'s two ops have to get right, and the ways they can silently
//! not.
//!
//! Split out under the repo's file-size rule rather than because the tests form
//! a unit of their own: `crop.rs` crossed 500 lines when `Cover` gained a
//! [`Framing`](crate::region::framing::Framing). Same sibling-file convention
//! as `clock_tests.rs` and `level_tests.rs`.
//!
//! One theme runs through all of them: **a wrong crop still renders.** Every
//! failure these guard against produces a plausible picture of the wrong thing
//! at exactly the right dimensions — so no assertion about output size would
//! catch it, nothing returns an error, and the first anyone sees of it is in
//! the edit.

use super::*;

/// The flip that is easy to get wrong and impossible to see in a dimension
/// check: a band across the **top** of a 1080-tall frame must become a band
/// at the top in Core Image's bottom-left space, which means a y of
/// 1080 - 0 - 200 = 880, not 0.
#[test]
fn the_crop_rect_is_flipped_into_core_images_space() {
    let top_band = Crop::new((0.0, 0.0, 1920.0, 200.0), PixelSize::rounded(1920.0, 200.0));
    let rect = top_band.source_rect(1080.0);
    assert_eq!(rect.origin.x, 0.0);
    assert_eq!(
        rect.origin.y, 880.0,
        "a band at the top of the frame must sit high in a bottom-left space",
    );
    assert_eq!(rect.size.height, 200.0);

    let bottom_band = Crop::new(
        (0.0, 880.0, 1920.0, 200.0),
        PixelSize::rounded(1920.0, 200.0),
    );
    assert_eq!(
        bottom_band.source_rect(1080.0).origin.y,
        0.0,
        "and a band at the bottom must sit at the origin",
    );
}

#[test]
fn a_full_frame_crop_is_the_whole_frame() {
    let whole = Crop::new(
        (0.0, 0.0, 1920.0, 1080.0),
        PixelSize::rounded(1920.0, 1080.0),
    );
    let rect = whole.source_rect(1080.0);
    assert_eq!((rect.origin.x, rect.origin.y), (0.0, 0.0));
    assert_eq!((rect.size.width, rect.size.height), (1920.0, 1080.0));
}

/// The flip is its own inverse, so applying it twice returns the rect —
/// which is the property that makes it safe to reason about in either
/// space.
#[test]
fn flipping_twice_returns_the_original_rect() {
    let source = (120.0, 340.0, 700.0, 400.0);
    let op = Crop::new(source, PixelSize::rounded(700.0, 400.0));
    let flipped = op.source_rect(1080.0);
    let back = Crop::new(
        (
            flipped.origin.x,
            flipped.origin.y,
            flipped.size.width,
            flipped.size.height,
        ),
        PixelSize::rounded(700.0, 400.0),
    )
    .source_rect(1080.0);
    assert_eq!((back.origin.x, back.origin.y), (source.0, source.1));
}

#[test]
fn an_op_that_was_never_opened_drops_rather_than_passing_through() {
    // No `open`, so no renderer and no pool — the state a graph bug would
    // produce. Passing the frame through would emit an uncropped file at
    // the wrong aspect, which is the failure this guards.
    let mut op = Crop::new((0.0, 0.0, 100.0, 100.0), PixelSize::rounded(100.0, 100.0));
    let mut sidecar = Sidecar::default();
    let pixels =
        crate::ops::frame::test_support::pixel_buffer(64, 64, u32::from_be_bytes(*b"420v"), 64);
    let mut frame = Frame::new(
        pixels,
        objc2_core_media::CMTime {
            value: 0,
            timescale: 1,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        },
        crate::ops::StreamId::Screen,
        &mut sidecar,
    );
    assert!(matches!(op.apply(&mut frame).unwrap(), Flow::Drop));
    assert!(!frame.was_replaced());
}

#[test]
fn a_top_band_flips_the_same_way_as_crop() {
    let op = Cover::for_canvas("cover-horizontal", (1920.0, 1080.0), Framing::fixed());
    let rect = op.source_rect((0.0, 0.0), (1920.0, 200.0), 1080.0);
    assert_eq!(rect.origin.y, 880.0);
}

#[test]
fn an_unopened_cover_drops() {
    let mut op = Cover::for_canvas("cover-vertical", (1080.0, 1920.0), Framing::fixed());
    let mut sidecar = Sidecar::default();
    let pixels =
        crate::ops::frame::test_support::pixel_buffer(64, 64, u32::from_be_bytes(*b"BGRA"), 64);
    let mut frame = Frame::new(
        pixels,
        objc2_core_media::CMTime {
            value: 0,
            timescale: 1,
            flags: objc2_core_media::CMTimeFlags::Valid,
            epoch: 0,
        },
        crate::ops::StreamId::Camera,
        &mut sidecar,
    );
    assert!(matches!(op.apply(&mut frame).unwrap(), Flow::Drop));
    assert!(!frame.was_replaced());
}
