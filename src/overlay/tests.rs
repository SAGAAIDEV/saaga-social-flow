//! Hit-testing tests for the overlay's controls.
//!
//! All of it is geometry over plain structs, so none of it needs AppKit, a
//! display, or the main thread — which is the point of keeping `grab_anywhere`
//! a free function over `&[DrawnRegion]` rather than a method on the view.

use super::hit::grab_anywhere;
use super::*;
use crate::region::{Axis, Corner};

fn region(rect: PointRect, orientation: Orientation) -> DrawnRegion {
    DrawnRegion {
        orientation,
        rect,
        pixels: (1402, 1080),
        zoom: 1.0,
        min_width: 40.0,
        child_of: None,
    }
}

/// The real geometry: Split-Vertical's default placement sits it on
/// Split-Horizontal's top-left corner, so its gnomon is well inside the
/// horizontal region's body.
fn overlapping() -> Vec<DrawnRegion> {
    let horizontal = PointRect {
        x: 400.0,
        y: 200.0,
        w: 701.0,
        h: 540.0,
    };
    let vertical = PointRect {
        x: 400.0,
        y: 200.0,
        w: 540.0,
        h: 640.0,
    };
    vec![
        region(horizontal, Orientation::Horizontal),
        region(vertical, Orientation::Vertical),
    ]
}

/// The bug this file's tier sweep exists to prevent: clicking the inactive
/// region's gnomon used to grab the *active* region's body instead, because
/// hit-testing ran region-major and the active body answered first. The
/// vertical arrows were simply not clickable.
///
/// The body is no longer a target at all, but the sweep order still matters
/// between handles, and this is the case that proves the inactive region is
/// reachable through the active one.
#[test]
fn an_inactive_regions_gnomon_beats_the_active_regions_body() {
    let regions = overlapping();
    let vertical = regions[1].rect;
    let (cx, cy) = vertical.center();

    // Precondition — without this overlap the test proves nothing.
    assert!(
        regions[0].rect.contains((cx, cy)),
        "fixture is useless: the vertical gnomon must sit inside the \
         horizontal region's body",
    );

    for (label, point, want) in [
        ("knob", (cx, cy), None),
        ("x arm", (cx + 40.0, cy), Some(Axis::X)),
        ("y arm", (cx, cy - 40.0), Some(Axis::Y)),
    ] {
        let grabbed = grab_anywhere(&regions, 0, point)
            .unwrap_or_else(|| panic!("{label} grabbed nothing at all"));
        assert_eq!(
            grabbed.region, 1,
            "{label} grabbed the horizontal region instead of the vertical one",
        );
        match (want, grabbed.drag) {
            (Some(axis), Drag::Axis { axis: got, .. }) => assert_eq!(got, axis),
            (None, Drag::Move { .. }) => {}
            (_, other) => panic!("{label} produced {other:?}"),
        }
    }
}

/// And the vertical region moves on its own: dragging its gnomon changes
/// its rect and leaves the horizontal one untouched.
#[test]
fn dragging_the_vertical_region_leaves_the_horizontal_one_alone() {
    let mut regions = overlapping();
    let before = regions[0].rect;
    let (cx, cy) = regions[1].rect.center();
    let geometry = DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (1512.0, 982.0),
        pixels: (3024, 1964),
        primary_height_points: 982.0,
    };

    let grabbed = grab_anywhere(&regions, 0, (cx + 40.0, cy)).expect("grabs the x arm");
    let Drag::Axis { axis, grab } = grabbed.drag else {
        panic!("expected an axis drag, got {:?}", grabbed.drag);
    };
    let target = (cx + 140.0, cy);
    regions[grabbed.region].rect = regions[grabbed.region].rect.moved_on(
        axis,
        (target.0 - grab.0, target.1 - grab.1),
        &geometry,
    );

    assert_eq!(regions[0].rect, before, "the horizontal region moved");
    assert_eq!(regions[1].rect.x, before.x + 100.0);
    assert_eq!(regions[1].rect.y, before.y, "an X drag moved y");
}

