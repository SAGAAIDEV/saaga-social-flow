//! Where a screen region is, in the three coordinate spaces it has to survive.
//!
//! A region is the part of a display that gets captured, sized so it matches the
//! aspect of the layout slot it will land in. Getting it from a mouse drag to
//! `SCStreamConfiguration` crosses three coordinate systems and one unit change,
//! and every one of them is a place to be off by a factor of two or a screen
//! height:
//!
//! 1. **AppKit global** — origin at the *primary* screen's bottom-left, y
//!    growing **up**, points. What [`NSScreen::frame`] returns and what the
//!    overlay window and its mouse events live in.
//! 2. **Core Graphics global** — origin at the *primary* display's top-left, y
//!    growing **down**, points. What `CGDisplayBounds` and `SCDisplay::frame`
//!    return.
//! 3. **Display-local** — Core Graphics global minus that display's own origin.
//!    This is [`PointRect`], and it is what `setSourceRect` takes.
//!
//! The unit change sits inside step 3: `setSourceRect` is in **points** while
//! `setWidth`/`setHeight` on the very same `SCStreamConfiguration` are in
//! **pixels**. That is the same trap `capture::screen_stream::find_display`
//! already carries a warning about — passing `SCDisplay`'s point size to
//! `setWidth` captures a Retina display at half its resolution — so the two
//! units get two types here, [`PointRect`] and [`PixelSize`], and there is no
//! way to hand one where the other belongs.
//!
//! The y-flip in step 1 is defined against the **primary** screen's height, not
//! the height of the display the rect is on. On a single-monitor machine those
//! are the same number and every wrong implementation looks right; on a second
//! display of a different height they diverge, which is why
//! [`DisplayGeometry::primary_height_points`] is carried explicitly rather than
//! read off `points`.
//!
//! Everything here is arithmetic. Nothing sends an Objective-C message, so it is
//! all testable with no display, no permission prompt, and no main thread —
//! `CGRect` and its `NSRect` alias are plain `repr(C)` data.
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | the primitives — a rect, a pixel size, a display, and the resize gesture |
//! | [`placement`] | what gets *saved*: offset, zoom, and Vertical's parent link to Horizontal |
//! | [`cover`] | `object-fit: cover` + `object-position`, for placing the camera in a layout slot |
//! | [`framing`] | how the camera is aimed inside that slot: a punch-in, and a live anchor |

pub mod cover;
pub mod framing;
pub mod placement;
pub mod track;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

/// A rect in one display's own point space: origin at that display's top-left,
/// y growing downward, both axes in points.
///
/// This is the canonical form a region is stored, dragged, and persisted in,
/// because it is the one space that does not move when another monitor is
/// plugged in or the primary is reassigned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// An encoder-ready size in pixels, both axes even.
///
/// H.264 requires even dimensions — `find_display` already masks the display
/// size with `& !1` for the same reason — so evenness is a property of the type
/// rather than something each call site remembers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelSize {
    pub w: usize,
    pub h: usize,
}

impl PixelSize {
    /// Round a size to even whole pixels.
    ///
    /// The only constructor, so evenness cannot be bypassed by building the
    /// struct literally — a layout slot like 1402.562 x 1080 has to come
    /// through here before it can reach an encoder.
    pub fn rounded(w: f64, h: f64) -> PixelSize {
        PixelSize {
            w: even(w),
            h: even(h),
        }
    }
}

/// Everything about one display that a coordinate conversion needs, gathered so
/// no caller has to hold two of these numbers and guess at the third.
///
/// `capture::screen` computes the point size and the pixel size in two separate
/// functions today (`read_displays` reads `SCDisplay::width`, which is points;
/// `find_display` reads `CGDisplayModeGetPixelWidth`, which is pixels) and
/// discards the pairing. This is where the pairing lives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayGeometry {
    /// This display's origin in Core Graphics' global space — top-left origin,
    /// y down. Negative for a display placed left of or above the primary.
    pub cg_origin: (f64, f64),
    /// This display's size in points.
    pub points: (f64, f64),
    /// This display's size in pixels.
    pub pixels: (usize, usize),
    /// The **primary** screen's height in points. Only used for the AppKit
    /// y-flip, which is defined against the primary and not against this
    /// display — see the module docs.
    pub primary_height_points: f64,
}

