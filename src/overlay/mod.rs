//! The region overlay: a borderless window over the captured display that
//! draws where each layout's screen crop sits, and lets the active one be
//! dragged.
//!
//! Without this, a region is a number in a config file and the first look at
//! the framing is the edit. With it, the border on screen *is* the frame — what
//! it encloses is what the composition receives, pixel for pixel.
//!
//! ## It is not in the recording
//!
//! Two independent mechanisms, applied together because they fail differently.
//! [`NSWindowSharingType::None`] makes the window uncapturable by anything at
//! all, ours or a screen-share on the same desktop, and it is in force from the
//! moment the window is created. `SCContentFilter`'s window exclusion
//! (`screen_stream::content_filter`) covers the same ground from the capture
//! side, but only as of the `SCShareableContent` snapshot its filter was built
//! from — which is why `ScreenConnection::refresh_exclusions` exists and why
//! this window's own setting is the one that holds on the first frame.
//!
//! ## One coordinate space, on purpose
//!
//! The view overrides `isFlipped` to `true` and the window covers the whole
//! display, so the view's coordinates *are* [`PointRect`]'s display-local
//! space: top-left origin, y down, points. Nothing inside `drawRect:` or the
//! mouse handlers converts anything. The single conversion into AppKit's
//! bottom-left global space happens once, when the window frame is computed —
//! see [`PointRect::to_appkit`] and the three-space explanation in
//! [`crate::region`].
//!
//! ## Panics here abort the process
//!
//! `define_class!` emits `extern "C-unwind"` with no `catch_unwind`, so a panic
//! inside `drawRect:` or `mouseDragged:` unwinds through AppKit off a stack
//! Rust never created — the same hazard the op graph documents at
//! `ops::Graph::run`, and with a worse outcome here because there is no writer
//! state to salvage. Every access to the region list is therefore a `get`, and
//! every `RefCell` access is a `try_borrow`: an overlay that quietly stops
//! drawing is recoverable, an aborted recorder is not.

mod draw;
mod hit;

use std::cell::RefCell;
use std::sync::mpsc::Sender;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSStatusWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowSharingType, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::layouts::Orientation;
use crate::region::{DisplayGeometry, PointRect};
use crate::ui::UiEvent;
use draw::draw;
use hit::{grab_anywhere, Drag, Grabbed};

/// How a region follows another one during a drag.
///
/// Both fields are ratios against the parent's *current* rect, so applying them
/// to a moved or resized parent reproduces the relationship without the overlay
/// knowing anything about layouts, slots or zoom. The real parenting model
/// lives in [`crate::region::placement`]; this is the same arithmetic reduced
/// to what a mouse drag needs to stay live at 60Hz.
#[derive(Debug, Clone, Copy)]
pub struct ChildLink {
    /// Index into the drawn regions of the parent.
    pub parent: usize,
    /// Origin offset, normalized to the parent's width and height.
    pub offset: (f64, f64),
    /// Child size over parent size, per axis.
    pub size_ratio: (f64, f64),
}

/// One region the overlay draws.
#[derive(Debug, Clone, Copy)]
pub struct DrawnRegion {
    /// Which half of the pair this is — also its label and its identity in the
    /// [`UiEvent::RegionPlaced`] a drag emits.
    pub orientation: Orientation,
    pub rect: PointRect,
    /// The size this region's file is written at, shown in the label. Fixed by
    /// the layout slot, so it does *not* change as the region is zoomed — which
    /// is exactly what makes zooming safe mid-chapter.
    pub pixels: (usize, usize),
    /// Effective zoom, shown next to the size. It is the whole story of the
    /// sharpness trade in one number: 1.00x is pixel-exact, above that trades
    /// sharpness for context, below it upscales and only costs.
    pub zoom: f64,
    /// The narrowest this region may be resized to, in points — the same floor
    /// `MIN_ZOOM` imposes on a committed placement. Passing it down is what
    /// keeps a drag from stopping at one size and springing to another when the
    /// mouse is released.
    pub min_width: f64,
    /// Set when this region follows another, i.e. on Split-Vertical.
    pub child_of: Option<ChildLink>,
}

/// One colour per frame, because the whole point of drawing both at once is
/// telling them apart — and at the default placement they are drawn on top of
/// each other: Split-Vertical starts on Split-Horizontal's top-left corner, so
/// two same-coloured rectangles sharing an origin is exactly the state the
/// overlay opens in.
///
/// Horizontal keeps SAAGA orange (`#eb5201`), the accent every layout already
/// uses. Vertical gets sky blue (`#2ab7ff`): about as far from orange as the
/// wheel allows while staying clear of the gnomon's red X and green Y arms,
/// which are painted over both frames and would otherwise read as part of a
/// border.
///
/// The colour says *which frame*, never *which one is recording* — that is the
/// solid-versus-dashed border and the dot on the label. A colour that meant
/// both would leave nothing identifying the frame you are not recording.
const HORIZONTAL: (f64, f64, f64) = (0.922, 0.322, 0.004);
const VERTICAL: (f64, f64, f64) = (0.165, 0.718, 1.0);