/// Both frames are resizable, and now both draw handles saying so. This is the
/// half that has to be true for those handles to mean anything: a corner of the
/// *inactive* region answers, through the active region's body.
#[test]
fn the_inactive_regions_corner_handles_answer_too() {
    let regions = overlapping();
    let vertical = regions[1].rect;

    for corner in [Corner::TopRight, Corner::BottomLeft, Corner::BottomRight] {
        let grabbed = grab_anywhere(&regions, 0, corner.at(&vertical))
            .unwrap_or_else(|| panic!("{corner:?} on the vertical region grabbed nothing"));
        assert_eq!(
            grabbed.region, 1,
            "{corner:?} grabbed the horizontal region instead of the vertical one",
        );
        assert_eq!(grabbed.drag, Drag::Resize { corner });
    }
}

/// The one corner that does *not* belong to the vertical frame, and the reason
/// the active region is painted last: at the default placement the child sits on
/// its parent's top-left corner, so that handle is coincident and the tie goes
/// to the frame being recorded. Drawing order has to agree, or the handle on top
/// would be the one you cannot grab.
#[test]
fn a_coincident_corner_goes_to_the_active_region() {
    let regions = overlapping();
    assert_eq!(
        regions[0].rect.x, regions[1].rect.x,
        "fixture is useless unless the two top-left corners coincide",
    );
    let shared = Corner::TopLeft.at(&regions[0].rect);
    assert_eq!(grab_anywhere(&regions, 0, shared).unwrap().region, 0);
    assert_eq!(grab_anywhere(&regions, 1, shared).unwrap().region, 1);
}

/// Within a tier the active region still wins, so two coincident handles
/// resolve to the one being recorded.
#[test]
fn the_active_region_wins_a_tie_inside_one_tier() {
    let same = PointRect {
        x: 400.0,
        y: 200.0,
        w: 600.0,
        h: 460.0,
    };
    let regions = vec![
        region(same, Orientation::Horizontal),
        region(same, Orientation::Vertical),
    ];
    let centre = same.center();
    assert_eq!(grab_anywhere(&regions, 1, centre).unwrap().region, 1);
    assert_eq!(grab_anywhere(&regions, 0, centre).unwrap().region, 0);
}
/// The property that makes click-through possible: the region's interior is
/// *not* a drag target, so the overlay wants nothing there and can let the
/// press reach whatever is behind it.
///
/// Without this the overlay would have to swallow clicks over exactly the
/// windows being framed, which is the whole reason the gnomon exists.
#[test]
fn the_region_interior_is_not_a_target_so_clicks_pass_through() {
    let regions = overlapping();
    let rect = regions[0].rect;
    // Well inside the region, clear of its centre gnomon and its corners.
    let inside = (rect.x + rect.w * 0.75, rect.y + rect.h * 0.75);
    assert!(
        rect.contains(inside),
        "fixture point must be inside the region"
    );
    assert_eq!(
        grab_anywhere(&regions, 0, inside),
        None,
        "the overlay claimed a point in open region interior, so a click there \
         would be eaten instead of reaching the window behind it",
    );
}

/// And the handles still answer, or the overlay would be click-through
/// everywhere and nothing could be dragged at all.
#[test]
fn the_handles_still_answer() {
    let regions = overlapping();
    let (cx, cy) = regions[0].rect.center();
    assert!(grab_anywhere(&regions, 0, (cx, cy)).is_some(), "knob");
    assert!(
        grab_anywhere(&regions, 0, (cx + 40.0, cy)).is_some(),
        "x arm"
    );
    assert!(
        grab_anywhere(&regions, 0, (cx, cy - 40.0)).is_some(),
        "y arm"
    );
    let corner = Corner::TopLeft.at(&regions[0].rect);
    assert!(
        grab_anywhere(&regions, 0, corner).is_some(),
        "corner handle"
    );
}

/// A 2× display roomy enough for a widened Split pair.
fn geom() -> DisplayGeometry {
    DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (1728.0, 1117.0),
        pixels: (3456, 2234),
        primary_height_points: 1117.0,
    }
}

