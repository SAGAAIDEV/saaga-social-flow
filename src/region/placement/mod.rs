//! How a region is positioned and sized — the part that is saved, and the part
//! that makes Vertical follow Horizontal.
//!
//! A [`PointRect`] is where a region *is*. A [`Placement`] is the thing the
//! operator actually edits and the recorder writes to disk, and the two are not
//! the same because a rect cannot survive the things that change underneath it.
//! Resolving a placement into a rect takes the display's geometry, the layout's
//! slot, and — for a child — its parent's resolved rect. Storing the rect
//! instead would mean re-deriving all three every time any of them moved.
//!
//! ## The parent link
//!
//! Split-Vertical is parented to Split-Horizontal. They are two crops of the
//! same demonstration, and framing them independently means framing the same
//! content twice and keeping the two in your head. Parenting makes the
//! horizontal region the thing you aim, and the vertical a fixed relationship
//! to it that travels along.
//!
//! Note that the child is **not** contained by the parent, and cannot be: at
//! 1:1 the horizontal slot is 1402x1080 pixels and the vertical is 1080x1280,
//! so the child is 200 pixels *taller* than its parent. Parenting here is an
//! inherited transform, not a bounding box — the child's offset is normalized
//! to the parent's rect, so it is free to hang outside it while still moving
//! and scaling as one.
//!
//! ## Zoom does not change the output size
//!
//! [`Placement::zoom`] scales how much of the display a region covers, and
//! nothing else. The file is always written at the layout slot's own pixel
//! size, with ScreenCaptureKit resampling on the GPU on the way out. Two
//! consequences worth stating, because the first one used to be false:
//!
//! - **A zoom or a move never changes an `AVAssetWriterInput`'s dimensions**,
//!   so neither one needs a chapter cut. Only a *layout* change does, because
//!   only that changes the slot.
//! - `zoom == 1.0` is the pixel-exact case — one display pixel per output
//!   pixel, the sharpest a screencast can be. Above 1.0 buys context and costs
//!   sharpness; below 1.0 upscales and only costs.

use super::{DisplayGeometry, PixelSize, PointRect};

/// The narrowest and widest a region may be zoomed.
///
/// The floor is where upscaling stops being a trade and starts being a mistake;
/// the ceiling is roughly where a 2x display has no more pixels to give.
pub const MIN_ZOOM: f64 = 0.25;
pub const MAX_ZOOM: f64 = 4.0;

/// Where a region sits and how big it is, in the form that gets saved.
///
/// The meaning of `offset` depends on the role, not on the type:
///
/// - **Root** (Split-Horizontal): an absolute display-local point origin.
/// - **Child** (Split-Vertical): normalized to the parent's rect — `(0.0, 0.0)`
///   is the parent's top-left corner and `(1.0, 1.0)` its bottom-right. Storing
///   it normalized rather than in points is what makes the child survive the
///   parent being zoomed: a point offset would keep its distance while the
///   parent grew around it, and the relationship the operator framed would
///   drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub offset: (f64, f64),
    /// Multiplier on the 1:1 base size. For a child this is *additional* to
    /// whatever its parent is zoomed to, so a child at 1.0 tracks its parent
    /// exactly.
    pub zoom: f64,
}

impl Default for Placement {
    /// Origin, unzoomed. For a root this is the display's top-left corner,
    /// which callers immediately replace with a centred origin; for a child it
    /// means "sitting on the parent's top-left corner at the parent's scale".
    fn default() -> Placement {
        Placement {
            offset: (0.0, 0.0),
            zoom: 1.0,
        }
    }
}