/// Side of the square grab handle drawn at each corner of every region.
const HANDLE: f64 = 18.0;

struct OverlayState {
    geometry: DisplayGeometry,
    regions: Vec<DrawnRegion>,
    /// Index into `regions` of the one being recorded. Not the only one that
    /// can be moved or resized — every region is grabbable — but the one drawn
    /// solid, painted last, and given the tie when two handles coincide. Out of
    /// range means "none active", which is how a talking-head layout is drawn.
    active: usize,
    grabbed: Option<Grabbed>,
    /// Mouse tracking's live framing, when it is engaged.
    tracking: Option<Tracked>,
}

/// What the overlay needs to draw the frame mouse tracking is *actually*
/// recording, rather than the region it is recording inside of.
///
/// The overlay holds the cell and reads it in `drawRect:` rather than being
/// handed a rect. That is deliberate: the alternative is pushing a new rect
/// through [`RegionOverlay::set_regions`] on every tick, and that call clears
/// `grabbed` — sixty times a second, which would make a drag impossible to
/// start anywhere on the display for as long as tracking was on.
pub struct Tracked {
    pub cell: std::sync::Arc<crate::region::framing::TrackCell>,
    /// What the screen stream is capturing, display-local points — the space
    /// the published anchor is normalized to.
    pub capture: PointRect,
    /// The 1:1 stop for each orientation, in points, indexed by
    /// `orientation as usize`. The same limit each composite applies in buffer
    /// pixels, so what is drawn and what is recorded agree.
    pub floors: [(f64, f64); 2],
}

impl OverlayState {
    /// The rect this frame is recording *right now*.
    ///
    /// For an untracked frame that is simply its own rect. For the tracked one
    /// it is the sub-rect the pointer has pulled it to, computed with the very
    /// same function the composite crops with, so the border on screen keeps
    /// meaning what this module's header says it means: what it encloses is
    /// what the composition receives. Drawing the authored region instead
    /// would be drawing a frame that is not the frame.
    pub(super) fn recording_rect(&self, region: &DrawnRegion) -> PointRect {
        let Some(tracked) = self.tracking.as_ref() else {
            return region.rect;
        };
        let Some(track) = tracked.cell.get() else {
            return region.rect;
        };
        if !(region.rect.w > 0.0 && region.rect.h > 0.0) {
            return region.rect;
        }
        // The anchor is normalized to the capture, which is both where the
        // pointer is and the envelope the frame travels in — the same two
        // things `Composite::screen_crop` uses, in points instead of buffer
        // pixels, so what is drawn and what is recorded cannot disagree.
        let pointer = (
            tracked.capture.x + track.anchor.0 * tracked.capture.w,
            tracked.capture.y + track.anchor.1 * tracked.capture.h,
        );
        let (x, y, w, h) = crate::region::track::tracked_crop(
            (region.rect.x, region.rect.y, region.rect.w, region.rect.h),
            (
                tracked.capture.x,
                tracked.capture.y,
                tracked.capture.w,
                tracked.capture.h,
            ),
            pointer,
            track.punch,
            tracked.floors[region.orientation as usize],
        );
        PointRect { x, y, w, h }
    }

    /// Re-place every region that follows `parent` after it has moved.
    ///
    /// Runs inside the drag rather than round-tripping through the app: the
    /// event queue is drained on a 60Hz tick, so asking the app to re-resolve
    /// would put a frame of lag between the parent and its child and make the
    /// two look unrelated exactly while you are aiming them.
    fn reflow_children(&mut self, parent: usize) {
        let Some(anchor) = self.regions.get(parent).map(|region| region.rect) else {
            return;
        };
        for index in 0..self.regions.len() {
            let Some(link) = self.regions[index].child_of else {
                continue;
            };
            if link.parent != parent {
                continue;
            }
            self.regions[index].rect = PointRect {
                x: anchor.x + link.offset.0 * anchor.w,
                y: anchor.y + link.offset.1 * anchor.h,
                w: anchor.w * link.size_ratio.0,
                h: anchor.h * link.size_ratio.1,
            };
        }
    }
}

pub struct RegionViewIvars {
    tx: Sender<UiEvent>,
    state: RefCell<OverlayState>,
}