/// Tracking engaged on `regions`, punched in on `anchor`.
fn punched(regions: Vec<DrawnRegion>, anchor: (f64, f64), punch: f64) -> OverlayState {
    let capture = regions[0].rect;
    let cell = std::sync::Arc::new(crate::region::framing::TrackCell::new());
    cell.set(Some(crate::region::framing::Track { anchor, punch }));
    OverlayState {
        geometry: geom(),
        regions,
        active: 0,
        grabbed: None,
        tracking: Some(Tracked {
            cell,
            capture,
            // Split's two slots at 2×: 1402×1080 and 1080×1280 in pixels.
            floors: [(701.0, 540.0), (540.0, 640.0)],
        }),
    }
}

/// **Both features at once.** Setting the rest position and punching into it
/// are independent, and neither may cost the other.
///
/// They were not independent for one revision: the punch-in was guarded by
/// suppressing every grab target, on the theory that a frame following the
/// pointer would park its knob under the cursor. That reasoning belonged to a
/// dwell gate that was never built — with a held key combo the controls live
/// on the authored region, which tracking never moves. This pins the outcome
/// so the guard cannot come back by accident: while the frame is fully punched
/// in, every control is still reachable and the region is still adjustable.
#[test]
fn a_punched_in_frame_is_still_fully_adjustable() {
    let rect = PointRect {
        x: 300.0,
        y: 150.0,
        w: 756.0,
        h: 896.0,
    };
    let state = punched(vec![region(rect, Orientation::Vertical)], (0.25, 0.5), 1.0);

    // The punch is live: the recorded frame is a strict sub-rect of the region.
    let framed = state.recording_rect(&state.regions[0]);
    assert!(
        framed.w < rect.w && framed.h < rect.h,
        "fully punched in but the frame is still {}×{} of {}×{}",
        framed.w,
        framed.h,
        rect.w,
        rect.h,
    );

    // And every control the operator sets the rest position with still answers.
    for corner in Corner::ALL {
        assert!(
            grab_anywhere(&state.regions, 0, corner.at(&rect)).is_some(),
            "{corner:?} is unreachable while punched in — the region cannot be \
             resized",
        );
    }
    assert!(
        grab_anywhere(&state.regions, 0, rect.center()).is_some(),
        "the free-move knob is unreachable while punched in",
    );
    for axis in Axis::ALL {
        let (cx, cy) = rect.center();
        let probe = match axis {
            Axis::X => (cx + 40.0, cy),
            Axis::Y => (cx, cy - 40.0),
        };
        assert!(
            grab_anywhere(&state.regions, 0, probe).is_some(),
            "the {axis:?} arm is unreachable while punched in",
        );
    }
}

/// The controls hit-test against the *authored* region, not the punched-in
/// view — so a deep punch-in does not shrink the target you have to hit.
///
/// The failure this prevents is subtle and infuriating: handles drawn in one
/// place and grabbable in another, which reads as the overlay ignoring clicks.
#[test]
fn the_controls_follow_the_region_not_the_punched_in_view() {
    let rect = PointRect {
        x: 300.0,
        y: 150.0,
        w: 756.0,
        h: 896.0,
    };
    let state = punched(vec![region(rect, Orientation::Vertical)], (0.9, 0.9), 1.0);
    let framed = state.recording_rect(&state.regions[0]);
    assert_ne!(framed, rect, "fixture is useless: nothing was punched in");

    // The region's own bottom-right corner answers...
    assert!(
        grab_anywhere(&state.regions, 0, Corner::BottomRight.at(&rect)).is_some(),
        "the region's corner stopped answering once the view punched away from it",
    );
    // ...and the punched-in view's own corner, which sits well inside the
    // region and nowhere near a handle or an arm, is not a target at all.
    assert!(
        grab_anywhere(&state.regions, 0, Corner::TopLeft.at(&framed)).is_none(),
        "the punched-in view's corner is grabbable — controls have leaked onto \
         the frame that moves",
    );
}
