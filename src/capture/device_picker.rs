//! Device enumeration and selection UI, shared by microphone and camera.

use std::io::Write;

use anyhow::Result;
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition, AVCaptureDeviceType,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeContinuityCamera,
    AVCaptureDeviceTypeDeskViewCamera, AVCaptureDeviceTypeExternal, AVCaptureDeviceTypeMicrophone,
    AVMediaType,
};
use objc2_foundation::NSArray;

#[derive(Debug, Clone)]
pub struct CaptureDevice {
    pub uid: String,
    pub name: String,
}

/// Every connected device of one media type.
///
/// The single place this recorder asks that question — `av`, `camera` and `mic`
/// all come through here — so "which devices exist" has one answer rather than
/// five that can drift.
///
/// ## Why this is not just `devicesWithMediaType`
///
/// That call is deprecated, and it is also the *safer* of the two: it returns
/// every device of a media type, full stop. `AVCaptureDeviceDiscoverySession`
/// returns only the device **types** you enumerate, so a type missing from the
/// list below is a camera or a microphone that silently vanishes from the
/// picker — on a recorder that was working a moment ago, with nothing
/// anywhere reporting a problem.
///
/// So the migration keeps a safety net: if the discovery session comes back
/// empty, fall through to the deprecated call. A compiler warning is worth
/// fixing; a mic that disappears mid-shoot is not, and no list of device types
/// I can write here is provably complete against future macOS.
pub fn devices_of(media_type: &AVMediaType) -> Retained<NSArray<AVCaptureDevice>> {
    // Audio and video need different device types, and an unrecognised media
    // type falls to the video list rather than to an empty one — an empty list
    // would make the discovery session find nothing and silently take the
    // fallback path below on every call.
    let is_audio =
        unsafe { objc2_av_foundation::AVMediaTypeAudio }.is_some_and(|audio| media_type == audio);
    let types: &[&AVCaptureDeviceType] = unsafe {
        if is_audio {
            // `Microphone` supersedes `BuiltInMicrophone` and covers external
            // interfaces too — an XLR dock arrives as one of these.
            &[AVCaptureDeviceTypeMicrophone, AVCaptureDeviceTypeExternal]
        } else {
            &[
                AVCaptureDeviceTypeBuiltInWideAngleCamera,
                AVCaptureDeviceTypeExternal,
                AVCaptureDeviceTypeContinuityCamera,
                AVCaptureDeviceTypeDeskViewCamera,
            ]
        }
    };

    let session = unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &NSArray::from_slice(types),
            Some(media_type),
            AVCaptureDevicePosition::Unspecified,
        )
    };
    let found = unsafe { session.devices() };
    if !found.is_empty() {
        return found;
    }

    // The safety net described above. Deprecated on purpose: reaching here
    // means the type list missed something, and an out-of-date API returning
    // the right devices beats a current one returning none.
    #[allow(deprecated)]
    unsafe {
        AVCaptureDevice::devicesWithMediaType(media_type)
    }
}

/// List all connected devices of a given media type.
pub fn list_devices(media_type: &AVMediaType) -> Result<Vec<CaptureDevice>> {
    Ok(devices_of(media_type)
        .iter()
        .map(|d| CaptureDevice {
            uid: unsafe { d.uniqueID() }.to_string(),
            name: unsafe { d.localizedName() }.to_string(),
        })
        .collect())
}

/// The system's current default device of one media type: the input System
/// Settings › Sound points at, or the primary camera.
///
/// What a launch falls back to when the saved device is not connected. The
/// first entry of [`list_devices`] is whatever AVFoundation happens to list
/// first — on this machine, a loopback device that records silence.
pub fn default_device(media_type: &AVMediaType) -> Option<CaptureDevice> {
    let device = unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media_type) }?;
    Some(CaptureDevice {
        uid: unsafe { device.uniqueID() }.to_string(),
        name: unsafe { device.localizedName() }.to_string(),
    })
}

/// Return the saved default device UID if it's still connected and reselect
/// was not requested, otherwise `None` (caller must prompt).
pub fn resolve_default(
    saved: Option<&str>,
    devices: &[CaptureDevice],
    reselect: bool,
) -> Option<String> {
    if reselect {
        return None;
    }
    let saved = saved?;
    devices
        .iter()
        .any(|d| d.uid == saved)
        .then(|| saved.to_string())
}

/// Parse a 1-based selection line into a 0-based index, `None` if it's not a
/// number in range `1..=count`.
pub fn parse_selection(line: &str, count: usize) -> Option<usize> {
    let n: usize = line.trim().parse().ok()?;
    (n >= 1 && n <= count).then(|| n - 1)
}

/// Prompt the user to select a device from a list.
pub fn prompt_select(kind: &str, devices: &[CaptureDevice]) -> Result<String> {
    println!("stream-recorder: available {kind}s:");
    for (i, d) in devices.iter().enumerate() {
        println!("  [{}] {}", i + 1, d.name);
    }
    loop {
        print!("select a {kind} (1-{}): ", devices.len());
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if let Some(i) = parse_selection(&line, devices.len()) {
            return Ok(devices[i].uid.clone());
        }
        println!(
            "stream-recorder: enter a number between 1 and {}",
            devices.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(uid: &str) -> CaptureDevice {
        CaptureDevice {
            uid: uid.to_string(),
            name: format!("device {uid}"),
        }
    }

    #[test]
    fn resolve_default_uses_the_saved_device_when_still_present() {
        let devices = [device("a"), device("b")];
        assert_eq!(
            resolve_default(Some("b"), &devices, false),
            Some("b".to_string())
        );
    }

    #[test]
    fn resolve_default_reprompts_when_reselect_is_requested() {
        let devices = [device("a")];
        assert_eq!(resolve_default(Some("a"), &devices, true), None);
    }

    #[test]
    fn resolve_default_reprompts_when_the_saved_device_is_gone() {
        let devices = [device("a")];
        assert_eq!(resolve_default(Some("unplugged"), &devices, false), None);
    }

    #[test]
    fn resolve_default_reprompts_when_nothing_is_saved_yet() {
        let devices = [device("a")];
        assert_eq!(resolve_default(None, &devices, false), None);
    }

    #[test]
    fn parse_selection_accepts_a_number_in_range() {
        assert_eq!(parse_selection("2\n", 3), Some(1));
        assert_eq!(parse_selection("1", 1), Some(0));
    }

    #[test]
    fn parse_selection_rejects_out_of_range_and_non_numeric() {
        assert_eq!(parse_selection("0", 3), None);
        assert_eq!(parse_selection("4", 3), None);
        assert_eq!(parse_selection("nope", 3), None);
        assert_eq!(parse_selection("", 3), None);
    }
}
