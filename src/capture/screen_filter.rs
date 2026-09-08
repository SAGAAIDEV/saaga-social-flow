//! Which display to capture, and which windows to leave out of it.
//!
//! One `SCShareableContent` query answers both, so they live together. The
//! query is an async round-trip with a ten-second timeout — expensive enough
//! that asking twice on every region change would be felt — and its completion
//! block fires on an arbitrary dispatch queue, so results are parked in a
//! shared slot rather than sent down a channel: `Retained` is not `Send` for
//! any of these types.
//!
//! The exclusion list is this process's own windows, matched by pid. The record
//! window and the region overlay are tools, not content. This used to pass an
//! empty array on the reasoning that an operator might want the record window
//! visible "as proof of what was captured"; the region border is that proof
//! now, and a recorder that films its own UI is a bug.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{SCContentFilter, SCDisplay, SCShareableContent, SCWindow};

/// A filter over one display that excludes every window this process owns.
///
/// The record window and the region overlay are tools, not content. This used
/// to pass an empty array on the reasoning that an operator might want the
/// record window visible "as proof of what was captured"; the region border is
/// that proof now, and a recorder that films its own UI is a bug.
///
/// Matched by pid rather than by window number, so it needs no bookkeeping and
/// picks up windows this process has not created yet — though only as of the
/// snapshot `own_windows` came from, which is why
/// [`ScreenConnection::refresh_exclusions`] exists.
pub(super) fn content_filter(
    display: &SCDisplay,
    own_windows: &NSArray<SCWindow>,
) -> Retained<SCContentFilter> {
    unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            display,
            own_windows,
        )
    }
}

/// The display to capture, the windows to leave out of it, and its native
/// pixel size — width, then height.
pub(super) type FoundDisplay = (
    Retained<SCDisplay>,
    Retained<NSArray<SCWindow>>,
    usize,
    usize,
);

/// Resolve a `CGDirectDisplayID` to its `SCDisplay`, this process's windows,
/// and the size to capture the whole display at.
///
/// The size comes from Core Graphics, not from `SCDisplay::width`/`height`:
/// those are in *points*, so configuring the stream with them captures a
/// Retina display at half its real resolution, and screencast text is the
/// first thing to suffer. `CGDisplayMode`'s pixel size is the native one.
pub(super) fn find_display(target: u32) -> Result<FoundDisplay> {
    let (display, own_windows) = fetch_content(target)?;

    // Core Graphics can decline to describe a display (it does for some
    // virtual and sidecar displays); fall back to SCDisplay's point size
    // rather than configuring a 0×0 stream that fails at startCapture.
    let (mut width, mut height) = super::screen::pixel_size(target).unwrap_or((0, 0));
    if width == 0 || height == 0 {
        width = unsafe { display.width() }.max(0) as usize;
        height = unsafe { display.height() }.max(0) as usize;
    }
    if width == 0 || height == 0 {
        bail!("display {target} reported no usable size");
    }
    // H.264 requires even dimensions.
    Ok((display, own_windows, width & !1, height & !1))
}

/// Ask ScreenCaptureKit for one display and for the windows this process owns.
///
/// Both come out of a single `SCShareableContent` snapshot because it is an
/// expensive async round-trip with a ten-second timeout, and asking twice would
/// double that on every region change for no benefit.
///
/// The completion block runs on an arbitrary queue, so the results are parked
/// in a shared slot rather than sent down a channel — `Retained` is not `Send`
/// for these types.
pub(super) fn fetch_content(
    target: u32,
) -> Result<(Retained<SCDisplay>, Retained<NSArray<SCWindow>>)> {
    type Found = (Retained<SCDisplay>, Retained<NSArray<SCWindow>>);

    let (tx, rx) = mpsc::channel::<std::result::Result<(), String>>();
    // The handler runs on ScreenCaptureKit's own queue, so this genuinely
    // crosses threads; the payload is an Objective-C handle this crate cannot
    // mark `Send`, which is what the lint objects to. An `Rc` would be wrong.
    #[allow(clippy::arc_with_non_send_sync)]
    let slot: Arc<Mutex<Option<Found>>> = Arc::new(Mutex::new(None));
    let sink = slot.clone();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            if let Some(error) = unsafe { error.as_ref() } {
                let _ = tx.send(Err(error.localizedDescription().to_string()));
                return;
            }
            if let Some(content) = unsafe { content.as_ref() } {
                let display = unsafe { content.displays() }
                    .iter()
                    .find(|d| unsafe { d.displayID() } == target);
                if let Some(display) = display {
                    *sink.lock().unwrap() = Some((display, own_windows(content)));
                }
            }
            let _ = tx.send(Ok(()));
        },
    );
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    rx.recv_timeout(Duration::from_secs(10))
        .context("timed out asking ScreenCaptureKit for the display list")?
        .map_err(|e| anyhow!("ScreenCaptureKit could not list displays: {e}"))?;

    let found = slot.lock().unwrap().take();
    found.ok_or_else(|| {
        anyhow!("display {target} is no longer attached — pick another in the Screen dropdown")
    })
}

/// Every shareable window owned by this process.
///
/// A window with no owning application is skipped rather than guessed at: it
/// belongs to the window server, never to us, and including it in an exclusion
/// list would hide part of somebody else's desktop from the recording.
pub(super) fn own_windows(content: &SCShareableContent) -> Retained<NSArray<SCWindow>> {
    let ours = std::process::id();
    let windows: Vec<Retained<SCWindow>> = unsafe { content.windows() }
        .iter()
        .filter(|window| {
            unsafe { window.owningApplication() }
                .is_some_and(|app| unsafe { app.processID() } as u32 == ours)
        })
        .collect();
    NSArray::from_retained_slice(&windows)
}
