//! The 2x2, the screen regions it implies, and the overlay that shows them.
//!
//! Everything here answers one question: *which part of the display is this
//! chapter capturing, and does the operator agree?* The layout fixes the
//! region's aspect, the display fixes its 1:1 size, and the operator moves and
//! zooms it from there.
//!
//! ## Two kinds of change, and only one cuts a chapter
//!
//! A region's **output** size is its layout slot's, never its own pixel count
//! (see [`crate::capture::screen_stream::Capture`]). So:
//!
//! - **Move or zoom** keeps the same output size. The live writer keeps
//!   receiving the dimensions it locked onto, so this is applied straight to
//!   the running stream and the take continues. Reframing mid-chapter is a
//!   creative choice, like moving a camera, and the overlay shows it happening.
//! - **Switching layout** changes the slot, so it changes the output size, so
//!   it needs a new `AVAssetWriterInput` and therefore a new chapter. That cut
//!   is forced by AVFoundation, not chosen.
//!
//! ## Vertical follows Horizontal
//!
//! Split-Vertical is parented to Split-Horizontal ([`Layout::parent`]), so
//! aiming the horizontal region carries the vertical one with it. Parents are
//! resolved before children in [`App::rebuild_regions`], and the overlay
//! reproduces the same relationship live during a drag so the two never look
//! detached while you are aiming them.
//!
//! Split out of [`super`] because it is the only part of the app that reasons
//! about geometry; device lifecycle lives in [`super::devices`], and window and
//! event plumbing in [`super`] itself.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;

use crate::app::devices::screen_track;
use crate::app::resolve::{centred_placement, slot_output};
use crate::app::App;
use crate::capture::screen;
use crate::capture::screen_stream::{Capture, ScreenConnection};
use crate::config::SavedPlacement;
use crate::layouts::{Layout, Orientation, Pair, LAYOUTS};
use crate::overlay::{ChildLink, DrawnRegion, RegionOverlay};
use crate::region::placement::{self, Placement, MIN_ZOOM};
use crate::region::PointRect;

