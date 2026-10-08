//! One picture of a whole display, for when no screen stream is running.
//!
//! The recorder streams the screen only while the layout has a slot for it,
//! and crops that stream to the slot — see `App::screen_wanted`. The
//! thumbnail's screen grab wants neither condition: it is a picture of what the
//! video is about, taken whatever the layout. Talking Head had no stream to
//! read, so Retake screen did nothing until the operator switched to Split.
//!
//! [`SCScreenshotManager::captureImageWithFilter_configuration_completionHandler`]
//! takes one frame through the same content filter the stream is built from,
//! so this app's own windows stay out of it exactly as they stay out of a
//! recording — the press that asked for the picture is on one of them. The
//! figure path's `captureImageInRect:` takes no filter, which is why it is not
//! used here.
//!
//! ## Blocking, then asynchronous
//!
//! Resolving the display is the `SCShareableContent` round trip with a
//! ten-second timeout (see [`super::screen_filter`]), so call this off the main
//! thread. The picture itself answers on one of ScreenCaptureKit's queues,
//! into `done` — the same shape as [`crate::figure::shot`].

use anyhow::{anyhow, bail, Result};
use block2::RcBlock;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCScreenshotManager, SCStreamConfiguration};

use super::screen_filter::{content_filter, find_display, no_exclusions};

/// Ask for the whole of `display` at its native pixel size, and call `done`
/// with the picture or why there is none.
///
/// `show_self` is the Show App switch, as the stream reads it. The pointer is
/// always left out: it sits on the button that was just pressed. An `Err`
/// here means nothing was asked for and `done` will not be called.
pub fn display<F>(display: u32, show_self: bool, done: F) -> Result<()>
where
    F: Fn(Result<&CGImage>) + Send + 'static,
{
    let (display, own_windows, width, height) = find_display(display)?;
    let excluded = if show_self {
        no_exclusions()
    } else {
        own_windows
    };
    let filter = content_filter(&display, &excluded);
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(width);
        config.setHeight(height);
        config.setShowsCursor(false);
    }
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        done(answer(image, error));
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            &filter,
            &config,
            Some(&handler),
        );
    }
    Ok(())
}

/// The picture, or why the window server sent none.
///
/// Split out so the block above has no `?` in it: an early return from inside
/// a completion block is a grab that silently never arrives.
fn answer<'a>(image: *mut CGImage, error: *mut NSError) -> Result<&'a CGImage> {
    if let Some(error) = unsafe { error.as_ref() } {
        bail!(
            "the window server refused the capture: {}",
            error.localizedDescription()
        );
    }
    unsafe { image.as_ref() }.ok_or_else(|| {
        anyhow!("the window server returned no image — is screen recording still granted?")
    })
}
