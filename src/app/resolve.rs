//! Turning a layout plus a saved placement plus a display into a rect.
//!
//! Pure functions, no [`App`](crate::app::App) state. Two callers need this and
//! only one of them has an `App` to ask: startup brings the screen stream up
//! *before* the camera session — deliberately, so a screen that refuses to open
//! does not leave a camera running headless — and by then it needs to know what
//! to capture. [`crate::app::framing`] does the same work for every layout at
//! once, once a display is settled on.
//!
//! Both go through here rather than each carrying the rule, because two
//! implementations would agree until one of them changed, and the symptom would
//! be a stream capturing one rect while the overlay drew another — invisible
//! until the edit.

use anyhow::Result;

use crate::capture::screen;
use crate::capture::screen_stream::Capture;
use crate::config::Config;
use crate::layouts::{Layout, Orientation, Pair};
use crate::region::placement::{self, Placement};
use crate::region::{DisplayGeometry, PixelSize, PointRect};

/// The pixel size a layout's screen file is written at: its slot, rounded even.
///
/// Fixed by the composition, never by the region — that separation is what lets
/// a zoom happen mid-chapter, and what stops the render resampling a second
/// time on top of ScreenCaptureKit's.
pub(crate) fn slot_output(layout: &Layout) -> Option<PixelSize> {
    layout
        .slot_size()
        .map(|(w, h)| PixelSize::rounded(w, h))
}

/// A root region's placement, centred on the display at 1:1.
pub(crate) fn centred_placement(layout: &Layout, geometry: &DisplayGeometry) -> Placement {
    let Some(output) = slot_output(layout) else {
        return Placement::default();
    };
    let base = placement::base_size(output, geometry);
    let centred = PointRect::centered(base, geometry);
    Placement {
        offset: (centred.x, centred.y),
        zoom: 1.0,
    }
}

/// What a pair should capture on `display_uid` before there is an [`App`].
///
/// Split captures the union of both orientation regions so each preview
/// branch can crop its own rect. Talking Head has no screen.
pub(crate) fn pair_capture(pair: Pair, display_uid: &str) -> Result<Option<Capture>> {
    if pair != Pair::Split {
        return layout_capture(Layout::get(pair, Orientation::Horizontal), display_uid);
    }
    let geometry = screen::display_geometry(display_uid)?;
    let cfg = crate::config::load();
    let horizontal = Layout::get(Pair::Split, Orientation::Horizontal);
    let vertical = Layout::get(Pair::Split, Orientation::Vertical);
    let Some(h_out) = slot_output(horizontal) else {
        return Ok(None);
    };
    let Some(v_out) = slot_output(vertical) else {
        return Ok(None);
    };
    let h_placement = saved_or_centred(&cfg, display_uid, horizontal, &geometry);
    let h_resolved = placement::resolve(
        placement::base_size(h_out, &geometry),
        h_placement,
        None,
        &geometry,
    );
    let v_placement = saved_or_centred(&cfg, display_uid, vertical, &geometry);
    let v_resolved = placement::resolve(
        placement::base_size(v_out, &geometry),
        v_placement,
        Some(&h_resolved),
        &geometry,
    );
    let union = h_resolved.rect.union(&v_resolved.rect);
    let scale = geometry.scale();
    Ok(Some(Capture {
        region: union,
        output: PixelSize::rounded(union.w * scale, union.h * scale),
    }))
}

/// What `layout` should capture on `display_uid` before there is an [`App`] to
/// ask. **The only place startup and [`App::rebuild_regions`] can disagree**,
/// so it goes through the same placement resolution rather than a second copy
/// of the rule.
pub(crate) fn layout_capture(layout: &Layout, display_uid: &str) -> Result<Option<Capture>> {
    let Some(output) = slot_output(layout) else {
        return Ok(None);
    };
    let geometry = screen::display_geometry(display_uid)?;
    let cfg = crate::config::load();

    // Resolve the parent first when there is one, exactly as `resolve_regions`
    // does, or a saved child placement would be normalized against nothing.
    let parent = layout.parent().and_then(|parent| {
        let base = placement::base_size(slot_output(parent)?, &geometry);
        let placement = saved_or_centred(&cfg, display_uid, parent, &geometry);
        Some(placement::resolve(base, placement, None, &geometry))
    });

    let base = placement::base_size(output, &geometry);
    let placement = saved_or_centred(&cfg, display_uid, layout, &geometry);
    let resolved = placement::resolve(base, placement, parent.as_ref(), &geometry);
    Ok(Some(Capture {
        region: resolved.rect,
        output,
    }))
}

pub(crate) fn saved_or_centred(
    cfg: &Config,
    display_uid: &str,
    layout: &Layout,
    geometry: &DisplayGeometry,
) -> Placement {
    match cfg.placement(display_uid, layout.block, geometry.points) {
        Some(saved) => Placement {
            offset: saved.offset,
            zoom: saved.zoom,
        }
        .sane(),
        None => match layout.parent() {
            Some(_) => Placement::default(),
            None => centred_placement(layout, geometry),
        },
    }
}