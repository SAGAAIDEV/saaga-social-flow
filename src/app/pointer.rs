//! The Track Mouse switch: what flipping it does, and the one thing it changes
//! that the operator did not ask for.
//!
//! Split out of [`super`] alongside [`super::face`], and shorter than it by
//! the whole of a loading state. Face tracking has to model Off→Loading→On
//! because its detector fetches 34 MB and compiles a model graph; there is
//! nothing to load here, so this switch is a plain boolean and the tracker is
//! built on the click.
//!
//! ## Why switching it on can widen your region
//!
//! The punch-in stops at one buffer pixel per output pixel, so it only has
//! somewhere to go if the region covers *more* of the display than the file
//! it is written to. At the default `Placement::zoom` of 1.0 it covers exactly
//! that and not a pixel more: the vertical region resolves to 1080×1280,
//! writes a 1080×1280 file, and there is no room either to slide or to zoom.
//! Every existing config is in that state.
//!
//! So the first switch-on with no headroom raises the vertical region's own
//! zoom to [`MouseTracking::rest_zoom`] and says so. That is a real change to
//! something the operator authored, and it is worth being uneasy about — but
//! the alternatives are worse. Leaving it alone ships a feature that does
//! nothing until you happen to drag the region wider, with no hint that is
//! what it wants. Overriding the framing per frame without saving it would
//! make the recording disagree with the overlay.
//!
//! The distinction that keeps this honest: it happens **once**, on an explicit
//! toggle, it is announced, it is saved where a drag would have saved it, and
//! dragging the region afterwards overrides it for good. Tracking itself still
//! never touches the region — which is the invariant the whole design rests
//! on, because it is what holds `App::union_capture` still while the frame
//! moves.

use std::sync::Arc;

use crate::app::App;
use crate::config::SavedPlacement;
use crate::layouts::{Layout, Orientation, Pair};
use crate::ops::graphs::Pointing;
use crate::pointer::PointerTracker;
use crate::region::placement::Placement;

impl App {
    /// What to build the preview graph's pointer node from: `Some` only when
    /// the operator asked for it *and* there is a display, a resolved vertical
    /// region, and a tracker to put the readings in.
    ///
    /// Rebuilt per call rather than cached, like [`App::tracking`] and for the
    /// same reason — a cached copy would be a second place for "is tracking
    /// on" to be true, and a stale region in it would crop the wrong part of
    /// the buffer.
    pub(super) fn pointing(&self) -> Option<Pointing> {
        if !self.pointer_config.enabled {
            return None;
        }
        let tracker = self.pointer_tracker.as_ref()?;
        Some(Pointing {
            tracker: Arc::clone(tracker),
            geometry: self.geometry?,
            // What the stream is actually capturing, not either crop: the
            // anchor is published in this space so both orientations can read
            // one reading. See `region::framing::Track`.
            capture: self.current_capture()?.region,
        })
    }

    /// What the overlay needs to draw the frame tracking is actually
    /// recording: the shared cell, and the 1:1 stop expressed in **points**.
    ///
    /// The composite applies that same stop in buffer pixels. Both come from
    /// the same slot through `base_size`, so the border drawn on screen and
    /// the rect written to the file stop punching in at the same place — which
    /// is the only way the overlay's promise that "the border *is* the frame"
    /// survives this feature.
    pub(super) fn overlay_tracking(&self) -> Option<crate::overlay::Tracked> {
        if !self.pointer_holds() {
            return None;
        }
        let tracker = self.pointer_tracker.as_ref()?;
        let geometry = self.geometry?;
        let mut floors = [(0.0, 0.0); 2];
        for orientation in Orientation::ALL {
            let layout = Layout::get(Pair::Split, orientation);
            let output = crate::app::resolve::slot_output(layout)?;
            floors[orientation as usize] =
                crate::region::placement::base_size(output, &geometry);
        }
        Some(crate::overlay::Tracked {
            cell: tracker.cell(),
            capture: self.current_capture()?.region,
            floors,
        })
    }

