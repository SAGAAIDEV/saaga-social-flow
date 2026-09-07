//! What a press at a point grabs — and, just as importantly, what it does not.
//!
//! Two properties live here, and both are load-bearing for the overlay being
//! usable at all:
//!
//! **Nothing claims a region's interior.** A region covers the part of the
//! screen being demonstrated, so if its body were a drag target the overlay
//! would have to swallow clicks over exactly the windows being arranged inside
//! it. Returning `None` there is what lets `RegionOverlay::track_cursor` set
//! the window click-through.
//!
//! **The sweep is tier-major, not region-major.** The regions overlap heavily,
//! so one region's gnomon routinely sits inside another's bounds. Asking each
//! region for everything it offers in turn made the vertical region's arrows
//! unreachable behind the horizontal region.

use super::DrawnRegion;
use crate::region::{Axis, Corner};
use super::draw::{arm_at, corner_at, knob_rect};

/// What a mouse-down started.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Drag {
    /// Moving the whole region; carries the offset from its origin to the point
    /// the mouse grabbed it at, so it does not jump under the cursor.
    Move { grab: (f64, f64) },
    /// Moving along one gnomon arm, leaving the other coordinate alone.
    Axis { axis: Axis, grab: (f64, f64) },
    /// Resizing from one corner, with the opposite corner held still.
    Resize { corner: Corner },
}

/// Which region a mouse-down landed on, and what it grabbed there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Grabbed {
    pub(super) region: usize,
    pub(super) drag: Drag,
}

/// How deliberate a target is. Swept most specific first.
///
/// There is deliberately no `Body` tier. A region's interior is the part of the
/// screen you are demonstrating *in*, so making it a drag target would mean the
/// overlay had to swallow clicks over exactly the windows you are arranging.
/// The gnomon exists so it does not have to: the knob is the free-move handle
/// the body used to be, and everything that is not a handle clicks straight
/// through to whatever is behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tier {
    Knob,
    Arm,
    Corner,
}

impl Tier {
    const ALL: [Tier; 3] = [Tier::Knob, Tier::Arm, Tier::Corner];
}

/// What `point` grabs on `region` at one tier, if anything.
pub(super) fn grab_in(region: &DrawnRegion, point: (f64, f64), tier: Tier) -> Option<Drag> {
    let grab = (point.0 - region.rect.x, point.1 - region.rect.y);
    match tier {
        Tier::Knob => knob_rect(&region.rect)
            .contains(point)
            .then_some(Drag::Move { grab }),
        Tier::Arm => arm_at(&region.rect, point).map(|axis| Drag::Axis { axis, grab }),
        Tier::Corner => corner_at(&region.rect, point).map(|corner| Drag::Resize { corner }),
    }
}

/// What a mouse-down at `point` grabs, across every region.
///
/// **Tier-major, not region-major**, and that is the whole content of this
/// function. The regions overlap heavily — Split-Vertical's default placement
/// puts it inside Split-Horizontal — so one region's gnomon routinely sits on
/// top of another's body. Asking "what does the active region offer?" first and
/// only then moving on meant the active region's *body* answered before the
/// other region's *gnomon* was ever considered, and the vertical arrows could
/// not be clicked at all: every press landed as a free drag of the horizontal
/// region.
///
/// Sweeping by tier fixes that in the general case rather than for this one
/// pair: a knob on any region beats an arm on any region beats a corner on any
/// region. Within a tier the active region wins, which is the right tie-break
/// when two handles genuinely coincide.
///
/// `None` means the overlay wants nothing at this point — which is also the
/// signal that it should let the click through entirely. See
/// [`RegionOverlay::track_cursor`].
pub(super) fn grab_anywhere(regions: &[DrawnRegion], active: usize, point: (f64, f64)) -> Option<Grabbed> {
    Tier::ALL.into_iter().find_map(|tier| {
        std::iter::once(active)
            .chain(0..regions.len())
            .find_map(|index| {
                let region = regions.get(index)?;
                grab_in(region, point, tier).map(|drag| Grabbed {
                    region: index,
                    drag,
                })
            })
    })
}
