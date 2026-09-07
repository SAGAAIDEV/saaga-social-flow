//! Tests for the saved half of a region: offset, zoom, and the parent link
//! that makes Vertical follow Horizontal.
//!
//! All arithmetic, no display — the parenting behaviour is provable without
//! hardware, which is the point of keeping it out of the AppKit call sites.

use super::*;

/// 1512x982 points at 2x — a 14" MacBook Pro in its default scaled mode.
fn geom() -> DisplayGeometry {
    DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (1512.0, 982.0),
        pixels: (3024, 1964),
        primary_height_points: 982.0,
    }
}

/// A display with room to spare, so `resolve`'s fit-shrink never fires and
/// the tests are about parenting rather than about clamping.
fn roomy() -> DisplayGeometry {
    DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (5000.0, 5000.0),
        pixels: (5000, 5000),
        primary_height_points: 5000.0,
    }
}

const SPLIT_H: PixelSize = PixelSize { w: 1402, h: 1080 };
const SPLIT_V: PixelSize = PixelSize { w: 1080, h: 1280 };

#[test]
fn base_size_is_the_output_divided_by_the_backing_scale() {
    let base = base_size(SPLIT_H, &geom());
    assert!((base.0 - 701.0).abs() < 1e-9);
    assert!((base.1 - 540.0).abs() < 1e-9);
}

/// The reason `base_size` takes a rounded `PixelSize`: a region's aspect
/// has to equal its *file's*, not its slot's, or `preservesAspectRatio`
/// letterboxes the difference.
#[test]
fn a_resolved_region_matches_its_output_aspect_exactly() {
    for output in [SPLIT_H, SPLIT_V] {
        for zoom in [0.5, 1.0, 1.7] {
            let g = roomy();
            let base = base_size(output, &g);
            let resolved = resolve(
                base,
                Placement { offset: (10.0, 10.0), zoom },
                None,
                &g,
            );
            let want = output.w as f64 / output.h as f64;
            let got = resolved.rect.w / resolved.rect.h;
            assert!(
                (got - want).abs() < 1e-12,
                "{output:?} at {zoom}x: region aspect {got} != file aspect {want}",
            );
        }
    }
}

#[test]
fn zoom_scales_the_region_and_nothing_else() {
    let base = base_size(SPLIT_H, &roomy());
    let at_one = resolve(base, Placement::default(), None, &roomy());
    let at_two = resolve(
        base,
        Placement {
            offset: (0.0, 0.0),
            zoom: 2.0,
        },
        None,
        &roomy(),
    );
    assert!((at_two.rect.w / at_one.rect.w - 2.0).abs() < 1e-9);
    assert!((at_two.rect.h / at_one.rect.h - 2.0).abs() < 1e-9);
    // Aspect is the thing that must not move.
    assert!(
        (at_two.rect.w / at_two.rect.h - at_one.rect.w / at_one.rect.h).abs() < 1e-9,
        "zoom changed the aspect",
    );
}

#[test]
fn zoom_is_clamped_into_range_from_disk_and_from_a_drag() {
    for (given, want) in [(0.0, MIN_ZOOM), (99.0, MAX_ZOOM), (f64::NAN, 1.0)] {
        let placed = Placement {
            offset: (0.0, 0.0),
            zoom: given,
        }
        .sane();
        assert_eq!(placed.zoom, want, "zoom {given} should clamp to {want}");
    }
}

/// The headline behaviour: move the parent, the child comes with it.
#[test]
fn moving_the_parent_carries_the_child() {
    let g = roomy();
    let (base_h, base_v) = (base_size(SPLIT_H, &g), base_size(SPLIT_V, &g));
    let child = Placement {
        offset: (0.25, 0.10),
        zoom: 1.0,
    };

    let before_parent = resolve(base_h, Placement::default(), None, &g);
    let before = resolve(base_v, child, Some(&before_parent), &g);

    let moved_parent = resolve(
        base_h,
        Placement {
            offset: (300.0, 200.0),
            zoom: 1.0,
        },
        None,
        &g,
    );
    let after = resolve(base_v, child, Some(&moved_parent), &g);

    assert_eq!(
        (after.rect.x - before.rect.x, after.rect.y - before.rect.y),
        (300.0, 200.0),
        "the child did not travel with its parent",
    );
    assert_eq!((after.rect.w, after.rect.h), (before.rect.w, before.rect.h));
}

/// And zoom the parent, the child scales with it — position *and* size, or
/// the relationship the operator framed drifts apart.
#[test]
fn zooming_the_parent_scales_the_child() {
    let g = roomy();
    let (base_h, base_v) = (base_size(SPLIT_H, &g), base_size(SPLIT_V, &g));
    let child = Placement {
        offset: (0.25, 0.10),
        zoom: 1.0,
    };

    let parent_1x = resolve(base_h, Placement::default(), None, &g);
    let child_1x = resolve(base_v, child, Some(&parent_1x), &g);
    let parent_2x = resolve(
        base_h,
        Placement {
            offset: (0.0, 0.0),
            zoom: 2.0,
        },
        None,
        &g,
    );
    let child_2x = resolve(base_v, child, Some(&parent_2x), &g);

    assert!((child_2x.rect.w / child_1x.rect.w - 2.0).abs() < 1e-9);
    assert!((child_2x.rect.h / child_1x.rect.h - 2.0).abs() < 1e-9);
    // The child's offset from the parent doubled too, because it is
    // normalized to a parent that doubled.
    assert!(
        ((child_2x.rect.x - parent_2x.rect.x) / (child_1x.rect.x - parent_1x.rect.x) - 2.0)
            .abs()
            < 1e-9,
        "the child kept a fixed point offset instead of a proportional one",
    );
}