    /// Whether tracking currently has hold of the frames, and so whether they
    /// should offer anything to grab.
    ///
    /// Both orientations or neither: they punch in on the same point through
    /// different slots, so there is no state where one is being driven and the
    /// other is free to drag.
    pub(super) fn pointer_holds(&self) -> bool {
        self.pair == Pair::Split
            && self.pointer_config.enabled
            && self.pointer_tracker.is_some()
    }

    pub(super) fn mouse_tracking_wanted(&self) -> bool {
        self.pointer_config.enabled
    }

    /// Build the tracker if the saved setting wants one and there is not one
    /// already.
    ///
    /// Called at launch as well as from the switch, so a session that starts
    /// with tracking on tracks from the first frame. Deliberately does *not*
    /// widen the region the way [`set_mouse_tracking`](App::set_mouse_tracking)
    /// does: widening is a response to an operator flipping a switch, not
    /// something an app should do to a saved config every time it opens.
    pub(super) fn ensure_pointer_tracker(&mut self) {
        if !self.pointer_config.enabled || self.pointer_tracker.is_some() {
            return;
        }
        self.pointer_tracker = Some(Arc::new(PointerTracker::new(&self.pointer_config)));
    }

    /// The Track Mouse checkbox moved.
    pub(super) fn set_mouse_tracking(&mut self, on: bool) {
        if self.pointer_config.enabled == on {
            return;
        }
        self.pointer_config.enabled = on;

        if on {
            self.pointer_tracker = Some(Arc::new(PointerTracker::new(&self.pointer_config)));
            self.widen_for_headroom();
        } else {
            // Report before dropping. These counters are the only evidence
            // that the op ran at all: zero frames means the node was never in
            // the graph or the camera never delivered, which from outside
            // looks exactly like a feature that does not work.
            if let Some(tracker) = self.pointer_tracker.as_ref() {
                let stats = tracker.stats();
                println!(
                    "stream-recorder: mouse tracking off — {} frames, pointer on the \
                     captured display for {} of them, {} punch-ins.",
                    stats.frames, stats.found, stats.punches,
                );
            }
            // Dropped rather than parked, like the face tracker: it holds a
            // smoother whose state is only meaningful while something is
            // reading it, and a fresh one on the next switch-on is what stops
            // the frame resuming from wherever the pointer was ten minutes ago.
            self.pointer_tracker = None;
        }
        self.save_mouse_config();
        self.install_preview();
        if on {
            self.report_tracking();
        }
    }

