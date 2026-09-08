//! Display enumeration for the record window's screen picker, via
//! ScreenCaptureKit.
//!
//! Displays are not `AVCaptureDevice`s, so `device_picker::list_devices` does
//! not reach them — but they are presented as the same [`CaptureDevice`]
//! `{uid, name}` pair so the window's popup builder treats all three pickers
//! identically. The uid is the `CGDirectDisplayID` rendered as a decimal
//! string; that is what a later capture step resolves back to an `SCDisplay`.
//!
//! **`CGDirectDisplayID` is not stable across reboots or re-plugging** — it is
//! assigned by the window server, unlike a camera's `uniqueID`. A saved
//! display id is therefore a best-effort default, and callers must fall back
//! gracefully when it no longer matches (see `app::resolve_display`).
//!
//! `getShareableContentWithCompletionHandler:` is asynchronous and fires its
//! block on an arbitrary dispatch queue, so this blocks the calling thread on
//! a channel — the same shape as `permissions::ensure_access`, and safe for
//! the same reason: it is only ever called from the synchronous startup flow,
//! never from inside the winit event loop.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGMainDisplayID,
};
use objc2_foundation::{NSError, NSNumber, NSString};
use objc2_screen_capture_kit::{SCDisplay, SCShareableContent};

use super::device_picker::CaptureDevice;
use crate::region::DisplayGeometry;

pub type ScreenDevice = CaptureDevice;

/// The `Send` subset of an `SCDisplay`. The `Retained<SCShareableContent>` the
/// completion block receives is not `Send`, so everything needed must be read
/// out inside the block and carried across the channel as plain data.
struct DisplayInfo {
    id: u32,
    width: isize,
    height: isize,
}

/// Every display ScreenCaptureKit will let this process capture, in the order
/// it reports them.
///
/// Returns an empty vec rather than an error when screen-recording permission
/// has not been granted: ScreenCaptureKit reports zero displays in that case
/// instead of failing, and the caller (which has already run the permission
/// check) is better placed to explain it.
pub fn list_displays() -> Result<Vec<ScreenDevice>> {
    let infos = fetch_display_infos()?;
    let names = screen_names();
    Ok(infos
        .into_iter()
        .enumerate()
        .map(|(i, info)| ScreenDevice {
            uid: info.id.to_string(),
            name: display_name(&names, &info, i),
        })
        .collect())
}

/// Block until ScreenCaptureKit answers with its shareable content.
fn fetch_display_infos() -> Result<Vec<DisplayInfo>> {
    let (tx, rx) = mpsc::channel::<std::result::Result<Vec<DisplayInfo>, String>>();
    // `dyn Fn`, not `FnOnce`: `Sender::send` takes `&self`, so the block stays
    // callable — ScreenCaptureKit only invokes it once regardless.
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if let Some(error) = unsafe { error.as_ref() } {
                Err(error.localizedDescription().to_string())
            } else if let Some(content) = unsafe { content.as_ref() } {
                Ok(read_displays(content))
            } else {
                Err("ScreenCaptureKit returned neither content nor an error".to_string())
            };
            let _ = tx.send(result);
        },
    );
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };

    rx.recv_timeout(Duration::from_secs(10))
        .context("timed out asking ScreenCaptureKit for the display list")?
        .map_err(|e| anyhow!("ScreenCaptureKit could not list displays: {e}"))
}

fn read_displays(content: &SCShareableContent) -> Vec<DisplayInfo> {
    let displays: Retained<objc2_foundation::NSArray<SCDisplay>> = unsafe { content.displays() };
    displays
        .iter()
        .map(|d| DisplayInfo {
            id: unsafe { d.displayID() },
            width: unsafe { d.width() },
            height: unsafe { d.height() },
        })
        .collect()
}

/// `(CGDirectDisplayID, localized name)` for every attached screen.
///
/// `SCDisplay` exposes no name of its own, but `NSScreen` does, and the two
/// are joined by the display id `NSScreen` publishes under `NSScreenNumber`.
/// AppKit requires the main thread; off it this yields nothing and the caller
/// falls back to a synthesized name.
fn screen_names() -> Vec<(u32, String)> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let key = NSString::from_str("NSScreenNumber");
    NSScreen::screens(mtm)
        .iter()
        .filter_map(|screen| {
            let description = screen.deviceDescription();
            let number: Retained<AnyObject> = description.objectForKey(&key)?;
            let number = number.downcast::<NSNumber>().ok()?;
            Some((
                number.unsignedIntValue(),
                screen.localizedName().to_string(),
            ))
        })
        .collect()
}

/// "Built-in Retina Display (1728×1117)", falling back to the display's
/// position in the list when AppKit has no name to offer.
///
/// Those numbers are **points**, not pixels — they come from `SCDisplay::width`
/// and `::height`, which the header documents as points, so a 2x Retina panel
/// shows half its native pixel count here. That is the right thing for a
/// picker label (it is what the desktop measures), and it is emphatically the
/// wrong thing to configure a stream with: see
/// [`crate::capture::screen_stream::find_display`] and [`display_geometry`],
/// which is where both units live together.
fn display_name(names: &[(u32, String)], info: &DisplayInfo, index: usize) -> String {
    let label = names
        .iter()
        .find(|(id, _)| *id == info.id)
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| format!("Display {}", index + 1));
    format!("{label} ({}×{})", info.width, info.height)
}