impl App {
    /// The layout the 2x2 currently names.
    pub(super) fn layout(&self) -> &'static Layout {
        Layout::get(self.pair, self.orientation)
    }

    /// What the stream should be capturing for the current layout: where to
    /// crop, and the fixed size to deliver it at.
    ///
    /// `None` for a layout with no screen slot or when no display is selected —
    /// and in the first case the caller will not have opened a stream at all.
    pub(super) fn current_capture(&self) -> Option<Capture> {
        if self.pair == Pair::Split {
            return self.union_capture();
        }
        let layout = self.layout();
        Some(Capture {
            region: self.regions.get(layout.block)?.rect,
            output: slot_output(layout)?,
        })
    }

    /// Both Split regions in one capture, so each preview branch can crop its
    /// own rect out of the same buffer.
    fn union_capture(&self) -> Option<Capture> {
        let horizontal = Layout::get(Pair::Split, Orientation::Horizontal);
        let vertical = Layout::get(Pair::Split, Orientation::Vertical);
        let h = self.regions.get(horizontal.block)?.rect;
        let v = self.regions.get(vertical.block)?.rect;
        let union = h.union(&v);
        let scale = self.geometry?.scale();
        Some(Capture {
            region: union,
            output: crate::region::PixelSize::rounded(union.w * scale, union.h * scale),
        })
    }

    /// Each orientation's region, mapped into the union capture's buffer.
    pub(super) fn screen_crops(&self) -> [Option<(f64, f64, f64, f64)>; 2] {
        let Some(capture) = self.current_capture() else {
            return [None, None];
        };
        let mut crops = [None, None];
        for (index, orientation) in Orientation::ALL.iter().enumerate() {
            let layout = Layout::get(self.pair, *orientation);
            let Some(resolved) = self.regions.get(layout.block) else {
                continue;
            };
            crops[index] = resolved.rect.in_buffer(&capture.region, capture.output);
        }
        crops
    }

    /// Recompute every screen-bearing layout's region for the selected display,
    /// restoring saved placements where they still apply.
    ///
    /// Called whenever the display changes, never when the layout does: the
    /// regions for *both* Split cells exist at once so the overlay can show
    /// where the other orientation's crop sits while you frame this one, and so
    /// the parent link has something to resolve against.
    pub(super) fn rebuild_regions(&mut self) {
        self.geometry = None;
        self.placements.clear();
        self.regions.clear();

        let Some(uid) = self.screen_uid.clone() else {
            return;
        };
        let geometry = match screen::display_geometry(&uid) {
            Ok(geometry) => geometry,
            Err(e) => {
                eprintln!("stream-recorder: could not read display geometry: {e:#}");
                return;
            }
        };

        let cfg = crate::config::load();
        for layout in LAYOUTS.iter().filter(|layout| layout.needs_screen()) {
            let saved = cfg.placement(&uid, layout.block, geometry.points);
            self.placements.insert(
                layout.block,
                match saved {
                    Some(saved) => Placement {
                        offset: saved.offset,
                        zoom: saved.zoom,
                    }
                    .sane(),
                    // No saved placement: centre a root, and sit a child on its
                    // parent's top-left at the parent's scale.
                    None => match layout.parent() {
                        Some(_) => Placement::default(),
                        None => centred_placement(layout, &geometry),
                    },
                },
            );
        }
        self.geometry = Some(geometry);
        self.resolve_regions();
    }

    /// Turn every placement into a rect. Parents first — a child cannot be
    /// resolved before the rect it is normalized against exists.
    pub(super) fn resolve_regions(&mut self) {
        let Some(geometry) = self.geometry else {
            self.regions.clear();
            return;
        };
        self.regions.clear();

        for pass in [false, true] {
            for layout in LAYOUTS.iter().filter(|layout| layout.needs_screen()) {
                if layout.parent().is_some() != pass {
                    continue;
                }
                let (Some(output), Some(placement)) = (
                    slot_output(layout),
                    self.placements.get(layout.block).copied(),
                ) else {
                    continue;
                };
                let parent = layout
                    .parent()
                    .and_then(|parent| self.regions.get(parent.block).copied());
                let base = placement::base_size(output, &geometry);
                let resolved = placement::resolve(base, placement, parent.as_ref(), &geometry);
                // Only an *upscale* is worth warning about, and `clamped`
                // is the wrong test for it: a region shrunk to fit a small
                // display can still be supersampling, and a deliberate zoom
                // below 1.0 upscales without ever being clamped. The honest
                // question is how many screen pixels feed the output, so ask
                // that — in pixels, never in points. Comparing a point size
                // against a pixel size is exactly the confusion the `region`
                // module exists to prevent, and it previously made this line
                // claim "softer" on every Retina display while the capture was
                // in fact sharper than 1:1.
                let source = resolved.rect.pixels(geometry.scale());
                if source.w < output.w || source.h < output.h {
                    eprintln!(
                        "stream-recorder: {} is capturing {}×{} screen pixels into a {}×{} \
                         file — upscaled {:.2}×, which is softer than a 1:1 take. Zoom in or \
                         pick a denser display to recover it.",
                        layout.block,
                        source.w,
                        source.h,
                        output.w,
                        output.h,
                        output.w as f64 / source.w.max(1) as f64,
                    );
                }
                self.regions.insert(layout.block, resolved);
            }
        }
    }

    /// Push the current regions into the overlay, and the overlay's state into
    /// the record window's button.
    pub(super) fn sync_overlay(&mut self) {
        let layout = self.layout();
        let has_screen = layout.needs_screen() && self.screen_uid.is_some();

        // Ordered by orientation so the active index is stable and the two
        // labels always appear in the same order on screen.
        let mut drawn: Vec<DrawnRegion> = Vec::new();
        let mut index_of: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut active = usize::MAX;
        for orientation in Orientation::ALL {
            let other = Layout::get(self.pair, orientation);
            let (Some(resolved), Some(output)) =
                (self.regions.get(other.block), slot_output(other))
            else {
                continue;
            };
            if other.block == layout.block {
                active = drawn.len();
            }
            // The overlay is given ratios rather than the parent link itself,
            // so it can keep a child attached during a drag without knowing
            // what a layout or a zoom is.
            let child_of = other.parent().and_then(|parent| {
                let parent_index = *index_of.get(parent.block)?;
                let parent_rect = self.regions.get(parent.block)?.rect;
                (parent_rect.w > 0.0 && parent_rect.h > 0.0).then_some(ChildLink {
                    parent: parent_index,
                    offset: (
                        (resolved.rect.x - parent_rect.x) / parent_rect.w,
                        (resolved.rect.y - parent_rect.y) / parent_rect.h,
                    ),
                    size_ratio: (
                        resolved.rect.w / parent_rect.w,
                        resolved.rect.h / parent_rect.h,
                    ),
                })
            });
            // A child's zoom is *relative* to its parent, so the floor a drag
            // has to stop at is the parent's scale times the child's own
            // minimum — not the child's minimum alone.
            let inherited = other
                .parent()
                .and_then(|parent| self.regions.get(parent.block))
                .map_or(1.0, |parent| parent.effective_zoom);
            index_of.insert(other.block, drawn.len());
            drawn.push(DrawnRegion {
                orientation,
                rect: resolved.rect,
                pixels: (output.w, output.h),
                zoom: resolved.effective_zoom,
                // The same floor `Placement::sane` will clamp a committed zoom
                // to, so the drag stops exactly where the saved placement would
                // have put it back. `inherited` is what makes that true for the
                // vertical frame: its committed zoom is divided by its parent's
                // on the way to disk, so with the horizontal frame zoomed to 2x
                // a floor of `base * MIN_ZOOM` let the drag reach half the size
                // the placement layer would accept — and the region sprang back
                // to twice its size the instant the mouse came up.
                min_width: self.geometry.map_or(0.0, |g| {
                    placement::base_size(output, &g).0 * MIN_ZOOM * inherited
                }),
                child_of,
            });
        }

        let tracking = self.overlay_tracking();
        if let Some(live) = self.live.as_mut() {
            if let Some(overlay) = live.overlay.as_ref() {
                overlay.set_regions(drawn, active);
                overlay.set_tracking(tracking);
            }
            live.control_target.set_regions(
                has_screen,
                live.overlay.as_ref().is_some_and(RegionOverlay::is_visible),
            );
        }
    }

    /// Show or hide the region overlay, building it on first use.
    pub(super) fn toggle_regions(&mut self) {
        if self
            .live
            .as_ref()
            .and_then(|l| l.overlay.as_ref())
            .is_some()
        {
            if let Some(live) = self.live.as_mut() {
                let visible = live.overlay.as_ref().is_some_and(RegionOverlay::is_visible);
                if let Some(overlay) = live.overlay.as_mut() {
                    if visible {
                        overlay.hide();
                    } else {
                        overlay.show();
                    }
                }
            }
        } else {
            let Some(geometry) = self.geometry else {
                eprintln!("stream-recorder: pick a display before framing its regions");
                return;
            };
            let Some(mtm) = objc2::MainThreadMarker::new() else {
                return;
            };
            let Some(live) = self.live.as_mut() else {
                return;
            };
            let mut overlay = RegionOverlay::new(mtm, geometry, live.ui_tx.clone());
            overlay.show();
            live.overlay = Some(overlay);
        }
        self.sync_overlay();
        // The overlay is a window this process owns, and it was not in the
        // SCShareableContent snapshot the running stream's filter was built
        // from. Without this it would appear in the recording on the very frame
        // it is shown.
        if let Some(screen) = self.screen.as_ref() {
            if let Err(e) = screen.refresh_exclusions() {
                eprintln!("stream-recorder: could not exclude the overlay from capture: {e:#}");
            }
        }
    }

    /// A region was dragged or resized and released. Save it, and re-aim the
    /// capture if it was the one being recorded.
    pub(super) fn region_placed(&mut self, orientation: Orientation, rect: PointRect) {
        let layout = Layout::get(self.pair, orientation);
        let (Some(geometry), Some(uid), Some(output)) =
            (self.geometry, self.screen_uid.clone(), slot_output(layout))
        else {
            return;
        };

        // Back through a placement rather than storing the rect: that is what
        // re-normalizes a dragged child against its parent, so moving the
        // parent afterwards still carries it.
        let parent = layout
            .parent()
            .and_then(|parent| self.regions.get(parent.block).copied());
        let base = placement::base_size(output, &geometry);
        let placement = Placement::from_rect(&rect, base, parent.as_ref());
        self.placements.insert(layout.block, placement);
        self.resolve_regions();

        let mut cfg = crate::config::load();
        cfg.set_placement(
            &uid,
            layout.block,
            geometry.points,
            SavedPlacement {
                offset: placement.offset,
                zoom: placement.zoom,
            },
        );
        if let Err(e) = crate::config::save(&cfg) {
            // Not fatal: the placement is applied either way, it just will not
            // survive a restart.
            eprintln!("stream-recorder: could not save the region placement: {e:#}");
        }

        // Re-aim when the drag touched what is being recorded — either
        // directly, or by moving the parent it follows. That second case is
        // live now that every region is grabbable: dragging Horizontal while
        // recording Vertical carries the vertical region with it, so the
        // capture has moved even though the dragged layout is not the recorded
        // one.
        let recording = self.layout();
        let touches_capture = self.pair == Pair::Split
            || layout.block == recording.block
            || recording
                .parent()
                .is_some_and(|parent| parent.block == layout.block);
        if touches_capture {
            self.apply_region_change();
        }
        self.sync_overlay();
        self.install_preview();
    }

    /// Re-aim the running stream at the current region, **without** cutting a
    /// chapter.
    ///
    /// Safe mid-take precisely because the output size did not change: the live
    /// `AVAssetWriterInput` keeps receiving the dimensions it locked onto at
    /// creation. Only [`apply_layout_change`](App::apply_layout_change) can
    /// change those, and only it cuts.
    pub(super) fn apply_region_change(&mut self) {
        let capture = self.current_capture();
        if let Some(screen) = self.screen.as_mut() {
            if let Err(e) = screen.set_capture(capture) {
                eprintln!("stream-recorder: could not re-aim screen capture: {e:#}");
            }
        }
    }

    /// Move across the 2x2, cutting a chapter around the change if one is
    /// recording.
    ///
    /// Unlike a move or a zoom, this changes the layout slot and therefore the
    /// output size, so the current chapter's writer cannot be kept. The cut is
    /// **forced by `AVAssetWriterInput` locking its dimensions**, not a UX
    /// choice.
    ///
    /// Restarting the stream rather than updating it when the layout crosses
    /// between having a screen and not: `updateConfiguration` cannot express
    /// "stop", and a talking-head chapter should not have a live `SCStream`
    /// burning an encoder on frames nothing will read.
    pub(super) fn apply_layout_change(&mut self) {
        let wants = self.screen_wanted().is_some();
        let capture = self.current_capture();

        // Nothing is recording: reconfigure in place, no chapter involved.
        if self.router.is_none() {
            match (wants, self.screen.is_some()) {
                (true, true) => self.apply_region_change(),
                (true, false) => self.open_screen(),
                (false, true) => self.stop_screen(),
                (false, false) => {}
            }
            self.announce_layout();
            self.sync_controls();
            self.sync_overlay();
            self.install_preview();
            return;
        }

        // Recording: finish this chapter, change the stream while no writer is
        // installed, open the next one. Safe to do without waiting for a human
        // — unlike a device switch — because nothing here touches the capture
        // session or the audio device, so no audio format can move underneath
        // the replacement writer. See the module docs for why that distinction
        // is the whole reason `switch_devices` behaves differently.
        let reopened = {
            let router = self.router.as_mut().expect("checked above");
            router.set_pair(self.pair);
            let screen = &mut self.screen;
            let screen_uid = self.screen_uid.clone();
            router.reopen_with_screen(|| {
                match (wants, screen.is_some()) {
                    (true, true) => screen.as_mut().expect("checked").set_capture(capture)?,
                    (true, false) => {
                        let uid = screen_uid.context("no display selected")?;
                        let connection = ScreenConnection::start_capture(&uid, capture)?;
                        // Not fatal: an idle display legitimately delivers
                        // nothing until something on it changes.
                        let _ = connection.wait_for_warmup(Duration::from_secs(2));
                        *screen = Some(connection);
                    }
                    (false, true) => {
                        if let Some(connection) = screen.take() {
                            connection.stop()?;
                        }
                    }
                    (false, false) => {}
                }
                screen_track(screen.as_ref())
            })
        };

        match reopened {
            Ok(()) => {
                let router = self.router.as_ref().expect("still recording");
                println!(
                    "stream-recorder: {} — chapter {} open",
                    self.layout().label(),
                    router.current_chapter_number(),
                );
            }
            Err(e) => {
                // The finished chapter is safely on disk; recovery is the same
                // as every other screen failure — drop the Router and let the
                // next New Chapter press build a fresh one.
                eprintln!("stream-recorder: layout change failed: {e:#}");
                if let Some(router) = self.router.as_ref() {
                    self.next_chapter = router.current_chapter_number() + 1;
                }
                self.router = None;
            }
        }
        self.announce_layout();
        self.sync_controls();
        self.sync_overlay();
        self.install_preview();
    }

    pub(super) fn announce_layout(&self) {
        let layout = self.layout();
        match (slot_output(layout), self.regions.get(layout.block)) {
            (Some(output), Some(resolved)) => {
                let source = self
                    .geometry
                    .map(|geometry| resolved.rect.pixels(geometry.scale()));
                println!(
                    "stream-recorder: layout {} ({}) — writing {}×{} from {} screen pixels \
                     at ({:.0}, {:.0}), zoom {:.2}×",
                    layout.label(),
                    layout.block,
                    output.w,
                    output.h,
                    match source {
                        Some(source) => format!("{}×{}", source.w, source.h),
                        None => "an unknown number of".to_string(),
                    },
                    resolved.rect.x,
                    resolved.rect.y,
                    resolved.effective_zoom,
                );
            }
            (Some(_), None) => println!(
                "stream-recorder: layout {} ({}) — no display selected, so no screen file",
                layout.label(),
                layout.block,
            ),
            (None, _) => println!(
                "stream-recorder: layout {} ({}) — camera only, no screen capture",
                layout.label(),
                layout.block,
            ),
        }
    }
}
