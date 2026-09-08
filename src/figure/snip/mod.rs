//! The drag-to-select overlay: ⌃⇧S, then draw a rectangle over what you want.
//!
//! Built on the same bones as [`crate::overlay`] — a borderless window covering
//! one display, a flipped view so its coordinates *are* [`PointRect`]'s
//! display-local space, and [`NSWindowSharingType::None`] so the window cannot
//! be captured by anything, ours included. What differs is what the two windows
//! want from the mouse. The region overlay is click-through except over a
//! handle, because it sits on top of the app being demonstrated for minutes at
//! a time. This one deliberately swallows every click: for the second or two it
//! is up, it *is* the interaction.
//!
//! ## Why it does not take focus
//!
//! [`orderFrontRegardless`] rather than `makeKeyAndOrderFront`, and no
//! `canBecomeKeyWindow` override. Activating this app would repaint the window
//! being photographed — an inactive title bar, a lost focus ring, a dimmed
//! selection — and the whole point of a figure is that it shows the screen as
//! the viewer saw it. The cost is that Escape never arrives, because a
//! non-key window gets no `keyDown:`, which is why cancelling is a right-click
//! or a click that does not travel.
//!
//! Refusing focus is also why the view carries an [`NSTrackingArea`] rather
//! than relying on `setAcceptsMouseMovedEvents:`. AppKit routes mouse-moved
//! events to the *key* window's first responder, so a window that will not
//! become key never sees one — the crosshair guides would simply never appear.
//! A tracking area with `ActiveAlways` is the documented way to have them
//! delivered to one view regardless. Presses need none of this: a mouse-down
//! goes to whatever window is under the pointer.
//!
//! ## Why the selection is a hole, not a rectangle
//!
//! The dim is painted as the four rects *around* the selection rather than as
//! one rect with a lighter one on top. It reads better — what you have selected
//! is shown at full brightness, exactly as it will be captured — and it means
//! that even if the window server were to composite this overlay into the
//! screenshot despite the sharing type, the captured area contains nothing of
//! ours to composite.
//!
//! ## The box is what you drag
//!
//! Figures are published at their own size — see [`crate::figure::encode`] — so
//! the drag chooses the rectangle itself, shape and all. It runs from the press
//! to the pointer whichever way the hand went, and stops at the display's edge,
//! because a rect past it is one ScreenCaptureKit answers with nothing. The
//! label shows the pixels under it. See [`drag_rect`].
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | the window, the view, and what a drag means |
//! | [`draw`] | the dim, the hole, the guides and the labels |
//!
//! ## Panics here abort the process
//!
//! Same rule as [`crate::overlay`]: everything below runs inside `drawRect:` or
//! a mouse handler, where a panic unwinds into Objective-C. Index with `get`,
//! borrow with `try_borrow`, and never unwrap.

use std::cell::RefCell;
use std::sync::mpsc::Sender;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSCursor, NSEvent, NSScreenSaverWindowLevel, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowSharingType,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::region::{DisplayGeometry, PointRect};
use crate::ui::UiEvent;

mod draw;
#[cfg(test)]
mod tests;

use draw::draw;

/// Smallest selection that counts as one, in points on either axis.
///
/// Below this a drag is a click, and a click cancels. Two reasons for the floor
/// rather than accepting any non-empty rect: a stray click on the overlay would
/// otherwise capture a 2×3-pixel image and file it as a figure, and a
/// deliberate cancel needs *some* gesture now that Escape cannot reach a
/// window that refuses focus.
const MIN_DRAG: f64 = 12.0;

/// How dark the unselected screen goes. Enough to make the selection obvious,
/// not so much that you cannot see what you are aiming at.
pub(super) const DIM: f64 = 0.45;

pub(super) struct SnipState {
    pub(super) geometry: DisplayGeometry,
    /// Where the press landed, in display-local points. `None` before the first
    /// press and after a cancel.
    anchor: Option<(f64, f64)>,
    /// Where the mouse is now — tracked even before a press, so the crosshair
    /// guides can be drawn while aiming.
    pub(super) cursor: Option<(f64, f64)>,
}

