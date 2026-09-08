//! Camera / microphone / screen-recording access requests.
//!
//! `requestAccessForMediaType:completionHandler:` calls its handler on an
//! arbitrary dispatch queue, so this blocks the calling thread on a channel
//! rather than trying to thread a callback through — fine here since it's
//! only ever called from a synchronous CLI flow, never from inside the
//! winit event loop.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

/// Block until the user answers the microphone permission prompt, or return
/// immediately if it was already decided. `true` if access is granted.
pub fn ensure_audio_access() -> Result<bool> {
    let media_type =
        unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
    ensure_access(&media_type)
}

/// Block until the user answers the camera permission prompt, or return
/// immediately if it was already decided. `true` if access is granted.
pub fn ensure_video_access() -> Result<bool> {
    let media_type =
        unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
    ensure_access(&media_type)
}

/// Whether this process may capture the screen, prompting once if the user
/// has not been asked yet. `true` if access is granted *right now*.
///
/// Screen recording is not an `AVCaptureDevice` media type and does not work
/// like the camera/mic prompts: `CGRequestScreenCaptureAccess` shows the
/// dialog but returns `false` for the run in which the user grants it — the
/// TCC decision only takes effect for a *new* process. So a `false` here can
/// mean either "denied" or "just granted, restart me", and the caller has to
/// say so rather than treat it as a hard denial.
///
/// The permission is also attributed to the *running binary*, which for
/// `cargo run` is the debug binary under `target/` — rebuilding to a new path
/// can require re-granting.
pub fn ensure_screen_access() -> bool {
    if CGPreflightScreenCaptureAccess() {
        return true;
    }
    CGRequestScreenCaptureAccess()
}

fn ensure_access(media_type: &AVMediaType) -> Result<bool> {
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    if status == AVAuthorizationStatus::Authorized {
        return Ok(true);
    }
    if status != AVAuthorizationStatus::NotDetermined {
        // Denied or Restricted: asking again would not show a dialog.
        return Ok(false);
    }

    let (tx, rx) = mpsc::channel::<bool>();
    let handler = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &handler);
    }
    rx.recv_timeout(Duration::from_secs(120))
        .context("timed out waiting for the permission prompt")
}
