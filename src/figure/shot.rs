//! Turning a selected rectangle into the figure file on disk.
//!
//! Through [`SCScreenshotManager::captureImageInRect_completionHandler`], which
//! is the one API here that takes a rect in Core Graphics' *global* point space
//! and works across displays without being told which one it is on. The
//! recorder's own screen stream is no use for this: it is configured to the
//! layout's source rect at the layout's output size, so a figure taken from it
//! would be cropped to whatever the composition happens to be showing and
//! resampled to its resolution.
//!
//! ## It is asynchronous, and stays that way
//!
//! `captureImageInRect:` answers on one of ScreenCaptureKit's own queues. The
//! request is fired from a `mouseUp:` handler on the main thread, so blocking
//! for the answer would block the window server's own event delivery — the
//! overlay would still be on screen, dimming the very frame being photographed,
//! for as long as the wait lasted. Instead the completion block encodes, writes,
//! and posts a [`crate::figure::FigureEvent`] down the channel the App already
//! drains on its 60Hz tick. The same shape as every other worker in this crate.
//!
//! ## The window is down before the shutter
//!
//! [`crate::figure::snip`]'s window is `NSWindowSharingType::None`, so no screen
//! capture can see it. That is the guarantee; hiding it before the request is
//! the belt to that braces, and the selection being a *hole* in the dim is the
//! third. Three independent reasons the overlay cannot end up in a figure, kept
//! because this is the failure nobody would notice until the article was live.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use block2::RcBlock;
use objc2_core_foundation::CGRect;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::SCScreenshotManager;

use super::{encode, Capture, FigureEvent};

/// Ask the window server for `rect` and write it to `capture.file`.
///
/// `rect` is in Core Graphics' global point space — see [`crate::region`] for
/// the three spaces and which is which. Returns immediately; the outcome
/// arrives on `tx`.
pub fn capture(rect: CGRect, capture: Capture, tx: Sender<FigureEvent>) {
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let event = match finish(image, error, &capture) {
            // The size rides on the capture from here: the ledger row records
            // the file's own dimensions, not the rectangle that was asked for.
            Ok((width, height)) => FigureEvent::Captured(Capture {
                width,
                height,
                ..capture.clone()
            }),
            Err(err) => FigureEvent::Failed(format!("{err:#}")),
        };
        let _ = tx.send(event);
    });
    unsafe {
        SCScreenshotManager::captureImageInRect_completionHandler(rect, Some(&handler));
    }
}

/// Encode and write, or say why not. Answers with the pixel size the file was
/// written at.
///
/// Split out so the block above has no `?` in it: an early return from inside a
/// completion block is a figure that silently never appears, where this way
/// every path ends in an event.
fn finish(
    image: *mut CGImage,
    error: *mut NSError,
    capture: &Capture,
) -> anyhow::Result<(u32, u32)> {
    if !error.is_null() {
        let message = unsafe { (*error).localizedDescription() }.to_string();
        anyhow::bail!("the window server refused the capture: {message}");
    }
    let image = unsafe { image.as_ref() }.ok_or_else(|| {
        anyhow::anyhow!(
            "the window server returned no image — is screen recording still granted?"
        )
    })?;

    // Lossless, at its own size under the ceiling — see `encode`.
    let bytes = encode::encode_cg(image)?;
    // Measured from the bytes rather than taken from the request: the size the
    // ledger records has to be the file's own.
    let size = encode::dimensions(&bytes)
        .ok_or_else(|| anyhow::anyhow!("the encoder returned bytes that are not an image"))?;
    write(&capture.file, &bytes)?;
    Ok(size)
}

fn write(path: &PathBuf, bytes: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}
