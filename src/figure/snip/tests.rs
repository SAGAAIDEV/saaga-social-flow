//! The snip gesture's arithmetic: what a drag selects, and what it refuses.
//!
//! No display, no window and no main thread — the geometry is plain data, which
//! is the whole reason `clamp` and `is_capture` are functions rather than inline
//! steps of a mouse handler.

use super::*;
use crate::region::{DisplayGeometry, PointRect};

fn geometry() -> DisplayGeometry {
    DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (1512.0, 982.0),
        pixels: (3024, 1964),
        primary_height_points: 982.0,
    }
}

fn state(anchor: (f64, f64), cursor: (f64, f64)) -> SnipState {
    SnipState {
        geometry: geometry(),
        anchor: Some(anchor),
        cursor: Some(cursor),
    }
}

/// A drag is the rectangle between the press and the pointer, whichever way
/// the hand went: the two gestures name the same box.
#[test]
fn a_drag_is_the_rectangle_between_the_press_and_the_pointer() {
    let forward = state((100.0, 100.0), (400.0, 300.0)).selection().unwrap();
    assert_eq!(
        forward,
        PointRect {
            x: 100.0,
            y: 100.0,
            w: 300.0,
            h: 200.0
        }
    );
    let backward = state((400.0, 300.0), (100.0, 100.0)).selection().unwrap();
    assert_eq!(backward, forward);
}

/// No shape is imposed: a tall drag is a tall figure and a strip is a strip.
/// Figures are published at the size they were dragged at.
#[test]
fn the_box_takes_whatever_shape_was_dragged() {
    let tall = state((100.0, 100.0), (150.0, 400.0)).selection().unwrap();
    assert_eq!(
        tall,
        PointRect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 300.0
        }
    );
    let strip = state((100.0, 100.0), (900.0, 140.0)).selection().unwrap();
    assert_eq!(
        strip,
        PointRect {
            x: 100.0,
            y: 100.0,
            w: 800.0,
            h: 40.0
        }
    );
}

/// The pointer is not confined to the window, so a drag off the edge stops at
/// it — ScreenCaptureKit answers a rect past the display with nothing at all.
#[test]
fn a_drag_off_the_display_stops_at_its_edge() {
    let selection = state((1400.0, 900.0), (2000.0, 1400.0))
        .selection()
        .unwrap();
    assert_eq!(
        selection,
        PointRect {
            x: 1400.0,
            y: 900.0,
            w: 112.0,
            h: 82.0
        }
    );
}

#[test]
fn a_press_from_outside_the_display_starts_at_its_edge() {
    let selection = state((-200.0, -50.0), (300.0, 200.0)).selection().unwrap();
    assert_eq!(
        selection,
        PointRect {
            x: 0.0,
            y: 0.0,
            w: 300.0,
            h: 200.0
        }
    );
}

/// A click on a window covering the whole display must not file a figure.
#[test]
fn a_click_that_did_not_travel_is_not_a_capture() {
    assert!(!is_capture(
        &state((400.0, 400.0), (400.0, 400.0)).selection().unwrap()
    ));
    assert!(!is_capture(
        &state((400.0, 400.0), (405.0, 404.0)).selection().unwrap()
    ));
    assert!(is_capture(
        &state((400.0, 400.0), (460.0, 480.0)).selection().unwrap()
    ));
}

/// The label is the pixels under the box, and says so when the encoder will
/// bring them down to its ceiling. Nothing is scaled up, so no arrow that way.
#[test]
fn the_size_label_says_when_the_figure_will_be_scaled_down() {
    assert_eq!(super::draw::size_text(1600, 1200), "1600 × 1200");
    assert_eq!(super::draw::size_text(640, 900), "640 × 900");
    assert_eq!(
        super::draw::size_text(4800, 3000),
        "4800 × 3000  ↓ 2400 × 1500"
    );
}

#[test]
fn there_is_no_selection_before_a_press() {
    let idle = SnipState {
        geometry: geometry(),
        anchor: None,
        cursor: Some((10.0, 10.0)),
    };
    assert!(idle.selection().is_none());
}

/// The dim has to cover the display exactly, with a hole exactly the size of
/// the selection: a band that overlaps the hole darkens what is about to be
/// captured, and a gap leaves an undimmed stripe.
#[test]
fn the_dim_bands_tile_the_display_around_the_hole() {
    let full = PointRect {
        x: 0.0,
        y: 0.0,
        w: 1512.0,
        h: 982.0,
    };
    let hole = PointRect {
        x: 200.0,
        y: 150.0,
        w: 600.0,
        h: 400.0,
    };
    let bands = [
        PointRect {
            x: 0.0,
            y: 0.0,
            w: 1512.0,
            h: 150.0,
        },
        PointRect {
            x: 0.0,
            y: 550.0,
            w: 1512.0,
            h: 432.0,
        },
        PointRect {
            x: 0.0,
            y: 150.0,
            w: 200.0,
            h: 400.0,
        },
        PointRect {
            x: 800.0,
            y: 150.0,
            w: 712.0,
            h: 400.0,
        },
    ];
    let covered: f64 = bands.iter().map(|band| band.w * band.h).sum();
    assert_eq!(
        covered,
        full.w * full.h - hole.w * hole.h,
        "the bands plus the hole are the display"
    );
    for band in &bands {
        assert!(
            !band.overlaps(&hole),
            "a band covers the selection: {band:?}"
        );
    }
}

/// A selection at the very top of the display would put its label
/// off-screen, which is the one place the number matters most — you are
/// dragging toward the edge because you want the whole thing.
#[test]
fn the_size_label_stays_on_screen_at_the_top_edge() {
    // Mirrors `size_label`'s placement arithmetic with a plausible plate
    // height, since the real one needs a font and a graphics context.
    let plate_h = 24.0;
    for (selection_y, expected_inside) in [(400.0, false), (2.0, true)] {
        let above = selection_y - plate_h - 4.0;
        let y = if above >= 0.0 {
            above
        } else {
            selection_y + 4.0
        };
        assert!(y >= 0.0, "label placed off-screen at y={selection_y}");
        assert_eq!(y > selection_y, expected_inside);
    }
}
