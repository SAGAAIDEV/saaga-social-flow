//! Where the pointer is, in the space every region here already speaks.
//!
//! ## Why Core Graphics, and not `NSEvent::mouseLocation`
//!
//! There are two system answers to "where is the pointer", and they are in
//! different coordinate spaces. `NSEvent::mouseLocation` — the one
//! `overlay::RegionOverlay::track_cursor` already uses, because it is asking a
//! question about a *window* — is AppKit global: origin at the **primary**
//! screen's bottom-left, y growing **up**. Turning that into the display-local
//! space a [`PointRect`](crate::region::PointRect) lives in means running
//! [`PointRect::to_appkit`](crate::region::PointRect::to_appkit) backwards,
//! and [`crate::region`] refuses to have that inverse on purpose: "Adding a
//! `from_appkit` would be an untested second implementation of the same y-flip
//! waiting to disagree with this one."
//!
//! `CGEvent::location` is already in **Core Graphics global** space — origin
//! at the primary display's top-left, y growing **down**, points — which is
//! exactly [`DisplayGeometry::cg_origin`]'s space and therefore one
//! subtraction away from display-local. No flip, no primary height, no second
//! implementation of anything. That is the whole reason for the choice, and it
//! is why [`on_display`] is four lines instead of a coordinate-conversion test
//! suite.
//!
//! ## What it needs, which is nothing
//!
//! Neither call here opens an event tap, so neither needs the **Accessibility**
//! grant — the one permission this recorder has never asked for
//! ([`crate::permissions`] requests camera, microphone and screen recording,
//! and that list does not grow for this). `CGEvent::new(None)` asks the window
//! server for the current event state and `CGEventSource` counters are
//! statistics reads; both are ordinary Core Foundation calls with no main-
//! thread requirement, which matters because they run on the camera capture
//! queue.
//!
//! ## Measured: 131 ns per read
//!
//! Taken off the main thread on an M1 Max, 10,000 calls
//! (`the_pointer_is_readable_off_the_main_thread_and_costs_microseconds`).
//! That is worth stating next to the number `ops::face_track` carries for the
//! detector it sits beside — **1.5 ms** per BlazeFace call — because the ratio
//! decides a design question rather than just being a nice figure.
//!
//! Face tracking samples one frame in three (`FaceTracking::detect_every`)
//! because 1.5 ms every frame would be a real bite out of a 33 ms budget. At
//! four ten-thousandths of that, there is **no cadence to tune here**: the
//! pointer is read on every frame, `MouseTracking` has no `sample_every`
//! field, and the responsiveness-versus-cost trade that
//! `FaceTracking::detect_every`'s documentation has to explain simply does not
//! arise. A knob that would always be set to 1 is not a knob.

use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID};

use crate::region::DisplayGeometry;

/// The pointer in Core Graphics' global space: primary display's top-left
/// origin, y growing down, points.
///
/// `None` when the window server declines to answer, which it can do during a
/// fast user switch or a lock. Callers hold their last framing rather than
/// treating it as the pointer having moved to the origin — a `(0, 0)` fallback
/// would whip the frame into the corner every time the screen locked.
pub fn global() -> Option<(f64, f64)> {
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    // A non-finite coordinate has never been observed, but it would propagate
    // straight into a crop rect through the smoother's arithmetic, and the
    // house rule on a capture queue is to return rather than to trust.
    (point.x.is_finite() && point.y.is_finite()).then_some((point.x, point.y))
}

/// The pointer in `geom`'s own display-local points, or `None` when it is not
/// on that display at all.
///
/// The `None` is load-bearing on a multi-monitor desk: a pointer that has left
/// the captured display should read as *lost*, so the framing holds where it
/// was. Clamping to the nearest edge instead would have the frame slide to the
/// boundary and sit there every time the operator reached for a second screen,
/// which is motion that means nothing.
///
/// The bounds are half-open, matching [`crate::region`]'s `contains`, so two
/// displays that abut cannot both claim the same point.
pub fn on_display(geom: &DisplayGeometry) -> Option<(f64, f64)> {
    let (x, y) = global()?;
    let local = (x - geom.cg_origin.0, y - geom.cg_origin.1);
    let inside = local.0 >= 0.0
        && local.1 >= 0.0
        && local.0 < geom.points.0
        && local.1 < geom.points.1;
    inside.then_some(local)
}