/// A display's native pixel size, or `None` when Core Graphics declines to
/// describe it — which it does for some virtual and sidecar displays.
///
/// Shared with [`crate::capture::screen_stream::find_display`] so the recorder
/// only ever has one answer to "how many pixels wide is this screen". Two
/// answers is how a stream ends up configured at a different size from the
/// region that was framed for it.
pub fn pixel_size(display_id: u32) -> Option<(usize, usize)> {
    let mode = CGDisplayCopyDisplayMode(display_id)?;
    let width = CGDisplayMode::pixel_width(Some(&mode));
    let height = CGDisplayMode::pixel_height(Some(&mode));
    (width > 0 && height > 0).then_some((width, height))
}

/// Everything a region needs to know about one display: where it sits, how big
/// it is in both units, and the primary screen's height.
///
/// Read entirely from Core Graphics rather than from ScreenCaptureKit or
/// AppKit, which buys two things. `CGDisplayBounds` is synchronous, so this
/// needs none of the channel-and-timeout dance
/// [`fetch_display_infos`] does around `getShareableContentWithCompletionHandler`.
/// And it needs no `MainThreadMarker`, unlike [`screen_names`], so a region can
/// be resolved from anywhere — including a unit-test thread.
///
/// `primary_height_points` comes from `CGDisplayBounds(CGMainDisplayID())`
/// rather than from this display, because the AppKit y-flip is defined against
/// the primary screen. On a laptop with no second monitor the two numbers are
/// identical and any implementation looks correct; see [`crate::region`].
pub fn display_geometry(display_uid: &str) -> Result<DisplayGeometry> {
    let id: u32 = display_uid
        .parse()
        .with_context(|| format!("display id {display_uid} is not a CGDirectDisplayID"))?;

    let bounds = CGDisplayBounds(id);
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        bail!("display {id} reports no bounds — it is probably no longer attached");
    }
    let points = (bounds.size.width, bounds.size.height);

    // Falling back to the point size means falling back to 1x. That is right
    // for the displays Core Graphics declines to describe (virtual, sidecar),
    // which are not Retina, and it degrades to a softer capture rather than to
    // a stream configured at half size.
    let pixels = pixel_size(id).unwrap_or((points.0.round() as usize, points.1.round() as usize));

    Ok(DisplayGeometry {
        cg_origin: (bounds.origin.x, bounds.origin.y),
        points,
        pixels,
        primary_height_points: CGDisplayBounds(CGMainDisplayID()).size.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u32) -> DisplayInfo {
        DisplayInfo {
            id,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn display_name_prefers_the_appkit_name_for_a_matching_id() {
        let names = vec![(7, "Built-in Retina Display".to_string())];
        assert_eq!(
            display_name(&names, &info(7), 0),
            "Built-in Retina Display (1920×1080)"
        );
    }

    #[test]
    fn display_name_falls_back_to_the_list_position_when_unmatched() {
        let names = vec![(7, "Built-in Retina Display".to_string())];
        assert_eq!(display_name(&names, &info(99), 1), "Display 2 (1920×1080)");
    }

    #[test]
    fn display_name_falls_back_when_appkit_offers_nothing() {
        assert_eq!(display_name(&[], &info(7), 0), "Display 1 (1920×1080)");
    }

    /// Reads the real geometry of the main display and prints both units.
    ///
    /// Ignored not because it is slow but because it asserts against whatever
    /// hardware is attached; the arithmetic built on top of these three numbers
    /// is tested exhaustively and without hardware in [`crate::region`]. What
    /// this pins is that Core Graphics actually answers, and that its two size
    /// reports differ by a sane backing scale rather than by a factor nobody
    /// expected. Needs no permission — `CGDisplayBounds` is not TCC-gated.
    ///
    /// `cargo test -- --ignored --nocapture display_geometry`
    #[test]
    #[ignore]
    fn display_geometry_reports_points_and_pixels_of_the_main_display() {
        let uid = CGMainDisplayID().to_string();
        let geom = display_geometry(&uid).expect("the main display has geometry");
        println!(
            "display {uid}: origin {:?} points {:?} pixels {:?} scale {:.3} \
             primary height {:.1}pt",
            geom.cg_origin,
            geom.points,
            geom.pixels,
            geom.scale(),
            geom.primary_height_points,
        );
        assert!(
            (1.0..=4.0).contains(&geom.scale()),
            "backing scale {} is not a plausible one",
            geom.scale(),
        );
        assert!(geom.primary_height_points > 0.0);
    }

    /// Real ScreenCaptureKit round-trip: proves the async completion block
    /// actually fires and hands back displays, which no unit test can.
    /// Run explicitly with `cargo test -- --ignored list_displays`.
    ///
    /// Needs screen-recording permission granted to *the test binary*, which
    /// is a fresh path on every rebuild — so a failure here is far more often
    /// "TCC has not seen this binary" than a bug. Names fall back to
    /// "Display N" under `cargo test` regardless: `NSScreen` is main-thread
    /// only and the harness runs tests on a spawned thread.
    #[test]
    #[ignore]
    fn list_displays_returns_the_attached_screens() {
        let displays = list_displays().expect("ScreenCaptureKit answers");
        for d in &displays {
            println!("display uid={} name={}", d.uid, d.name);
        }
        assert!(
            !displays.is_empty(),
            "no displays — grant screen recording to the test binary in \
             System Settings > Privacy & Security > Screen & System Audio Recording"
        );
        assert!(
            displays.iter().all(|d| d.uid.parse::<u32>().is_ok()),
            "every uid must be a CGDirectDisplayID"
        );
    }
}