define_class!(
    // SAFETY:
    // - NSView's only subclassing requirement is that instances are used on
    //   the main thread, which `MainThreadOnly` (inherited from the
    //   superclass) enforces at the type level.
    // - `RegionView` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[ivars = RegionViewIvars]
    pub struct RegionView;

    unsafe impl NSObjectProtocol for RegionView {}

    impl RegionView {
        /// Top-left origin with y growing down — i.e. exactly `PointRect`'s
        /// display-local space, so nothing in this file converts coordinates.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Ok(state) = self.ivars().state.try_borrow() else {
                return;
            };
            draw(&state);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.point_in_view(event);
            let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                return;
            };
            // Every region is grabbable, not just the one being recorded: both
            // orientations are framed in the same pass, and having to switch
            // layout to nudge the other one would mean cutting a chapter to do
            // it.
            state.grabbed = grab_anywhere(&state.regions, state.active, point);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let point = self.point_in_view(event);
            let moved = {
                let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                    return;
                };
                let Some(Grabbed { region: index, drag }) = state.grabbed else {
                    return;
                };
                let geometry = state.geometry;
                let Some(region) = state.regions.get_mut(index) else {
                    return;
                };
                let next = match drag {
                    Drag::Move { grab } => region
                        .rect
                        .moved_to((point.0 - grab.0, point.1 - grab.1), &geometry),
                    Drag::Axis { axis, grab } => region.rect.moved_on(
                        axis,
                        (point.0 - grab.0, point.1 - grab.1),
                        &geometry,
                    ),
                    Drag::Resize { corner } => {
                        region.rect.resized(corner, point, region.min_width, &geometry)
                    }
                };
                let changed = next != region.rect;
                region.rect = next;
                if changed {
                    state.reflow_children(index);
                }
                changed
            };
            if moved {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            // Committed on release, not per drag frame: the app persists every
            // committed placement and re-aims the capture, and doing that on
            // each mouse-move would write the config file hundreds of times
            // across one drag.
            let committed = {
                let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                    return;
                };
                let Some(Grabbed { region: index, .. }) = state.grabbed.take() else {
                    return;
                };
                state
                    .regions
                    .get(index)
                    .map(|region| (region.orientation, region.rect))
            };
            if let Some((orientation, rect)) = committed {
                let _ = self
                    .ivars()
                    .tx
                    .send(UiEvent::RegionPlaced { orientation, rect });
            }
        }
    }
);

impl RegionView {
    fn new(mtm: MainThreadMarker, frame: NSRect, ivars: RegionViewIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// An event's location in this view's coordinates, which with `isFlipped`
    /// are display-local points.
    fn point_in_view(&self, event: &NSEvent) -> (f64, f64) {
        let window_point = event.locationInWindow();
        let local = self.convertPoint_fromView(window_point, None);
        (local.x, local.y)
    }
}

/// The overlay window for one display.
pub struct RegionOverlay {
    window: Retained<NSWindow>,
    view: Retained<RegionView>,
    visible: bool,
    /// Whether the window is currently accepting clicks. Mirrored here so the
    /// per-tick update is a comparison rather than an Objective-C message on
    /// every one of the 60 ticks a second.
    interactive: bool,
    /// The tracked frame as of the last repaint, so the per-tick check can ask
    /// "has it moved" instead of repainting unconditionally.
    last_tracked: Option<PointRect>,
}

impl RegionOverlay {
    /// Build the overlay, hidden. Covers `geometry`'s display exactly.
    pub fn new(
        mtm: MainThreadMarker,
        geometry: DisplayGeometry,
        tx: Sender<UiEvent>,
    ) -> RegionOverlay {
        // The whole display, expressed in AppKit's bottom-left global space.
        // This is the only coordinate conversion in the overlay.
        let full = PointRect {
            x: 0.0,
            y: 0.0,
            w: geometry.points.0,
            h: geometry.points.1,
        };
        let frame = full.to_appkit(&geometry);

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // `initWithContentRect:` leaves `releasedWhenClosed` at YES, which
        // makes `close` release a reference AppKit never took — the `init`
        // above handed its only +1 to the `Retained`. winit closes every window
        // in `[NSApp windows]`, ours included, as `run_app` returns, so that
        // over-release frees the window a moment before `Live` is dropped and
        // `Drop` below messages freed memory: a panic on a NULL isa, then a
        // segfault, on every single quit.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setLevel(NSStatusWindowLevel);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        // Click-through until proven otherwise. A window this size covering the
        // whole display would otherwise eat every click on the machine for as
        // long as it is visible — including on the very windows being framed.
        // `track_cursor` turns this off for the moments the cursor is actually
        // over a handle.
        window.setIgnoresMouseEvents(true);
        // The load-bearing one: uncapturable by any screen recorder, including
        // ours, from creation. See the module docs.
        window.setSharingType(NSWindowSharingType::None);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );

        let view = RegionView::new(
            mtm,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(full.w, full.h)),
            RegionViewIvars {
                tx,
                state: RefCell::new(OverlayState {
                    geometry,
                    regions: Vec::new(),
                    active: usize::MAX,
                    grabbed: None,
                    tracking: None,
                }),
            },
        );
        window.setContentView(Some(&view));