/// A running count of deliberate pointer events since boot.
///
/// Only differences of this are meaningful — the absolute value is whatever
/// the machine has accumulated since it booted, and it wraps. Two samples that
/// differ mean the operator clicked or scrolled between them, which is the
/// strongest "I am looking at this" signal available without an event tap, and
/// far stronger than the pointer merely having stopped moving.
/// The modifier keys held right now, as the session sees them.
///
/// A *state* query, not an event tap: it asks the window server what is down
/// this instant rather than subscribing to key events, which is why it needs
/// no Accessibility or Input Monitoring grant and why it answers while another
/// application has focus. An event tap would give the same answer and require
/// a permission prompt, a run loop, and a story for what happens when macOS
/// disables the tap for being slow.
pub fn modifiers() -> CGEventFlags {
    CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState)
}

/// The combo that punches the vertical frame in: **⌃⌥⇧**.
///
/// Chosen against the app's existing bindings rather than for comfort.
/// `hotkeys` uses ⌃⌥ plus a letter — ⌃⌥C new chapter, ⌃⌥T retake, ⌃⌥Q quit —
/// and none of them carry shift, so this cannot fire on the way to one of
/// those and none of those can fire on the way to this. Three modifiers is
/// also deliberate: two is a combo a hand rests on by accident, and an
/// accidental punch-in lands in the recording.
pub const ZOOM_COMBO: CGEventFlags = CGEventFlags::MaskControl
    .union(CGEventFlags::MaskAlternate)
    .union(CGEventFlags::MaskShift);

/// Whether the punch-in combo is held right now.
///
/// `contains` rather than equality, so holding ⌘ as well still counts. An
/// exact match would make the gesture fail for a reason nobody could see.
pub fn zoom_held() -> bool {
    modifiers().contains(ZOOM_COMBO)
}

#[cfg(test)]
mod tests {
    use super::*;


    /// Spike: is a held modifier combo readable off the main thread, cheaply,
    /// while another application has focus?
    ///
    /// The last question is the one that matters and the one no header
    /// answers, so this samples for three seconds and prints what it saw. Run
    /// it from a terminal — which *is* another application with focus — and
    /// hold the combo while it runs.
    ///
    /// ```text
    /// cargo test pointer::sample::modifier -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "interactive: hold a modifier combo for three seconds while it runs"]
    fn a_held_modifier_combo_is_readable_off_the_main_thread() {
        println!("hold Control+Option now — sampling for 3s…");
        let handle = std::thread::spawn(|| {
            let started = std::time::Instant::now();
            let mut seen: Vec<CGEventFlags> = Vec::new();
            let mut calls = 0u32;
            let mut combo_frames = 0u32;
            let wanted = CGEventFlags::MaskControl | CGEventFlags::MaskAlternate;
            while started.elapsed() < std::time::Duration::from_secs(3) {
                let flags = modifiers();
                calls += 1;
                if flags.contains(wanted) {
                    combo_frames += 1;
                }
                if !seen.contains(&flags) {
                    seen.push(flags);
                }
            }
            let each = started.elapsed() / calls.max(1);
            (seen, each, calls, combo_frames)
        });
        let (seen, each, calls, combo_frames) =
            handle.join().expect("the sampling thread panicked");

        println!("distinct flag states seen: {seen:?}");
        println!("cost per call:             {each:?} over {calls} calls");
        println!("frames with Control+Option held: {combo_frames}");
        assert!(
            each < std::time::Duration::from_micros(100),
            "a modifier read cost {each:?}, too much to spend per frame",
        );
    }

    /// The verification spike, kept as a test rather than deleted.
    ///
    /// Three questions this feature's design rests on, none of which can be
    /// answered by reading a header: is the pointer readable off the main
    /// thread, what does it cost per call, and does it need a permission this
    /// app does not request. It runs on a spawned thread on purpose — the
    /// production caller is the camera capture queue, and "works on the main
    /// thread" would not be evidence about that.
    ///
    /// ```text
    /// cargo test pointer::sample::the_pointer -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "reads the live pointer; needs a window server, prints rather than asserts"]
    fn the_pointer_is_readable_off_the_main_thread_and_costs_microseconds() {
        let handle = std::thread::spawn(|| {
            let first = global();
            let started = std::time::Instant::now();
            const CALLS: u32 = 10_000;
            let mut last = None;
            for _ in 0..CALLS {
                last = global();
            }
            let each = started.elapsed() / CALLS;
            (first, last, each, modifiers())
        });
        let (first, last, each, flags) = handle.join().expect("the sampling thread panicked");

        println!("pointer at first read: {first:?}");
        println!("pointer at last read:  {last:?}");
        println!("cost per call:         {each:?}");
        println!("modifiers held:        {flags:?}");

        assert!(
            first.is_some(),
            "CGEvent::new(None) returned nothing off the main thread — the \
             sensor cannot live on the capture queue and has to move to the \
             main tick",
        );
        assert!(
            each < std::time::Duration::from_micros(100),
            "a pointer read cost {each:?}, which is too much to spend per \
             frame on a capture queue",
        );
    }
}
