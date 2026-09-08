//! What the region overlay paints, and where its grab handles are.
//!
//! Split from the window and event handling in [`super`] because it is the only
//! part that talks to AppKit's drawing APIs, and because hit-testing a handle
//! and drawing one have to agree about exactly where it is — keeping
//! [`handle_rect`] as the single answer to that is what stops a handle you can
//! see from being one you cannot grab.
//!
//! Everything here runs inside `drawRect:` or a mouse handler, so the same
//! no-panic rule as [`super`] applies: index with `get`, never `[]`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSStringDrawing,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

use super::{DrawnRegion, OverlayState, HANDLE, HORIZONTAL, VERTICAL};
use crate::layouts::Orientation;
use crate::region::{Axis, Corner, PointRect};

/// How far each gnomon arrow reaches from the region's centre, in points.
///
/// Fixed rather than proportional to the region: it is a control, and a control
/// that grows with what it manipulates ends up either unusably small on a
/// tightly zoomed region or covering the whole frame on a loose one.
const ARM: f64 = 68.0;
/// Length of the arrowhead, measured back along the arm.
const HEAD: f64 = 15.0;
/// Half-thickness of an arm's grab area. Generous next to the 3pt line it
/// draws, because a 3pt hit target is a 3pt miss target.
const ARM_GRAB: f64 = 11.0;
/// Radius of the knob at the gnomon's root, which drags both axes at once.
const KNOB: f64 = 9.0;

/// Padding inside a label plate, and the gap between two stacked ones.
const LABEL_PAD: f64 = 6.0;
const LABEL_GAP: f64 = 4.0;

/// The conventional axis colours, so the gnomon reads as a gnomon on sight
/// rather than as decoration: X red, Y green.
const AXIS_X: (f64, f64, f64) = (0.905, 0.298, 0.235);
const AXIS_Y: (f64, f64, f64) = (0.180, 0.800, 0.443);

/// The colour that identifies one frame everywhere it is drawn — border,
/// corner handles, gnomon knob and label. See [`HORIZONTAL`].
fn frame_color(orientation: Orientation) -> (f64, f64, f64) {
    match orientation {
        Orientation::Horizontal => HORIZONTAL,
        Orientation::Vertical => VERTICAL,
    }
}

fn axis_color(axis: Axis) -> (f64, f64, f64) {
    match axis {
        Axis::X => AXIS_X,
        Axis::Y => AXIS_Y,
    }
}

/// Where one arm ends, from the region's centre.
///
/// Y is **negative** because the overlay view is `isFlipped`: display-local y
/// grows downward, so an arrow that points up on screen runs toward smaller y.
fn arm_tip(center: (f64, f64), axis: Axis) -> (f64, f64) {
    match axis {
        Axis::X => (center.0 + ARM, center.1),
        Axis::Y => (center.0, center.1 - ARM),
    }
}

/// The grab area for one arm: a band running from the edge of the knob out to
/// the tip.
///
/// Starting clear of the root is what keeps the two bands from overlapping each
/// other. Rooted at the centre they would share a square around it, and
/// `arm_at` — which has to answer with *one* axis — would return whichever it
/// happened to test first, so a drag straight up from just above the centre
/// would silently move sideways instead.
///
/// The inset is `max(KNOB, ARM_GRAB)`, not `KNOB`. A band is `ARM_GRAB` thick
/// either side of its axis, so insetting by less than that still leaves the two
/// clipping each other diagonally at the root — with the current 9pt knob and
/// 11pt grab, a 2x2 point square where both answer. Deriving the inset from
/// both constants means neither can be retuned into reintroducing it.
pub(super) fn arm_rect(region: &PointRect, axis: Axis) -> PointRect {
    let center = region.center();
    let inset = KNOB.max(ARM_GRAB);
    let reach = ARM - inset;
    match axis {
        Axis::X => PointRect {
            x: center.0 + inset,
            y: center.1 - ARM_GRAB,
            w: reach,
            h: ARM_GRAB * 2.0,
        },
        Axis::Y => PointRect {
            x: center.0 - ARM_GRAB,
            y: center.1 - ARM,
            w: ARM_GRAB * 2.0,
            h: reach,
        },
    }
}

/// The grab area for the knob at the gnomon's root.
pub(super) fn knob_rect(region: &PointRect) -> PointRect {
    let center = region.center();
    PointRect {
        x: center.0 - KNOB,
        y: center.1 - KNOB,
        w: KNOB * 2.0,
        h: KNOB * 2.0,
    }
}