impl SnipState {
    /// The selection as it stands: the rectangle from the press to the
    /// pointer, held inside the display — see [`drag_rect`].
    pub(super) fn selection(&self) -> Option<PointRect> {
        let anchor = self.anchor?;
        let cursor = self.cursor?;
        Some(drag_rect(anchor, cursor, &self.geometry))
    }
}

/// The rectangle a drag from `anchor` to `cursor` selects, held inside the
/// display.
///
/// Any shape. A figure is published at the size it was dragged at — see
/// [`crate::figure::encode`] — so the box is exactly the two corners the hand
/// named, normalised so that dragging up-left and dragging down-right are the
/// same gesture. Both corners are clamped to the display rather than the box
/// being shrunk to a shape, because there is no shape to keep: the pointer is
/// not confined to the window, and a source rect past the display's edge is one
/// ScreenCaptureKit answers with nothing, so the edge is simply where the
/// selection stops.
fn drag_rect(anchor: (f64, f64), cursor: (f64, f64), geometry: &DisplayGeometry) -> PointRect {
    let (max_w, max_h) = geometry.points;
    let ax = anchor.0.clamp(0.0, max_w);
    let ay = anchor.1.clamp(0.0, max_h);
    let cx = cursor.0.clamp(0.0, max_w);
    let cy = cursor.1.clamp(0.0, max_h);
    PointRect {
        x: ax.min(cx),
        y: ay.min(cy),
        w: (cx - ax).abs(),
        h: (cy - ay).abs(),
    }
}

/// Whether a selection is a capture or a cancel.
fn is_capture(rect: &PointRect) -> bool {
    rect.w >= MIN_DRAG && rect.h >= MIN_DRAG
}

pub struct SnipViewIvars {
    tx: Sender<UiEvent>,
    state: RefCell<SnipState>,
}

define_class!(
    // SAFETY:
    // - NSView's only subclassing requirement is main-thread use, which
    //   `MainThreadOnly` enforces at the type level.
    // - `SnipView` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[ivars = SnipViewIvars]
    pub struct SnipView;

    unsafe impl NSObjectProtocol for SnipView {}

    impl SnipView {
        /// Top-left origin, y down — [`PointRect`]'s display-local space, so
        /// nothing in this file converts coordinates.
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

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let point = self.point_in_view(event);
            {
                let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                    return;
                };
                state.cursor = Some(point);
            }
            // Re-asserted per move: another app can set the cursor back while
            // this overlay is up, and the crosshair is what says the click will
            // be caught here rather than passed through.
            NSCursor::crosshairCursor().set();
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.point_in_view(event);
            let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                return;
            };
            state.anchor = Some(point);
            state.cursor = Some(point);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let point = self.point_in_view(event);
            {
                let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                    return;
                };
                if state.anchor.is_none() {
                    return;
                }
                state.cursor = Some(point);
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let point = self.point_in_view(event);
            let selection = {
                let Ok(mut state) = self.ivars().state.try_borrow_mut() else {
                    return;
                };
                state.cursor = Some(point);
                let selection = state.selection();
                // Cleared either way: the overlay is about to be taken down,
                // and a rect left behind would be drawn on the next open.
                state.anchor = None;
                selection
            };
            // A press that did not travel is a cancel, so a stray click on a
            // window-sized overlay costs nothing rather than filing a figure.
            let event = match selection.filter(is_capture) {
                Some(rect) => UiEvent::FigureSnipped { rect },
                None => UiEvent::FigureSnipCancelled,
            };
            let _ = self.ivars().tx.send(event);
        }

        /// Keeps the tracking area the size of the view.
        ///
        /// `InVisibleRect` makes the area track the view's bounds by itself, so
        /// this only has to run once — but AppKit calls it on every bounds
        /// change and a second area would deliver every move twice.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            let () = unsafe { msg_send![super(self), updateTrackingAreas] };
            for area in self.trackingAreas().iter() {
                self.removeTrackingArea(&area);
            }
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    NSTrackingAreaOptions::MouseMoved
                        | NSTrackingAreaOptions::MouseEnteredAndExited
                        | NSTrackingAreaOptions::ActiveAlways
                        | NSTrackingAreaOptions::InVisibleRect,
                    Some(self.as_ref()),
                    None,
                )
            };
            self.addTrackingArea(&area);
        }

        /// The cursor is only ours while it is over this window, so the
        /// crosshair is set on entry rather than once at `show`.
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            NSCursor::crosshairCursor().set();
        }

        /// The cancel that is always available. See the module docs on why it is
        /// not Escape.
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            if let Ok(mut state) = self.ivars().state.try_borrow_mut() {
                state.anchor = None;
            }
            let _ = self.ivars().tx.send(UiEvent::FigureSnipCancelled);
        }
    }
);