impl DisplayGeometry {
    /// Pixels per point — 2.0 on a Retina display, 1.0 on an external one, and
    /// a fraction on a scaled mode.
    ///
    /// Taken from the width. The two axes agree on every real display, but the
    /// point size arrives as an integer from `SCDisplay`, so deriving from
    /// height as well would introduce a second, very slightly different scale
    /// for no gain. Falls back to 1.0 rather than dividing by zero: a display
    /// that reports no size is already being rejected upstream by
    /// `find_display`, and a NaN scale would propagate silently into a rect.
    pub fn scale(&self) -> f64 {
        if self.points.0 > 0.0 {
            self.pixels.0 as f64 / self.points.0
        } else {
            1.0
        }
    }
}

/// Which corner of a region a resize drag has hold of.
///
/// Corners only, no edge handles: the aspect is locked to the layout slot, so
/// an edge drag would have to move the opposite edge too and would read as
/// broken. One handle per corner is the whole gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    /// This corner's position on `rect`.
    pub fn at(self, rect: &PointRect) -> (f64, f64) {
        let (right, bottom) = (rect.x + rect.w, rect.y + rect.h);
        match self {
            Corner::TopLeft => (rect.x, rect.y),
            Corner::TopRight => (right, rect.y),
            Corner::BottomLeft => (rect.x, bottom),
            Corner::BottomRight => (right, bottom),
        }
    }

    /// The corner diagonally opposite — the one a resize holds still.
    pub fn opposite(self) -> Corner {
        match self {
            Corner::TopLeft => Corner::BottomRight,
            Corner::TopRight => Corner::BottomLeft,
            Corner::BottomLeft => Corner::TopRight,
            Corner::BottomRight => Corner::TopLeft,
        }
    }
}

/// One axis of a region's gnomon.
///
/// The gnomon is rooted at the region's centre and exists to make a *constrained*
/// move possible: a free body-drag nudges both coordinates when you meant one,
/// and on a region whose horizontal framing is already settled that is the whole
/// reason to reach for an axis handle instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Right, in both screen and display-local terms.
    X,
    /// **Up on screen**, which is *decreasing* y here: display-local space has
    /// its origin at the display's top-left with y growing down, and the
    /// overlay view sets `isFlipped` so its coordinates match. An arrow drawn
    /// toward +y would point down and mean the opposite of what it shows.
    Y,
}

impl Axis {
    pub const ALL: [Axis; 2] = [Axis::X, Axis::Y];
}

/// Smallest region a resize will produce, as a fraction of its current size.
///
/// Callers pass an absolute floor instead (see [`PointRect::resized`]); this is
/// only the fallback when they have none. Not zero: a region dragged to nothing
/// is unrecoverable by dragging, because there is nothing left to grab.
pub const MIN_SIZE_POINTS: f64 = 80.0;

impl PointRect {
    /// A rect of `size` points centred on the display.
    pub fn centered(size: (f64, f64), geom: &DisplayGeometry) -> PointRect {
        PointRect {
            x: ((geom.points.0 - size.0) / 2.0).max(0.0),
            y: ((geom.points.1 - size.1) / 2.0).max(0.0),
            w: size.0,
            h: size.1,
        }
    }

    /// Re-express this display-local rect in AppKit's global space, ready to be
    /// handed to an `NSWindow`.
    ///
    /// The recorder's only crossing into AppKit coordinates, used once to place
    /// the overlay window over its display. There is deliberately no inverse:
    /// the overlay's view sets `isFlipped`, so once the window is placed its
    /// contents are already in this space and nothing converts back. Adding a
    /// `from_appkit` would be an untested second implementation of the same
    /// y-flip waiting to disagree with this one.
    pub fn to_appkit(&self, geom: &DisplayGeometry) -> CGRect {
        // AppKit y is measured up from the *primary* screen's bottom edge and
        // names the rect's bottom; Core Graphics y is measured down from the
        // primary's top edge and names its top. Flipping needs the primary's
        // height and this rect's own height, and never this display's height —
        // see the module docs.
        let cg_x = self.x + geom.cg_origin.0;
        let cg_y = self.y + geom.cg_origin.1;
        CGRect::new(
            CGPoint::new(cg_x, geom.primary_height_points - (cg_y + self.h)),
            CGSize::new(self.w, self.h),
        )
    }

