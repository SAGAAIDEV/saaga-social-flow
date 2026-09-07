//! What the snip overlay paints.
//!
//! Split from the window and the gesture in [`super`] for the same reason
//! `overlay::draw` is split from its own: this is the only part that talks to
//! AppKit's drawing APIs, and keeping it apart means the arithmetic deciding
//! *what* is selected can be read without the code that colours it in.
//!
//! Everything here runs inside `drawRect:`, so the same no-panic rule as
//! [`super`] applies: borrow with `try_borrow`, and never unwrap.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSStringDrawing,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

use super::{SnipState, DIM};
use crate::region::{DisplayGeometry, PointRect};

/// Everything the overlay paints: the dim, the selection, and one line of help.
pub(super) fn draw(state: &SnipState) {
    let full = PointRect {
        x: 0.0,
        y: 0.0,
        w: state.geometry.points.0,
        h: state.geometry.points.1,
    };
    match state.selection().filter(|rect| rect.w > 0.0 && rect.h > 0.0) {
        Some(selection) => {
            dim_around(&full, &selection);
            outline(&selection);
            size_label(&selection, &state.geometry);
        }
        None => {
            shade(&full, DIM);
            guides(state);
            hint(&full);
        }
    }
}

/// The dim, painted as the four bands around `hole` so the selection is shown
/// at full brightness. See the module docs.
fn dim_around(full: &PointRect, hole: &PointRect) {
    let above = PointRect { x: full.x, y: full.y, w: full.w, h: hole.y - full.y };
    let below = PointRect {
        x: full.x,
        y: hole.y + hole.h,
        w: full.w,
        h: (full.y + full.h) - (hole.y + hole.h),
    };
    let left = PointRect { x: full.x, y: hole.y, w: hole.x - full.x, h: hole.h };
    let right = PointRect {
        x: hole.x + hole.w,
        y: hole.y,
        w: (full.x + full.w) - (hole.x + hole.w),
        h: hole.h,
    };
    for band in [above, below, left, right] {
        if band.w > 0.0 && band.h > 0.0 {
            shade(&band, DIM);
        }
    }
}

fn shade(area: &PointRect, alpha: f64) {
    grey(0.0, alpha).set();
    NSBezierPath::fillRect(ns_rect(area));
}

/// A hairline around the selection, light over the dim and dark over the
/// content, so the edge is visible whatever is behind it.
fn outline(selection: &PointRect) {
    let path = NSBezierPath::bezierPathWithRect(ns_rect(selection));
    path.setLineWidth(1.0);
    grey(1.0, 0.95).set();
    path.stroke();
}

/// Crosshair guides across the whole display while aiming, which is what makes
/// it possible to line a selection up with something before pressing.
fn guides(state: &SnipState) {
    let Some((x, y)) = state.cursor else { return };
    let (w, h) = state.geometry.points;
    grey(1.0, 0.35).set();
    NSBezierPath::fillRect(ns_rect(&PointRect { x: x - 0.5, y: 0.0, w: 1.0, h }));
    NSBezierPath::fillRect(ns_rect(&PointRect { x: 0.0, y: y - 0.5, w, h: 1.0 }));
}

/// `1280 × 720` above the selection, in pixels rather than points.
///
/// Pixels because that is what a figure in an article is measured in — a
/// selection that reads 640 points on a Retina display publishes at 1280, and
/// knowing which one you have is the difference between a crisp figure and a
/// blurry one.
fn size_label(selection: &PointRect, geometry: &DisplayGeometry) {
    let scale = geometry.scale();
    let text = NSString::from_str(&size_text(
        (selection.w * scale).round() as i64,
        (selection.h * scale).round() as i64,
    ));
    let attributes = attributes(13.0);
    let size = unsafe { text.sizeWithAttributes(Some(&attributes)) };
    let pad = 6.0;
    // Above the selection, and inside it when there is no room above — a
    // selection at the top of the display would otherwise label itself
    // off-screen.
    let plate_h = size.height + pad;
    let above = selection.y - plate_h - 4.0;
    let plate = PointRect {
        x: selection.x,
        y: if above >= 0.0 { above } else { selection.y + 4.0 },
        w: size.width + pad * 2.0,
        h: plate_h,
    };
    grey(0.05, 0.85).set();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(&plate), 4.0, 4.0).fill();
    unsafe {
        text.drawAtPoint_withAttributes(
            NSPoint::new(plate.x + pad, plate.y + pad / 2.0),
            Some(&attributes),
        )
    };
}

/// What the label says: the pixels under the box and, when they differ, the
/// size they become — see [`crate::figure::encode`]. An arrow up is the one
/// thing a thumbnail cannot show and the difference between a crisp figure and
/// a soft one: the box is smaller than the figure it will be scaled up into.
pub(super) fn size_text(w: i64, h: i64) -> String {
    let (tw, th) = (
        i64::from(crate::figure::encode::WIDTH),
        i64::from(crate::figure::encode::HEIGHT),
    );
    if w < tw {
        format!("{w} × {h}  ↑ {tw} × {th}")
    } else if w > tw {
        format!("{w} × {h}  ↓ {tw} × {th}")
    } else {
        format!("{w} × {h}")
    }
}

/// The one line that says what the gesture is, centred on the display.
///
/// Shown only before the first press: once there is a selection on screen the
/// gesture has explained itself, and a caption floating over the picture is in
/// the way of aiming it.
fn hint(full: &PointRect) {
    let text =
        NSString::from_str("Drag to capture a figure  ·  click or right-click to cancel");
    let attributes = attributes(16.0);
    let size = unsafe { text.sizeWithAttributes(Some(&attributes)) };
    let pad = 12.0;
    let plate = PointRect {
        x: (full.w - size.width) / 2.0 - pad,
        y: full.h * 0.08,
        w: size.width + pad * 2.0,
        h: size.height + pad,
    };
    grey(0.05, 0.8).set();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(&plate), 8.0, 8.0).fill();
    unsafe {
        text.drawAtPoint_withAttributes(
            NSPoint::new(plate.x + pad, plate.y + pad / 2.0),
            Some(&attributes),
        )
    };
}

fn attributes(size: f64) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = NSFont::boldSystemFontOfSize(size);
    let foreground = grey(1.0, 0.95);
    unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref() as &AnyObject, foreground.as_ref() as &AnyObject],
        )
    }
}

fn grey(level: f64, alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(level, level, level, alpha)
}

fn ns_rect(r: &PointRect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}