impl SnipView {
    fn new(mtm: MainThreadMarker, frame: NSRect, ivars: SnipViewIvars) -> Retained<Self> {
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

/// The snip window for one display, hidden until [`show`](Snip::show).
pub struct Snip {
    window: Retained<NSWindow>,
    view: Retained<SnipView>,
    visible: bool,
    /// Duplicated from the view's state, which is behind a `RefCell` that a
    /// `try_borrow` can legitimately fail on mid-draw. A `None` geometry at the
    /// wrong moment would cost the figure that was just dragged.
    geometry: DisplayGeometry,
}

impl Snip {
    pub fn new(
        mtm: MainThreadMarker,
        geometry: DisplayGeometry,
        tx: Sender<UiEvent>,
    ) -> Snip {
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
        // Same reason as `crate::overlay`: winit closes every window in
        // `[NSApp windows]` as the loop returns, and the default YES here would
        // make that `close` release a reference AppKit never took.
        unsafe { window.setReleasedWhenClosed(false) };
        // Above the region overlay, which sits at status level. The dim has to
        // cover everything or it stops reading as a modal gesture.
        window.setLevel(NSScreenSaverWindowLevel);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        // The one place this differs from the region overlay: for the second it
        // is up, every click on this display belongs to the snip.
        window.setIgnoresMouseEvents(false);
        // Guides need the pointer's position before any button goes down.
        window.setAcceptsMouseMovedEvents(true);
        // Uncapturable by any screen recorder, ours included, from creation.
        window.setSharingType(NSWindowSharingType::None);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );

        let view = SnipView::new(
            mtm,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(full.w, full.h)),
            SnipViewIvars {
                tx,
                state: RefCell::new(SnipState {
                    geometry,
                    anchor: None,
                    cursor: None,
                }),
            },
        );
        window.setContentView(Some(&view));

        Snip {
            window,
            view,
            visible: false,
            geometry,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The display this window covers.
    ///
    /// Held so the App can convert the rect this overlay posts without
    /// re-reading the display list — and, more to the point, so it converts it
    /// against the *same* geometry the drag happened in. A rect measured on one
    /// display and converted with another's origin names a rectangle on neither.
    pub fn geometry(&self) -> DisplayGeometry {
        self.geometry
    }

    /// Show without activating — see the module docs.
    pub fn show(&mut self) {
        if let Ok(mut state) = self.view.ivars().state.try_borrow_mut() {
            state.anchor = None;
            state.cursor = None;
        }
        self.window.orderFrontRegardless();
        self.view.setNeedsDisplay(true);
        self.visible = true;
        NSCursor::crosshairCursor().set();
    }

    pub fn hide(&mut self) {
        self.window.orderOut(None::<&AnyObject>);
        self.visible = false;
        NSCursor::arrowCursor().set();
    }
}

impl Drop for Snip {
    fn drop(&mut self) {
        // AppKit's window list holds its own reference, so a dropped handle
        // would leave a screen-covering, click-eating window on screen with
        // nothing left able to take it down.
        if self.visible {
            self.window.orderOut(None::<&AnyObject>);
        }
        self.window.close();
    }
}