/// The child is genuinely allowed outside its parent — at 1:1 it is 200
/// pixels taller, so any rule that clipped it to the parent would silently
/// change its aspect.
#[test]
fn a_child_may_hang_outside_its_parent() {
    let g = roomy();
    let (base_h, base_v) = (base_size(SPLIT_H, &g), base_size(SPLIT_V, &g));
    let parent = resolve(base_h, Placement::default(), None, &g);
    let child = resolve(base_v, Placement::default(), Some(&parent), &g);
    assert!(
        child.rect.h > parent.rect.h,
        "split-vertical's slot is taller than split-horizontal's; a fixture \
         where it is not proves nothing",
    );
    assert!((child.rect.w / child.rect.h - SPLIT_V.w as f64 / SPLIT_V.h as f64).abs() < 1e-9);
}

#[test]
fn a_root_placement_round_trips_through_a_rect() {
    let g = roomy();
    let base = base_size(SPLIT_H, &g);
    let original = Placement {
        offset: (120.0, 64.0),
        zoom: 1.5,
    };
    let resolved = resolve(base, original, None, &g);
    let back = Placement::from_rect(&resolved.rect, base, None);
    assert!((back.offset.0 - original.offset.0).abs() < 1e-9);
    assert!((back.offset.1 - original.offset.1).abs() < 1e-9);
    assert!((back.zoom - original.zoom).abs() < 1e-9);
}

/// The one that matters for dragging: a child dropped somewhere must come
/// back as a placement that resolves to where it was dropped, *and* still
/// track the parent afterwards.
#[test]
fn a_child_placement_round_trips_and_still_follows() {
    let g = roomy();
    let (base_h, base_v) = (base_size(SPLIT_H, &g), base_size(SPLIT_V, &g));
    let parent = resolve(
        base_h,
        Placement {
            offset: (100.0, 80.0),
            zoom: 1.25,
        },
        None,
        &g,
    );

    // Pretend a drag left the child here.
    let dropped = PointRect {
        x: 400.0,
        y: 300.0,
        w: base_v.0 * 1.25,
        h: base_v.1 * 1.25,
    };
    let placement = Placement::from_rect(&dropped, base_v, Some(&parent));
    let resolved = resolve(base_v, placement, Some(&parent), &g);
    assert!((resolved.rect.x - dropped.x).abs() < 1e-6);
    assert!((resolved.rect.y - dropped.y).abs() < 1e-6);
    assert!((resolved.rect.w - dropped.w).abs() < 1e-6);

    // A child that was dropped at its parent's scale has a local zoom of
    // 1.0, so moving the parent moves it rigidly.
    assert!((placement.zoom - 1.0).abs() < 1e-9);
    let moved_parent = resolve(
        base_h,
        Placement {
            offset: (500.0, 80.0),
            zoom: 1.25,
        },
        None,
        &g,
    );
    let followed = resolve(base_v, placement, Some(&moved_parent), &g);
    assert!((followed.rect.x - (dropped.x + 400.0)).abs() < 1e-6);
}

/// The drag floor and the zoom floor have to be the same number, or a
/// resize dragged to its minimum springs to a different size the instant
/// the mouse is released: `resized` stops at one width, `from_rect` derives
/// a zoom below `MIN_ZOOM`, and `sane` clamps it back up.
#[test]
fn a_resize_dragged_to_its_floor_commits_without_jumping() {
    use crate::region::Corner;

    let g = geom();
    for output in [SPLIT_H, SPLIT_V] {
        let base = base_size(output, &g);
        let start = resolve(base, Placement::default(), None, &g);
        let floor = base.0 * MIN_ZOOM;

        // Drag the top-left corner right onto the anchor — as small as the
        // gesture can ask for.
        let anchor = Corner::BottomRight.at(&start.rect);
        let dragged = start.rect.resized(Corner::TopLeft, anchor, floor, &g);

        let committed = Placement::from_rect(&dragged, base, None);
        let settled = resolve(base, committed, None, &g);
        assert!(
            (settled.rect.w - dragged.w).abs() < 1e-6,
            "{output:?}: released at {:.3}pt but settled at {:.3}pt",
            dragged.w,
            settled.rect.w,
        );
    }
}

#[test]
fn a_region_too_big_for_the_display_shrinks_without_changing_shape() {
    let g = geom();
    let base = base_size(SPLIT_H, &g);
    let resolved = resolve(
        base,
        Placement {
            offset: (0.0, 0.0),
            zoom: MAX_ZOOM,
        },
        None,
        &g,
    );
    assert!(resolved.clamped, "4x on a 1512x982 display must not fit");
    assert!(resolved.rect.w <= g.points.0 + 1e-9);
    assert!(resolved.rect.h <= g.points.1 + 1e-9);
    assert!(
        (resolved.rect.w / resolved.rect.h - SPLIT_H.w as f64 / SPLIT_H.h as f64).abs() < 1e-9,
        "the fit-shrink changed the aspect, which is worse than being soft",
    );
}

#[test]
fn a_resolved_region_never_hangs_off_the_display() {
    let g = geom();
    let base = base_size(SPLIT_V, &g);
    let resolved = resolve(
        base,
        Placement {
            offset: (10_000.0, -10_000.0),
            zoom: 1.0,
        },
        None,
        &g,
    );
    assert!(resolved.rect.x >= 0.0 && resolved.rect.x + resolved.rect.w <= g.points.0 + 1e-9);
    assert!(resolved.rect.y >= 0.0 && resolved.rect.y + resolved.rect.h <= g.points.1 + 1e-9);
}