    /// Re-express this display-local rect in Core Graphics' **global** space:
    /// origin at the primary display's top-left, y down, points.
    ///
    /// Not an inverse of [`to_appkit`](PointRect::to_appkit), and deliberately
    /// not built out of one. That conversion crosses into AppKit's bottom-left
    /// space and needs the *primary* display's height to do the y-flip; this one
    /// stays in Core Graphics, where both spaces already have y growing down, so
    /// it is the display's own origin added and nothing else. Writing it as a
    /// flip-and-unflip would import the one piece of arithmetic that is easy to
    /// get wrong into the one conversion that does not need it.
    ///
    /// What `SCScreenshotManager::captureImageInRect:` takes — see
    /// [`crate::figure::shot`].
    pub fn to_cg_global(&self, geom: &DisplayGeometry) -> CGRect {
        CGRect::new(
            CGPoint::new(self.x + geom.cg_origin.0, self.y + geom.cg_origin.1),
            CGSize::new(self.w, self.h),
        )
    }

    /// This rect as the `CGRect` `SCStreamConfiguration::setSourceRect` wants.
    ///
    /// A separate method from the field access it happens to be, so the call
    /// site reads as a deliberate handoff into ScreenCaptureKit's units rather
    /// than as a rect being passed somewhere by luck.
    pub fn to_source_rect(&self) -> CGRect {
        CGRect::new(CGPoint::new(self.x, self.y), CGSize::new(self.w, self.h))
    }

