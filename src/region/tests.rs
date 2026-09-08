//! Unit tests for the coordinate primitives and the resize gesture.
//!
//! Every one runs with no display, no permission prompt and no main thread:
//! this module is arithmetic, and the whole point of separating it from the
//! AppKit and ScreenCaptureKit call sites is that it can be proved on its own.
use super::*;

/// A 1512x982 point primary at 2x, the shape of a 14" MacBook Pro in its
/// default scaled mode.
fn primary() -> DisplayGeometry {
    DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (1512.0, 982.0),
        pixels: (3024, 1964),
        primary_height_points: 982.0,
    }
}

/// A 1920x1080 point 1x display placed to the *left* of the primary and
/// top-aligned with it — so its CG origin x is negative, its AppKit origin
/// y is negative, and its height differs from the primary's.
///
/// Every one of those three facts is load-bearing: a conversion that works
/// on a single laptop screen can be wrong in three independent ways and
/// still look correct until this fixture.
fn secondary() -> DisplayGeometry {
    DisplayGeometry {
        cg_origin: (-1920.0, 0.0),
        points: (1920.0, 1080.0),
        pixels: (1920, 1080),
        primary_height_points: 982.0,
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// The overlay window's own placement: the whole primary display maps to
/// the whole primary display, origin at AppKit's (0, 0).
#[test]
fn a_full_display_rect_places_the_overlay_over_that_display() {
    let geom = primary();
    let full = PointRect {
        x: 0.0,
        y: 0.0,
        w: geom.points.0,
        h: geom.points.1,
    };
    assert_eq!(full.to_appkit(&geom), rect(0.0, 0.0, 1512.0, 982.0));
}

/// The test the whole module exists for.
///
/// A 300-tall rect sitting 50 points below the secondary's top edge must
/// place at AppKit y = 632, because the flip is against the **primary's**
/// 982-point height: 982 - (0 + 50) - 300. Flipping against the secondary's
/// own 1080 would give 730. On a single-monitor machine the two are the
/// same number and a wrong implementation looks right, which is exactly why
/// this fixture has a second display of a different height sitting at a
/// negative x.
#[test]
fn the_y_flip_uses_the_primary_height_not_the_displays_own() {
    let geom = secondary();
    let local = PointRect {
        x: 100.0,
        y: 50.0,
        w: 400.0,
        h: 300.0,
    };
    let placed = local.to_appkit(&geom);
    assert_eq!(placed.origin.y, 632.0, "flipped against the wrong height");
    assert_eq!(
        placed.origin.x, -1820.0,
        "did not add the display's CG origin, so the overlay would land on \
         the primary instead of the secondary",
    );
    assert_eq!(placed.size.width, 400.0);
    assert_eq!(placed.size.height, 300.0);

    let wrong = geom.points.1 - (local.y + local.h);
    assert_ne!(
        wrong, placed.origin.y,
        "fixture is useless: the two screen heights must disagree",
    );
}

/// The figure path's conversion: display-local to Core Graphics global.
///
/// Same fixture, and the assertion that matters is what it *does not* do. Both
/// spaces have y growing down, so the y here is 50 — the rect's own offset plus
/// the display's origin — and never the 632 the AppKit flip produces. Handing
/// `SCScreenshotManager` a flipped y captures a band from the other end of the
/// screen, which is a wrong picture rather than an error.
#[test]
fn the_cg_global_conversion_adds_the_origin_and_does_not_flip() {
    let geom = secondary();
    let local = PointRect {
        x: 100.0,
        y: 50.0,
        w: 400.0,
        h: 300.0,
    };
    let global = local.to_cg_global(&geom);
    assert_eq!(global, rect(-1820.0, 50.0, 400.0, 300.0));
    assert_ne!(
        global.origin.y,
        local.to_appkit(&geom).origin.y,
        "the two conversions have collapsed into one",
    );
}

/// On the primary, where the origin is (0, 0), the rect passes through — which
/// is the case that makes a broken conversion invisible on one screen.
#[test]
fn a_rect_on_the_primary_converts_to_itself() {
    let geom = primary();
    let local = PointRect {
        x: 200.0,
        y: 150.0,
        w: 600.0,
        h: 400.0,
    };
    assert_eq!(local.to_cg_global(&geom), rect(200.0, 150.0, 600.0, 400.0));
}

/// Placement never touches the size, whichever display it is on.
#[test]
fn placing_a_rect_preserves_its_size() {
    for geom in [primary(), secondary()] {
        let local = PointRect {
            x: 10.0,
            y: 20.0,
            w: 701.0,
            h: 540.0,
        };
        let placed = local.to_appkit(&geom);
        assert_eq!((placed.size.width, placed.size.height), (local.w, local.h));
    }
}

#[test]
fn source_rect_is_display_local_and_untouched_by_the_flip() {
    let local = PointRect {
        x: 100.0,
        y: 50.0,
        w: 400.0,
        h: 300.0,
    };
    let cg = local.to_source_rect();
    assert_eq!(cg.origin.x, 100.0);
    assert_eq!(cg.origin.y, 50.0);
    assert_eq!(cg.size.width, 400.0);
    assert_eq!(cg.size.height, 300.0);
}

#[test]
fn moving_clamps_at_all_four_edges_and_never_resizes() {
    let geom = primary();
    let region = PointRect::centered((700.0, 540.0), &geom);
    let far = 10_000.0;
    for origin in [(-far, -far), (far, -far), (-far, far), (far, far)] {
        let moved = region.moved_to(origin, &geom);
        assert_eq!((moved.w, moved.h), (region.w, region.h), "a move resized");
        assert!(moved.x >= 0.0 && moved.x + moved.w <= geom.points.0);
        assert!(moved.y >= 0.0 && moved.y + moved.h <= geom.points.1);
    }
    assert_eq!(
        region.moved_to((0.0, 0.0), &geom),
        PointRect {
            x: 0.0,
            y: 0.0,
            w: 700.0,
            h: 540.0
        }
    );
}

#[test]
fn moving_a_region_larger_than_its_display_pins_it_at_the_origin() {
    // A stale saved origin can outlive the display it was authored on; the
    // clamp must not panic on an inverted range.
    let geom = primary();
    let oversized = PointRect {
        x: 0.0,
        y: 0.0,
        w: 4000.0,
        h: 3000.0,
    };
    let moved = oversized.moved_to((500.0, 500.0), &geom);
    assert_eq!((moved.x, moved.y), (0.0, 0.0));
}

#[test]
fn even_rounds_to_the_nearest_even_rather_than_truncating() {
    assert_eq!(even(1401.6), 1402);
    assert_eq!(even(1402.4), 1402);
    // The case round-then-clear-the-low-bit gets wrong: from 1403.4, 1404 is
    // 0.6 away and 1402 is 1.4.
    assert_eq!(even(1403.4), 1404);
    assert_eq!(even(1402.562), 1402, "split-horizontal's slot rounds down");
    assert_eq!(even(0.0), 2);
    assert_eq!(even(-5.0), 2);
    assert_eq!(even(f64::NAN), 2);
}

#[test]
fn contains_is_half_open_so_touching_regions_do_not_both_claim_a_point() {
    let region = PointRect {
        x: 10.0,
        y: 20.0,
        w: 100.0,
        h: 50.0,
    };
    assert!(region.contains((10.0, 20.0)));
    assert!(region.contains((109.9, 69.9)));
    assert!(!region.contains((110.0, 45.0)));
    assert!(!region.contains((50.0, 70.0)));
    assert!(!region.contains((9.9, 45.0)));
}

/// Split-Horizontal's shape, sitting well inside a roomy display so the
/// resize tests are about the gesture rather than about clamping.
fn resizable() -> (PointRect, DisplayGeometry) {
    let geom = DisplayGeometry {
        cg_origin: (0.0, 0.0),
        points: (4000.0, 4000.0),
        pixels: (4000, 4000),
        primary_height_points: 4000.0,
    };
    (
        PointRect {
            x: 1000.0,
            y: 1000.0,
            w: 1402.562,
            h: 1080.0,
        },
        geom,
    )
}

/// The property the whole feature rests on: a region may change size, never
/// shape. An aspect that drifts is silently trimmed by `object-fit: cover`
/// in the render, and nothing before the edit would show it.
#[test]
fn resizing_from_any_corner_preserves_the_aspect() {
    let (region, geom) = resizable();
    let want = region.w / region.h;
    for corner in Corner::ALL {
        for point in [(1200.0, 1150.0), (2900.0, 2600.0), (1500.0, 3000.0)] {
            let resized = region.resized(corner, point, 0.0, &geom);
            assert!(
                (resized.w / resized.h - want).abs() < 1e-9,
                "{corner:?} dragged to {point:?} changed the aspect: \
                 {want} -> {}",
                resized.w / resized.h,
            );
        }
    }
}

/// The corner you are not holding must not move, or the region slides
/// around under the cursor instead of scaling.
#[test]
fn resizing_holds_the_opposite_corner_still() {
    let (region, geom) = resizable();
    for corner in Corner::ALL {
        let anchor = corner.opposite().at(&region);
        let resized = region.resized(corner, (1800.0, 1600.0), 0.0, &geom);
        let moved = corner.opposite().at(&resized);
        assert!(
            (moved.0 - anchor.0).abs() < 1e-9 && (moved.1 - anchor.1).abs() < 1e-9,
            "{corner:?}: the anchor moved from {anchor:?} to {moved:?}",
        );
    }
}

#[test]
fn resizing_follows_whichever_axis_the_drag_moved_further() {
    let (region, geom) = resizable();
    let anchor = Corner::BottomRight.at(&region);
    // A drag that is mostly vertical still grows the region, which it
    // would not if only the x delta were read.
    let tall = region.resized(
        Corner::TopLeft,
        (anchor.0 - 1500.0, anchor.1 - 3000.0),
        0.0,
        &geom,
    );
    assert!(tall.h > region.h, "a vertical drag did nothing");
    let wide = region.resized(
        Corner::TopLeft,
        (anchor.0 - 3000.0, anchor.1 - 1500.0),
        0.0,
        &geom,
    );
    assert!(wide.w > region.w, "a horizontal drag did nothing");
}

/// Clamping happens on the *size*, before the rect is built. Clipping a
/// finished rect to the display would hand back the wrong shape, which is
/// the one outcome the aspect lock exists to prevent.
#[test]
fn resizing_past_the_display_edge_stops_at_the_edge_in_shape() {
    let (region, geom) = resizable();
    let want = region.w / region.h;
    let resized = region.resized(Corner::BottomRight, (99_999.0, 99_999.0), 0.0, &geom);
    assert!(resized.x >= -1e-9 && resized.x + resized.w <= geom.points.0 + 1e-9);
    assert!(resized.y >= -1e-9 && resized.y + resized.h <= geom.points.1 + 1e-9);
    assert!(
        (resized.w / resized.h - want).abs() < 1e-9,
        "the edge clamp changed the aspect",
    );
}

#[test]
fn resizing_to_nothing_stops_at_a_grabbable_size() {
    let (region, geom) = resizable();
    let anchor = Corner::BottomRight.at(&region);
    let resized = region.resized(Corner::TopLeft, anchor, 0.0, &geom);
    assert!(
        resized.w >= MIN_SIZE_POINTS - 1e-9,
        "a region collapsed to {}pt wide has no handle left to grab",
        resized.w,
    );
}

#[test]
fn opposite_corners_pair_up_and_round_trip() {
    for corner in Corner::ALL {
        assert_eq!(corner.opposite().opposite(), corner);
        assert_ne!(corner.opposite(), corner);
    }
}

/// A gnomon arm moves one coordinate and leaves the other exactly alone —
/// that constraint is the only reason the arm exists rather than just dragging
/// the body.
#[test]
fn an_axis_drag_moves_one_coordinate_and_pins_the_other() {
    let geom = primary();
    let region = PointRect::centered((700.0, 540.0), &geom);

    let x_moved = region.moved_on(Axis::X, (120.0, 999.0), &geom);
    assert_eq!(x_moved.x, 120.0);
    assert_eq!(x_moved.y, region.y, "an X drag moved y");

    let y_moved = region.moved_on(Axis::Y, (999.0, 60.0), &geom);
    assert_eq!(y_moved.y, 60.0);
    assert_eq!(y_moved.x, region.x, "a Y drag moved x");
}

#[test]
fn an_axis_drag_is_clamped_like_any_other_and_never_resizes() {
    let geom = primary();
    let region = PointRect::centered((700.0, 540.0), &geom);
    for axis in Axis::ALL {
        for target in [(-9999.0, -9999.0), (9999.0, 9999.0)] {
            let moved = region.moved_on(axis, target, &geom);
            assert_eq!((moved.w, moved.h), (region.w, region.h));
            assert!(moved.x >= 0.0 && moved.x + moved.w <= geom.points.0);
            assert!(moved.y >= 0.0 && moved.y + moved.h <= geom.points.1);
        }
    }
}

/// The gnomon is rooted at the centre, so the centre is what the arms are
/// measured from — and `center` is what both the drawing and the hit-testing
/// call.
#[test]
fn the_centre_is_the_middle_of_the_rect() {
    let region = PointRect {
        x: 100.0,
        y: 200.0,
        w: 400.0,
        h: 300.0,
    };
    assert_eq!(region.center(), (300.0, 350.0));
}

/// Edge-touching is not overlapping, which is what makes stacking the
/// overlay's two labels terminate: a plate pushed to sit exactly below another
/// is done, not still colliding.
#[test]
fn overlaps_is_half_open_on_both_axes() {
    let a = PointRect {
        x: 10.0,
        y: 20.0,
        w: 100.0,
        h: 50.0,
    };
    assert!(a.overlaps(&a));
    assert!(a.overlaps(&PointRect {
        x: 105.0,
        y: 65.0,
        w: 20.0,
        h: 20.0
    }));
    assert!(
        !a.overlaps(&PointRect {
            x: 10.0,
            y: 70.0,
            w: 100.0,
            h: 50.0
        }),
        "a rect stacked directly below must not count as overlapping",
    );
    assert!(!a.overlaps(&PointRect {
        x: 110.0,
        y: 20.0,
        w: 100.0,
        h: 50.0
    }));
    assert!(!a.overlaps(&PointRect {
        x: 200.0,
        y: 200.0,
        w: 10.0,
        h: 10.0
    }));
}

#[test]
fn union_covers_both_rects() {
    let a = PointRect {
        x: 10.0,
        y: 20.0,
        w: 100.0,
        h: 50.0,
    };
    let b = PointRect {
        x: 80.0,
        y: 10.0,
        w: 40.0,
        h: 80.0,
    };
    let u = a.union(&b);
    assert_eq!(u.x, 10.0);
    assert_eq!(u.y, 10.0);
    assert_eq!(u.w, 110.0);
    assert_eq!(u.h, 80.0);
}

#[test]
fn a_region_maps_into_the_union_buffer() {
    let outer = PointRect {
        x: 100.0,
        y: 200.0,
        w: 400.0,
        h: 200.0,
    };
    let inner = PointRect {
        x: 200.0,
        y: 200.0,
        w: 200.0,
        h: 200.0,
    };
    let crop = inner
        .in_buffer(&outer, PixelSize { w: 800, h: 400 })
        .unwrap();
    assert_eq!(crop, (200.0, 0.0, 400.0, 400.0));
}