/// Which gnomon arm `point` is on, if any.
///
/// Unambiguous by construction: the bands start outside the knob and run along
/// different axes, so no point is on both.
pub(super) fn arm_at(region: &PointRect, point: (f64, f64)) -> Option<Axis> {
    Axis::ALL
        .into_iter()
        .find(|axis| arm_rect(region, *axis).contains(point))
}

pub(super) fn color(rgb: (f64, f64, f64), alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(rgb.0, rgb.1, rgb.2, alpha)
}

pub(super) fn rect(r: &PointRect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

/// The square grab area centred on one corner of `region`.
pub(super) fn handle_rect(region: &PointRect, corner: Corner) -> PointRect {
    let (x, y) = corner.at(region);
    PointRect {
        x: x - HANDLE / 2.0,
        y: y - HANDLE / 2.0,
        w: HANDLE,
        h: HANDLE,
    }
}

/// Which corner handle `point` is inside, if any.
pub(super) fn corner_at(region: &PointRect, point: (f64, f64)) -> Option<Corner> {
    Corner::ALL
        .into_iter()
        .find(|corner| handle_rect(region, *corner).contains(point))
}

/// Everything the overlay paints, in one pass over the state.
pub(super) fn draw(state: &OverlayState) {
    let (display_w, display_h) = state.geometry.points;
    let active = state.regions.get(state.active);

    // Dim outside the active region, as four rects rather than an even-odd
    // path: the four are trivially correct and cost one fill each.
    if let Some(active) = active {
        // The rect actually being recorded, which under mouse tracking is a
        // sub-rect of the authored region. Dimming to the authored one instead
        // would leave the un-dimmed area showing more than the file contains.
        let r = &state.recording_rect(active);
        color((0.0, 0.0, 0.0), 0.35).set();
        for band in [
            PointRect {
                x: 0.0,
                y: 0.0,
                w: display_w,
                h: r.y,
            },
            PointRect {
                x: 0.0,
                y: r.y + r.h,
                w: display_w,
                h: (display_h - r.y - r.h).max(0.0),
            },
            PointRect {
                x: 0.0,
                y: r.y,
                w: r.x,
                h: r.h,
            },
            PointRect {
                x: r.x + r.w,
                y: r.y,
                w: (display_w - r.x - r.w).max(0.0),
                h: r.h,
            },
        ] {
            if band.w > 0.0 && band.h > 0.0 {
                NSBezierPath::fillRect(rect(&band));
            }
        }
    }

    // Painted inactive-first so the active frame's controls sit *on top* of any
    // they coincide with. `grab_anywhere` breaks a tie the same way, and at the
    // default placement the two top-left handles are exactly coincident — a
    // handle drawn over another one you cannot grab is the one thing this
    // overlay must never show.
    let order = (0..state.regions.len())
        .filter(|index| *index != state.active)
        .chain(std::iter::once(state.active));
    for index in order {
        let Some(region) = state.regions.get(index) else {
            continue;
        };
        let is_active = index == state.active;
        let rgb = frame_color(region.orientation);
        // Dim the frame that is not being recorded, but keep it *its own
        // colour*: the colour is the frame's identity, so "not this chapter" is
        // said with the dash and the alpha instead.
        let alpha = if is_active { 1.0 } else { 0.7 };

        let framed = state.recording_rect(region);

        // The authored region, solid: this is the frame the operator placed,
        // and tracking never moves it. Solid when it is the recorded
        // orientation, dashed when it is not — the existing convention, kept.
        let path = NSBezierPath::bezierPathWithRect(rect(&region.rect));
        color(rgb, alpha).set();
        if is_active {
            path.setLineWidth(3.0);
        } else {
            path.setLineWidth(2.0);
            let pattern = [8.0f64, 6.0];
            unsafe { path.setLineDash_count_phase(pattern.as_ptr(), 2, 0.0) };
        }
        path.stroke();

        // The punched-in view inside it, dotted — what is actually in the file
        // while the combo is held. Drawn only when it differs, so an unheld
        // frame looks exactly as it did before this feature.
        //
        // Fine dots (2 on, 4 off) rather than the inactive frame's long dashes
        // (8 on, 6 off): both are broken lines, and if they read alike then
        // "this is the other orientation" and "this is the zoom" become the
        // same signal at a glance. Same colour and alpha as its own region, so
        // the pairing stays obvious.
        if framed != region.rect {
            let zoomed = NSBezierPath::bezierPathWithRect(rect(&framed));
            color(rgb, alpha).set();
            zoomed.setLineWidth(2.0);
            let pattern = [2.0f64, 4.0];
            unsafe { zoomed.setLineDash_count_phase(pattern.as_ptr(), 2, 0.0) };
            zoomed.stroke();
        }

        // Handles and gnomon on every region, not just the active one: both
        // frames are moved *and* resized in the same pass, and a control that
        // is only drawn on the recorded frame reads as though the other one
        // were locked.
        //
        // On the **authored** region, never on the punched-in view. That is
        // what makes them safe to keep while tracking is running: the frame
        // being dragged is the one that does not move, so the knob stays where
        // the operator left it and cannot end up parked under the cursor. A
        // gesture that moved the frame *to* the pointer — a dwell that punched
        // in wherever you paused — could not keep these, because it would put
        // a grab target under the cursor permanently and the overlay would
        // swallow the click it was waiting for.
        draw_handles(&region.rect, rgb, alpha);
        draw_gnomon(&region.rect, rgb, is_active);
    }

    // Labels last, and in orientation order rather than paint order: they are
    // opaque plates, so drawing them inside the loop above would bury one
    // frame's name under the other frame's border, and a name that moves
    // between the two positions whenever the recorded layout changes is one
    // more thing to re-read.
    let mut placed: Vec<PointRect> = Vec::with_capacity(state.regions.len());
    for (index, region) in state.regions.iter().enumerate() {
        draw_label(region, index == state.active, &mut placed);
    }
}

/// The gnomon: an X arm pointing right, a Y arm pointing up, and a knob at
/// their root, all centred on the region.
///
/// Dragging an arm moves the region along that axis alone; dragging the knob
/// moves it freely. That constraint is the point — nudging a region's vertical
/// framing without disturbing a horizontal position you already settled is not
/// something a body-drag can do.
fn draw_gnomon(region: &PointRect, rgb: (f64, f64, f64), is_active: bool) {
    let center = region.center();
    // Inactive gnomons are dimmed rather than hidden: the other orientation is
    // still draggable, and a control you cannot see is a control nobody finds.
    let alpha = if is_active { 1.0 } else { 0.6 };

    for axis in Axis::ALL {
        let tip = arm_tip(center, axis);
        color(axis_color(axis), alpha).set();

        // The shaft stops short of the tip so the arrowhead is a solid
        // triangle rather than a triangle with a line up its middle.
        let shaft = NSBezierPath::bezierPath();
        shaft.moveToPoint(NSPoint::new(center.0, center.1));
        let (bx, by) = match axis {
            Axis::X => (tip.0 - HEAD, tip.1),
            Axis::Y => (tip.0, tip.1 + HEAD),
        };
        shaft.lineToPoint(NSPoint::new(bx, by));
        shaft.setLineWidth(3.0);
        shaft.stroke();

        let head = NSBezierPath::bezierPath();
        head.moveToPoint(NSPoint::new(tip.0, tip.1));
        match axis {
            Axis::X => {
                head.lineToPoint(NSPoint::new(bx, by - HEAD * 0.42));
                head.lineToPoint(NSPoint::new(bx, by + HEAD * 0.42));
            }
            Axis::Y => {
                head.lineToPoint(NSPoint::new(bx - HEAD * 0.42, by));
                head.lineToPoint(NSPoint::new(bx + HEAD * 0.42, by));
            }
        }
        head.closePath();
        head.fill();
    }

    // Knob last, so it sits over both shafts and looks like their root.
    let knob = knob_rect(region);
    color((1.0, 1.0, 1.0), alpha).set();
    NSBezierPath::bezierPathWithOvalInRect(rect(&knob)).fill();
    // Ringed in the frame's own colour, so the free-move handle says which
    // frame it will move even where the two gnomons sit on top of each other.
    color(rgb, alpha).set();
    let ring = NSBezierPath::bezierPathWithOvalInRect(rect(&knob));
    ring.setLineWidth(2.5);
    ring.stroke();
}

/// Filled corner squares in the frame's own colour, so it reads as resizable
/// rather than as a rectangle that happens to be highlighted.
///
/// Drawn on both frames. `grab_anywhere` has always offered every region's
/// corners — the vertical frame was resizable while showing no sign of it, and
/// an invisible handle is one nobody reaches for.
fn draw_handles(region: &PointRect, rgb: (f64, f64, f64), alpha: f64) {
    for corner in Corner::ALL {
        let handle = handle_rect(region, corner);
        color(rgb, alpha).set();
        NSBezierPath::fillRect(rect(&handle));
        // A white inset keeps the handle visible against dark screen content,
        // which the dimming outside the region makes more likely, not less.
        color((1.0, 1.0, 1.0), 0.9 * alpha).set();
        NSBezierPath::fillRect(rect(&PointRect {
            x: handle.x + 3.0,
            y: handle.y + 3.0,
            w: handle.w - 6.0,
            h: handle.h - 6.0,
        }));
    }
}

/// Where one label plate goes: just inside its region's top-left corner,
/// pushed down past any plate already placed.
///
/// The push is not cosmetic. Split-Vertical's default placement sits it exactly
/// on Split-Horizontal's top-left corner, so both labels want the same point
/// and one would be painted straight over the other — in the state the overlay
/// opens in, which is precisely when telling the two frames apart matters most.
pub(super) fn label_plate(region: &PointRect, size: (f64, f64), placed: &[PointRect]) -> PointRect {
    // Clear of the corner handle: the plate is opaque, and a label covering a
    // handle hides the control it is naming.
    let mut plate = PointRect {
        x: region.x + HANDLE / 2.0 + 4.0,
        y: region.y + HANDLE / 2.0 + 2.0,
        w: size.0 + LABEL_PAD * 2.0,
        h: size.1 + LABEL_PAD,
    };
    // One shift per plate already down is always enough, and the loop is
    // bounded regardless: this runs inside `drawRect:`, where a loop that fails
    // to terminate takes the recorder with it.
    for _ in 0..placed.len() {
        if !placed.iter().any(|other| other.overlaps(&plate)) {
            break;
        }
        plate.y += plate.h + LABEL_GAP;
    }
    plate
}

/// `● Horizontal · 1402×1080 · 1.00×` on a dark plate just inside the region's
/// top-left corner, in the frame's own colour. The dot marks the frame this
/// chapter is recording.
///
/// The plate is what makes the name readable: the text sits over whatever is
/// being demonstrated, and orange-on-white or blue-on-white is a name you have
/// to hunt for. Colour on a dark chip reads over any content, and reading the
/// name is the entire job.
fn draw_label(region: &DrawnRegion, is_active: bool, placed: &mut Vec<PointRect>) {
    let text = NSString::from_str(&format!(
        "{}{} · {}×{} · {:.2}×",
        if is_active { "● " } else { "" },
        region.orientation.as_str(),
        region.pixels.0,
        region.pixels.1,
        region.zoom,
    ));
    let font = NSFont::boldSystemFontOfSize(15.0);
    let foreground = color(
        frame_color(region.orientation),
        if is_active { 1.0 } else { 0.8 },
    );
    let attributes: Retained<NSDictionary<NSString, AnyObject>> = unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[
                font.as_ref() as &AnyObject,
                foreground.as_ref() as &AnyObject,
            ],
        )
    };

    // Measured rather than assumed: the plate has to fit whatever the system
    // font makes of this string, and a plate sized from a guess either clips
    // the name or floats around it.
    let size = unsafe { text.sizeWithAttributes(Some(&attributes)) };
    // Inside the border rather than above it, so a region dragged to the top
    // edge of the display does not put its own label off-screen.
    let plate = label_plate(&region.rect, (size.width, size.height), placed);

    color((0.05, 0.05, 0.05), if is_active { 0.82 } else { 0.62 }).set();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(&plate), 5.0, 5.0).fill();
    unsafe {
        text.drawAtPoint_withAttributes(
            NSPoint::new(plate.x + LABEL_PAD, plate.y + LABEL_PAD / 2.0),
            Some(&attributes),
        )
    };
    placed.push(plate);
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A region big enough that its corners are nowhere near its gnomon.
    fn region() -> PointRect {
        PointRect {
            x: 200.0,
            y: 200.0,
            w: 700.0,
            h: 540.0,
        }
    }

    /// The trap this whole module has to get right. The overlay view is
    /// `isFlipped`, so "up on screen" is *decreasing* y — an arm built toward
    /// +y would draw downward and grab the wrong half, while still looking
    /// plausible in the code.
    #[test]
    fn the_y_arm_points_up_the_screen_not_down() {
        let r = region();
        let (cx, cy) = r.center();
        assert_eq!(
            arm_at(&r, (cx, cy - 40.0)),
            Some(Axis::Y),
            "the Y arm must be reachable above the centre",
        );
        assert_eq!(
            arm_at(&r, (cx, cy + 40.0)),
            None,
            "nothing should be grabbable below the centre — that is where an \
             arm built toward +y would land",
        );
    }

    #[test]
    fn the_x_arm_points_right() {
        let r = region();
        let (cx, cy) = r.center();
        assert_eq!(arm_at(&r, (cx + 40.0, cy)), Some(Axis::X));
        assert_eq!(arm_at(&r, (cx - 40.0, cy)), None);
    }

    /// No point may be on both arms, or `arm_at` — which answers with one
    /// axis — would pick by iteration order and a drag straight up from near
    /// the centre would move sideways instead.
    #[test]
    fn the_two_arms_never_overlap() {
        let r = region();
        let (cx, cy) = r.center();
        for dx in -80..=80 {
            for dy in -80..=80 {
                let p = (cx + f64::from(dx), cy + f64::from(dy));
                let on_x = arm_rect(&r, Axis::X).contains(p);
                let on_y = arm_rect(&r, Axis::Y).contains(p);
                assert!(!(on_x && on_y), "{p:?} is on both arms");
            }
        }
    }

    /// The knob owns the square at the root and means "no constraint", so
    /// neither arm may reach into it — otherwise the free-move target would be
    /// partly shadowed by a constrained one.
    #[test]
    fn the_knob_is_clear_of_both_arms() {
        let r = region();
        let (cx, cy) = r.center();
        for dx in -12..=12 {
            for dy in -12..=12 {
                let p = (cx + f64::from(dx), cy + f64::from(dy));
                if knob_rect(&r).contains(p) {
                    assert_eq!(arm_at(&r, p), None, "{p:?} is in the knob and on an arm");
                }
            }
        }
        assert_eq!(arm_at(&r, (cx, cy)), None, "the exact centre is the knob's");
    }

    /// And why the arms are tested before the body: an arm lies inside the
    /// region, so body-first would swallow every constrained drag.
    #[test]
    fn the_arms_lie_inside_the_region_they_control() {
        let r = region();
        let (cx, cy) = r.center();
        assert!(r.contains((cx + 40.0, cy)));
        assert!(r.contains((cx, cy - 40.0)));
    }

    #[test]
    fn each_arm_stops_at_its_own_length() {
        let r = region();
        let (cx, cy) = r.center();
        assert_eq!(arm_at(&r, (cx + ARM - 1.0, cy)), Some(Axis::X));
        assert_eq!(arm_at(&r, (cx + ARM + 1.0, cy)), None);
        assert_eq!(arm_at(&r, (cx, cy - ARM + 1.0)), Some(Axis::Y));
        assert_eq!(arm_at(&r, (cx, cy - ARM - 1.0)), None);
    }

    /// The state the overlay opens in: Split-Vertical sits on
    /// Split-Horizontal's top-left corner, so both labels want the same point.
    /// Painted there, one frame's name would be invisible under the other's —
    /// with two identically placed rectangles on screen and nothing left saying
    /// which was which.
    #[test]
    fn two_labels_at_the_same_origin_are_stacked_not_overlaid() {
        let shared = PointRect {
            x: 400.0,
            y: 200.0,
            w: 700.0,
            h: 540.0,
        };
        let size = (240.0, 18.0);

        let first = label_plate(&shared, size, &[]);
        let second = label_plate(&shared, size, &[first]);

        assert!(
            !first.overlaps(&second),
            "the second label was painted over the first: {first:?} vs {second:?}",
        );
        assert_eq!(first.x, second.x, "stacked labels should stay left-aligned");
        assert!(
            second.y > first.y,
            "the second label should go below the first"
        );
        assert!(
            second.y + second.h < shared.y + shared.h,
            "both labels must stay inside the region they name",
        );
    }

    /// A label may not cover a corner handle: the plate is opaque, so a name
    /// drawn over the top-left handle hides the control it is naming.
    #[test]
    fn a_label_clears_the_corner_handle() {
        let r = region();
        let plate = label_plate(&r, (240.0, 18.0), &[]);
        assert!(
            !plate.overlaps(&handle_rect(&r, Corner::TopLeft)),
            "the label plate covers the top-left handle",
        );
    }

    /// The corner handles must not sit under the gnomon, or resizing a small
    /// region becomes unreachable behind a control that never shrinks.
    #[test]
    fn the_gnomon_does_not_cover_the_corner_handles() {
        let r = region();
        for corner in Corner::ALL {
            let handle = handle_rect(&r, corner);
            let (hx, hy) = (handle.x + handle.w / 2.0, handle.y + handle.h / 2.0);
            assert_eq!(
                arm_at(&r, (hx, hy)),
                None,
                "{corner:?}'s handle is buried under a gnomon arm",
            );
            assert!(!knob_rect(&r).contains((hx, hy)));
        }
    }
}