        RegionOverlay {
            window,
            view,
            visible: false,
            interactive: false,
            last_tracked: None,
        }
    }

    /// Replace what the overlay draws.
    ///
    /// `active` indexes `regions`; anything out of range draws every region as
    /// inactive and makes none of them draggable, which is what a layout with
    /// no screen slot wants.
    pub fn set_regions(&self, regions: Vec<DrawnRegion>, active: usize) {
        if let Ok(mut state) = self.view.ivars().state.try_borrow_mut() {
            state.regions = regions;
            state.active = active;
            state.grabbed = None;
        }
        self.view.setNeedsDisplay(true);
    }

    /// Point the overlay at mouse tracking's live framing, or `None` when it
    /// is off.
    ///
    /// Event-driven like [`set_regions`](RegionOverlay::set_regions) — the
    /// *cell* is what changes per frame, not this.
    pub fn set_tracking(&self, tracking: Option<Tracked>) {
        if let Ok(mut state) = self.view.ivars().state.try_borrow_mut() {
            state.tracking = tracking;
        }
        self.view.setNeedsDisplay(true);
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Let clicks through unless the cursor is on a handle.
    ///
    /// Called from the app's existing ~60Hz tick. Polling the cursor rather
    /// than installing an `NSEvent` global monitor: the tick already exists, a
    /// monitor would be a second event path to keep in step with this one, and
    /// at 16ms the flip lands long before a human can press the button they
    /// just moved onto.
    ///
    /// The cursor is converted through the *same* path a real click takes —
    /// screen to window to flipped view — rather than through an independent
    /// bit of coordinate arithmetic. Two implementations of that conversion
    /// could disagree, and the failure would be a handle that highlights but
    /// cannot be pressed, or worse, a window that stops passing clicks through
    /// over a handle that is not there.
    pub fn track_cursor(&mut self) {
        if !self.visible {
            return;
        }
        let in_window = self.window.convertPointFromScreen(NSEvent::mouseLocation());
        let local = self.view.convertPoint_fromView(in_window, None);

        let Ok(state) = self.view.ivars().state.try_borrow() else {
            return;
        };
        // Stay interactive for the whole of a drag: a resize routinely pulls
        // the cursor off the handle it started on, and going click-through
        // mid-gesture would drop the drag on the floor.
        let wanted = state.grabbed.is_some()
            || grab_anywhere(&state.regions, state.active, (local.x, local.y)).is_some();
        // The tracked frame moves without any event to redraw on, so this tick
        // is where it gets repainted — but only when it has actually moved.
        // The alternative is a full-display transparent-window repaint at
        // 60 Hz, and `drawRect:` measures its label text on every pass.
        let moved = state
            .tracking
            .as_ref()
            .and_then(|_| {
                // Any frame will do as the movement probe: one reading drives
                // both, so if the vertical has not moved the horizontal has not
                // either.
                let region = state
                    .regions
                    .iter()
                    .find(|region| region.orientation == Orientation::Vertical)?;
                Some(state.recording_rect(region))
            })
            .filter(|rect| self.last_tracked != Some(*rect));
        drop(state);

        if let Some(rect) = moved {
            self.last_tracked = Some(rect);
            self.view.setNeedsDisplay(true);
        }

        if wanted != self.interactive {
            self.window.setIgnoresMouseEvents(!wanted);
            self.interactive = wanted;
        }
    }

    /// Show without activating: `orderFrontRegardless` rather than
    /// `makeKeyAndOrderFront`, so bringing the overlay up does not steal focus
    /// from whatever is being demonstrated. Mouse events still arrive — key
    /// status governs the keyboard, not the mouse.
    pub fn show(&mut self) {
        self.window.orderFrontRegardless();
        self.view.setNeedsDisplay(true);
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.window.orderOut(None::<&AnyObject>);
        self.visible = false;
        // Left click-through on the way out, so a hidden overlay can never be
        // the thing eating a click.
        self.window.setIgnoresMouseEvents(true);
        self.interactive = false;
    }
}

impl Drop for RegionOverlay {
    fn drop(&mut self) {
        // An ordered-in window outlives its Rust handle otherwise: AppKit's
        // window list holds its own reference, so a dropped overlay would stay
        // on screen forever with nothing left to hide it.
        //
        // There is a live window here to message only because `new` cleared
        // `releasedWhenClosed`; and this runs on the main thread without a
        // check because `NSWindow` is `MainThreadOnly`, which makes
        // `RegionOverlay` `!Send` and pins the drop to the thread `new` was
        // given a `MainThreadMarker` on. Both are load-bearing — see `new`.
        self.window.orderOut(None::<&AnyObject>);
    }
}

#[cfg(test)]
mod tests;