    /// The smallest rect that contains both `self` and `other`.
    pub fn union(&self, other: &PointRect) -> PointRect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.w).max(other.x + other.w);
        let bottom = (self.y + self.h).max(other.y + other.h);
        PointRect {
            x,
            y,
            w: (right - x).max(0.0),
            h: (bottom - y).max(0.0),
        }
    }

    /// Where `self` sits inside `outer`, mapped into a `output`-sized buffer.
    ///
    /// Used to crop one region's pixels out of a capture of the union of both.
    pub fn in_buffer(&self, outer: &PointRect, output: PixelSize) -> Option<(f64, f64, f64, f64)> {
        if outer.w <= 0.0 || outer.h <= 0.0 {
            return None;
        }
        let sx = output.w as f64 / outer.w;
        let sy = output.h as f64 / outer.h;
        Some((
            (self.x - outer.x) * sx,
            (self.y - outer.y) * sy,
            self.w * sx,
            self.h * sy,
        ))
    }

    /// The encoder size this region produces on a display of `scale`.
    pub fn pixels(&self, scale: f64) -> PixelSize {
        PixelSize {
            w: even(self.w * scale),
            h: even(self.h * scale),
        }
    }

    /// The same rect at a new origin, clamped so it stays wholly on the
    /// display. Size is untouched.
    pub fn moved_to(&self, origin: (f64, f64), geom: &DisplayGeometry) -> PointRect {
        // `max(0.0)` on the limit, not just on the value: a region larger than
        // the display would otherwise clamp to a negative upper bound and
        // panic inside `clamp`.
        let max_x = (geom.points.0 - self.w).max(0.0);
        let max_y = (geom.points.1 - self.h).max(0.0);
        PointRect {
            x: origin.0.clamp(0.0, max_x),
            y: origin.1.clamp(0.0, max_y),
            w: self.w,
            h: self.h,
        }
    }

    /// Resize by dragging `corner` to `point`, holding the opposite corner
    /// still and the **aspect fixed**.
    ///
    /// The aspect lock is not a nicety. It is the whole reason a region is
    /// framed against a layout at all: the composition drops this capture into
    /// a fixed hole with `object-fit: cover`, so an aspect that drifts is
    /// silently trimmed at the edges in the render. Size may be whatever the
    /// operator wants; shape may not.
    ///
    /// The drag reads whichever axis moved further, converted through the
    /// aspect, so grabbing a corner and pulling sideways or downward both feel
    /// like the same gesture rather than one axis being dead.
    /// `min_width` is the narrowest the caller will accept, in points. It must
    /// be the same floor the *placement* layer enforces — `MIN_ZOOM x base` —
    /// or a drag stops at one size and springs to another the instant it is
    /// released, because `Placement::sane` clamps the committed zoom back up.
    pub fn resized(
        &self,
        corner: Corner,
        point: (f64, f64),
        min_width: f64,
        geom: &DisplayGeometry,
    ) -> PointRect {
        let aspect = self.w / self.h;
        if !aspect.is_finite() || aspect <= 0.0 {
            return *self;
        }
        let anchor = corner.opposite().at(self);

        // How much room there is between the still corner and the display edge
        // the drag is heading for. Capping here rather than clamping the
        // finished rect is what keeps the aspect intact at the boundary: a rect
        // clipped after the fact would come back the wrong shape.
        let room_x = if point.0 < anchor.0 {
            anchor.0
        } else {
            geom.points.0 - anchor.0
        };
        let room_y = if point.1 < anchor.1 {
            anchor.1
        } else {
            geom.points.1 - anchor.1
        };

        let wanted = (point.0 - anchor.0)
            .abs()
            .max((point.1 - anchor.1).abs() * aspect);
        let max_w = room_x.min(room_y * aspect);
        // An anchor already off the display leaves no room in the drag's
        // direction, and the clamp below would then collapse the rect to 0x0
        // with a NaN aspect. Not reachable through the overlay today — every
        // rect it hands back has been through `moved_to` — but a zero-sized
        // region cannot be grabbed to undo, so refuse the drag instead.
        if max_w <= 0.0 {
            return *self;
        }
        let floor = min_width.max(MIN_SIZE_POINTS).min(max_w);
        let w = wanted.clamp(floor, max_w);
        let h = w / aspect;

        PointRect {
            x: if point.0 < anchor.0 {
                anchor.0 - w
            } else {
                anchor.0
            },
            y: if point.1 < anchor.1 {
                anchor.1 - h
            } else {
                anchor.1
            },
            w,
            h,
        }
    }

    /// The rect's centre — where its gnomon is rooted.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Move to a new origin along one axis only, leaving the other coordinate
    /// exactly as it was.
    ///
    /// Not `moved_to` with one component copied in by the caller: doing it here
    /// means the untouched coordinate is the rect's own current value rather
    /// than whatever the caller happened to have, so a constrained drag cannot
    /// quietly re-clamp the axis it is not moving.
    pub fn moved_on(&self, axis: Axis, origin: (f64, f64), geom: &DisplayGeometry) -> PointRect {
        let constrained = match axis {
            Axis::X => (origin.0, self.y),
            Axis::Y => (self.x, origin.1),
        };
        self.moved_to(constrained, geom)
    }

    /// Whether the two rects share any area.
    ///
    /// Half-open on both axes, matching [`Self::contains`]: rects that merely
    /// touch along an edge do not overlap. Used to stack the overlay's labels,
    /// where the pair routinely shares an origin exactly.
    pub fn overlaps(&self, other: &PointRect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Whether a point in this display's own point space lands inside the rect.
    pub fn contains(&self, point: (f64, f64)) -> bool {
        point.0 >= self.x
            && point.0 < self.x + self.w
            && point.1 >= self.y
            && point.1 < self.y + self.h
    }
}

/// Round to the nearest even whole number, never below 2.
///
/// Rounding rather than truncating: at a fractional backing scale a slot lands
/// on values like 1401.6, and flooring would drop it to 1400 — two pixels of
/// aspect error on every frame, for nothing.
///
/// Rounds in *halves* rather than rounding then clearing the low bit. Those two
/// agree on every value the current layout table produces, but they diverge by a
/// whole pixel above an odd number — 1403.4 is nearest 1404, while round-then-mask
/// gives 1402 — and a re-export that lands a slot there would silently double the
/// aspect error this function exists to keep small.
fn even(value: f64) -> usize {
    if !value.is_finite() || value < 2.0 {
        return 2;
    }
    ((value / 2.0).round() as usize) * 2
}

#[cfg(test)]
mod tests;