impl Placement {
    /// A placement with `zoom` forced into the supported range.
    ///
    /// Applied on the way in from disk as well as from a drag: a hand-edited
    /// config with `"zoom": 0` would otherwise resolve to a zero-sized region
    /// that cannot be grabbed to fix.
    pub fn sane(self) -> Placement {
        let zoom = if self.zoom.is_finite() {
            self.zoom.clamp(MIN_ZOOM, MAX_ZOOM)
        } else {
            1.0
        };
        Placement {
            offset: (finite_or(self.offset.0, 0.0), finite_or(self.offset.1, 0.0)),
            zoom,
        }
    }
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// A region's 1:1 size in points: the **output's** pixels divided by the
/// display's backing scale.
///
/// At this size one display pixel becomes one output pixel and no resampling
/// happens anywhere in the chain. It is the *base* the zoom multiplies, not a
/// limit — a display too small to show it at 1:1 is handled by [`resolve`]
/// shrinking the resolved rect, which costs sharpness but never the aspect.
///
/// Derived from the rounded [`PixelSize`], **not** from the layout slot it came
/// from, and that distinction is load-bearing. Split-Horizontal's slot is
/// 1402.562 x 1080 but its file is written at 1402 x 1080; basing the region on
/// the raw slot would give it an aspect 4e-4 wider than the file's, and
/// `SCStreamConfiguration.preservesAspectRatio` — on by default, never set by
/// us — letterboxes any mismatch between `sourceRect`'s aspect and
/// `width`/`height`'s. Sub-pixel bars, but bars for no reason: rounding first
/// makes the two aspects bit-identical and there is nothing left to preserve.
pub fn base_size(output: PixelSize, geom: &DisplayGeometry) -> (f64, f64) {
    let scale = geom.scale();
    (output.w as f64 / scale, output.h as f64 / scale)
}

/// A resolved region: where it landed, and the zoom that got it there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub rect: PointRect,
    /// Zoom *including* any inherited from a parent — what a child's own zoom
    /// gets multiplied into, and what [`Placement::from_rect`] divides back out.
    pub effective_zoom: f64,
    /// `true` when the region had to be shrunk below its zoom to fit the
    /// display, so the output is resampled rather than pixel-exact even at
    /// `zoom == 1.0`. Callers are expected to say so out loud; a silently soft
    /// take is the failure mode this flag exists to prevent.
    pub clamped: bool,
}

/// Turn a placement into a rect on this display.
///
/// `parent` is the already-resolved parent for a child region, and `None` for a
/// root. Resolution order matters: a child cannot be resolved before its parent,
/// which is why [`crate::layouts::Layout::parent`] is a single link rather than
/// an arbitrary graph.
pub fn resolve(
    base: (f64, f64),
    placement: Placement,
    parent: Option<&Resolved>,
    geom: &DisplayGeometry,
) -> Resolved {
    let placement = placement.sane();
    let effective_zoom = match parent {
        Some(parent) => parent.effective_zoom * placement.zoom,
        None => placement.zoom,
    };

    let wanted = (base.0 * effective_zoom, base.1 * effective_zoom);
    // Shrink to fit rather than clip: both axes are divided by the same factor,
    // so a region too big for the display comes back smaller and the same
    // shape. Clipping would return the wrong aspect, which `object-fit: cover`
    // then trims in the render.
    let fit = (geom.points.0 / wanted.0)
        .min(geom.points.1 / wanted.1)
        .min(1.0);
    let clamped = fit < 1.0;
    let size = (wanted.0 * fit, wanted.1 * fit);

    let origin = match parent {
        // Normalized into the parent's rect, so the child holds its
        // relationship as the parent moves and zooms.
        Some(parent) => (
            parent.rect.x + placement.offset.0 * parent.rect.w,
            parent.rect.y + placement.offset.1 * parent.rect.h,
        ),
        None => placement.offset,
    };

    Resolved {
        rect: PointRect {
            x: 0.0,
            y: 0.0,
            w: size.0,
            h: size.1,
        }
        .moved_to(origin, geom),
        effective_zoom: effective_zoom * fit,
        clamped,
    }
}

impl Placement {
    /// The placement that would resolve to `rect` — the inverse of [`resolve`],
    /// used when a drag commits.
    ///
    /// Going back through the placement rather than storing the rect directly
    /// is what keeps a dragged child's relationship to its parent intact: the
    /// offset is re-normalized against the parent it was dropped on, so moving
    /// the parent afterwards carries the child along exactly as before.
    pub fn from_rect(rect: &PointRect, base: (f64, f64), parent: Option<&Resolved>) -> Placement {
        let zoom = if base.0 > 0.0 { rect.w / base.0 } else { 1.0 };
        match parent {
            Some(parent) if parent.rect.w > 0.0 && parent.rect.h > 0.0 => Placement {
                offset: (
                    (rect.x - parent.rect.x) / parent.rect.w,
                    (rect.y - parent.rect.y) / parent.rect.h,
                ),
                // Divide the inherited zoom back out, so the child's own zoom
                // stays a *relative* number and a later parent zoom still
                // scales it.
                zoom: zoom / parent.effective_zoom.max(f64::MIN_POSITIVE),
            }
            .sane(),
            _ => Placement {
                offset: (rect.x, rect.y),
                zoom,
            }
            .sane(),
        }
    }
}

#[cfg(test)]
mod tests;