    /// Give the regions room to punch into, and say what changed.
    ///
    /// Does nothing to a region that already has headroom, so an operator who
    /// framed wide keeps their framing exactly. Does nothing either where the
    /// display is too small to give a region its 1:1 size — a 1× 1080p display
    /// cannot fit the vertical's 1280 points of height, so `resolve` has
    /// already shrunk it and raising the zoom would only shrink it further.
    ///
    /// **Horizontal first, because the vertical is parented to it.** A child's
    /// zoom multiplies its parent's (`region::placement::resolve`), so widening
    /// the root carries the child along and one change usually buys both their
    /// headroom — with the framing *relationship* the operator set up left
    /// exactly as it was. Widening the child alone, which is what this did
    /// first, gave the vertical headroom and left the horizontal unable to
    /// punch in at all, and made the child wider than the parent it hangs off.
    fn widen_for_headroom(&mut self) {
        let (Some(geometry), Some(uid)) = (self.geometry, self.screen_uid.clone()) else {
            return;
        };
        let mut changed = false;

        for orientation in [Orientation::Horizontal, Orientation::Vertical] {
            let layout = Layout::get(Pair::Split, orientation);
            let (Some(output), Some(resolved)) = (
                crate::app::resolve::slot_output(layout),
                self.regions.get(layout.block).copied(),
            ) else {
                continue;
            };

            // "Headroom" in the only terms that matter: how many display
            // pixels feed the file. Asking it in pixels rather than points is
            // the same care `resolve_regions` takes before it warns about a
            // soft take — comparing a point size against a pixel size is the
            // confusion the `region` module exists to prevent.
            let source = resolved.rect.pixels(geometry.scale());
            if source.w > output.w && source.h > output.h {
                continue;
            }
            if resolved.clamped {
                println!(
                    "stream-recorder: {} is already as large as this display allows, so \
                     mouse tracking will follow it but not punch in. A denser display, or \
                     a smaller slot, is what would buy the zoom.",
                    layout.block,
                );
                continue;
            }

            let current = self
                .placements
                .get(layout.block)
                .copied()
                .unwrap_or_default();
            let widened = Placement {
                offset: current.offset,
                zoom: self.pointer_config.rest_zoom,
            }
            .sane();
            self.placements.insert(layout.block, widened);
            // Re-resolved inside the loop, so the child's check below sees the
            // headroom it just inherited from the parent and leaves its own
            // zoom alone.
            self.resolve_regions();
            changed = true;

            let mut cfg = crate::config::load();
            cfg.set_placement(
                &uid,
                layout.block,
                geometry.points,
                SavedPlacement {
                    offset: widened.offset,
                    zoom: widened.zoom,
                },
            );
            if let Err(e) = crate::config::save(&cfg) {
                eprintln!("stream-recorder: could not save the widened region: {e:#}");
            }
            println!(
                "stream-recorder: widened {} {:.2}× → {:.2}× so mouse tracking has room \
                 to punch in. Drag it to reframe.",
                layout.block, current.zoom, widened.zoom,
            );
        }

        if changed {
            // The regions moved, so the union moved — once, here, deliberately,
            // and never again while tracking runs.
            self.apply_region_change();
            self.sync_overlay();
        }
    }

    /// Say out loud what tracking is actually going to do.
    ///
    /// Every way this feature can be silently inert is a state the operator
    /// cannot see: no display selected, the wrong pair, a region with no room
    /// to punch into. Each of those looks identical from the outside — the
    /// switch is on and nothing happens — so the switch says which one it is
    /// rather than leaving it to be guessed at.
    fn report_tracking(&self) {
        if self.pair != Pair::Split {
            println!(
                "stream-recorder: mouse tracking is on, but only Split has a screen crop \
                 to move — switch to Split to see it."
            );
            return;
        }
        let Some(geometry) = self.geometry else {
            println!("stream-recorder: mouse tracking is on, but no display is selected.");
            return;
        };
        println!(
            "stream-recorder: mouse tracking on — hold ⌃⌥⇧ to punch in, release to widen."
        );
        let crops = self.screen_crops();
        if crops.iter().any(|crop| crop.is_none()) {
            println!(
                "stream-recorder:   WARNING no screen crop resolved ({crops:?}) — the \
                 composites will cover the whole capture and ignore tracking entirely."
            );
        }
        for orientation in [Orientation::Horizontal, Orientation::Vertical] {
            let layout = Layout::get(Pair::Split, orientation);
            let (Some(output), Some(resolved)) = (
                crate::app::resolve::slot_output(layout),
                self.regions.get(layout.block).copied(),
            ) else {
                continue;
            };
            let source = resolved.rect.pixels(geometry.scale());
            let room = source.w as f64 / output.w.max(1) as f64;
            println!(
                "stream-recorder:   {} captures {}×{} into {}×{} — {:.2}× room to punch in{}",
                layout.block,
                source.w,
                source.h,
                output.w,
                output.h,
                room,
                if room > 1.01 { "" } else { " (none: it will not zoom)" },
            );
        }
    }

    /// Persist the whole block, so a config predating a field gains it with
    /// its default the first time the switch is used.
    fn save_mouse_config(&self) {
        let mut cfg = crate::config::load();
        cfg.mouse_tracking = self.pointer_config;
        if let Err(e) = crate::config::save(&cfg) {
            eprintln!("stream-recorder: could not save the mouse-tracking setting: {e:#}");
        }
    }
}
