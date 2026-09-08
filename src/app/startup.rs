//! Session startup: resolve devices, take permissions, bring capture up, and
//! hand a built [`App`] to the winit event loop.
//!
//! Ordering here is load-bearing in two places. The screen stream starts
//! *before* the camera session, so a screen that refuses to open does not leave
//! a camera session running headless. And no writer is created at all — the
//! first chapter's writer is built by the first New Chapter press, on the far
//! side of every device renegotiation AppKit triggers as the window appears.
//! Both are explained in the [module docs](super).

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::app::resolve::pair_capture;
use crate::app::App;
use crate::capture::device_picker::CaptureDevice;
use crate::capture::{av, screen_stream};
use crate::layouts::Layout;
use winit::platform::macos::EventLoopBuilderExtMacOS;

/// A launch-time device choice, and whether it is the one config asked for.
#[derive(Debug, Clone, PartialEq)]
enum Choice {
    /// The saved device, still connected.
    Saved(String),
    /// Nothing saved yet: the system default, or the first listed.
    Unsaved(String),
    /// The saved device is not connected; a stand-in for this launch only.
    StandIn { uid: String, saved: String },
}

impl Choice {
    fn uid(&self) -> &str {
        match self {
            Choice::Saved(uid) | Choice::Unsaved(uid) | Choice::StandIn { uid, .. } => uid,
        }
    }
}

/// The device to capture from: the saved one when it is connected, else the
/// system default, else whatever is listed first.
///
/// A stand-in is never written back to config — see [`run_record_session`]. It
/// used to be, and the stand-in was `devices[0]`: with the XLR dock unplugged
/// and the iPhone out of range, that was BlackHole 2ch, a loopback device that
/// records digital silence. One such launch saved it, and every take for the
/// next three days transcribed as nothing, with no message anywhere.
fn resolve_device(
    saved: Option<&str>,
    devices: &[CaptureDevice],
    system_default: Option<&CaptureDevice>,
) -> Choice {
    let fallback = system_default
        .filter(|d| devices.iter().any(|listed| listed.uid == d.uid))
        .unwrap_or(&devices[0])
        .uid
        .clone();
    match saved {
        Some(uid) if devices.iter().any(|d| d.uid == uid) => Choice::Saved(uid.to_string()),
        Some(uid) => Choice::StandIn {
            uid: fallback,
            saved: uid.to_string(),
        },
        None => Choice::Unsaved(fallback),
    }
}

/// Says which device a launch is on when it is not the saved one, so a silent
/// or wrong-camera take is announced before it is recorded rather than found
/// in the transcript.
fn announce(kind: &str, choice: &Choice, devices: &[CaptureDevice]) {
    let name = |uid: &str| {
        devices
            .iter()
            .find(|d| d.uid == uid)
            .map_or_else(|| uid.to_string(), |d| d.name.clone())
    };
    match choice {
        Choice::Saved(_) => {}
        Choice::Unsaved(uid) => println!(
            "stream-recorder: no {kind} saved yet — using \"{}\"; pick one in the {kind} dropdown to keep it",
            name(uid)
        ),
        Choice::StandIn { uid, saved } => eprintln!(
            "stream-recorder: saved {kind} {saved:?} is not connected — using \"{}\" for this launch. \
             Reconnect it, or pick another in the {kind} dropdown to save that instead.",
            name(uid)
        ),
    }
}

/// Every display available to capture, or an empty list with an explanation
/// printed — a missing screen-recording permission disables one dropdown, it
/// does not stop a camera session from recording.
fn available_displays() -> Vec<CaptureDevice> {
    if !crate::permissions::ensure_screen_access() {
        eprintln!(
            "stream-recorder: no screen recording permission — the screen picker will be empty.\n  \
             Grant it in System Settings > Privacy & Security > Screen & System Audio Recording, \
             then restart stream-recorder (macOS only applies the grant to a new process)."
        );
        return Vec::new();
    }
    match crate::capture::screen::list_displays() {
        Ok(displays) => displays,
        Err(e) => {
            eprintln!("stream-recorder: could not list displays: {e:#}");
            Vec::new()
        }
    }
}

/// Run a full record session: window with device dropdowns and chapter
/// buttons, capture started on the saved devices — or, for a saved device that
/// is not connected, the system default for this launch only.
pub fn run_record_session(
    _reselect_camera: bool,
    _reselect_mic: bool,
    layout: &'static Layout,
) -> Result<()> {
    let mut cfg = crate::config::load();

    let cameras = av::list_camera_devices()?;
    if cameras.is_empty() {
        bail!("no camera devices found");
    }
    let mics = av::list_audio_devices()?;
    if mics.is_empty() {
        bail!("no audio input devices found");
    }
    let displays = available_displays();
    // A saved display id only survives if that display is still attached —
    // CGDirectDisplayIDs are reassigned across reboots and re-plugging, so a
    // stale one must fall back to "No screen" rather than to another monitor.
    let screen_uid = cfg
        .screen_display_id
        .as_deref()
        .filter(|uid| displays.iter().any(|d| d.uid == *uid))
        .map(str::to_string);

    let camera = resolve_device(
        cfg.camera_device_uid.as_deref(),
        &cameras,
        av::default_camera_device().as_ref(),
    );
    let audio = resolve_device(
        cfg.audio_device_uid.as_deref(),
        &mics,
        av::default_audio_device().as_ref(),
    );
    announce("camera", &camera, &cameras);
    announce("microphone", &audio, &mics);
    let camera_uid = camera.uid().to_string();
    let audio_uid = audio.uid().to_string();
    // Only the display is written back here. A camera or microphone chosen
    // *for* the operator is a guess about this launch, not their choice, and
    // saving the guess is how a stale config came to hold a silent mic and keep
    // it. Their choice is saved where they make it — `App::switch_devices`.
    if cfg.screen_display_id != screen_uid {
        cfg.screen_display_id = screen_uid.clone();
        crate::config::save(&cfg)?;
    }
    let camera_name = &cameras.iter().find(|d| d.uid == camera_uid).unwrap().name;
    let mic_name = &mics.iter().find(|d| d.uid == audio_uid).unwrap().name;
    let screen_name = screen_uid
        .as_ref()
        .and_then(|uid| displays.iter().find(|d| &d.uid == uid))
        .map_or("none", |d| d.name.as_str());
    println!(
        "stream-recorder: camera \"{camera_name}\", mic \"{mic_name}\", screen \"{screen_name}\""
    );
    match layout.slot_size() {
        Some((w, h)) => println!(
            "stream-recorder: layout {} ({}) — screen framed at {:.0}×{:.0}, {:.3}:1",
            layout.label(),
            layout.block,
            w,
            h,
            w / h,
        ),
        None => println!(
            "stream-recorder: layout {} ({}) — camera only, no screen capture",
            layout.label(),
            layout.block,
        ),
    }

    println!("stream-recorder: requesting camera access...");
    if !crate::permissions::ensure_video_access()? {
        bail!("camera access denied — enable it in System Settings > Privacy & Security > Camera");
    }
    println!("stream-recorder: requesting microphone access...");
    if !crate::permissions::ensure_audio_access()? {
        bail!(
            "microphone access denied — enable it in System Settings > Privacy & Security > Microphone"
        );
    }

    let session = crate::session::Session::open()?;

    // Start the screen stream, if this layout and this display both call for
    // one, before the camera: a failure here should not leave a camera session
    // running headless.
    let wanted = layout
        .needs_screen()
        .then_some(screen_uid.as_deref())
        .flatten();
    let screen = match wanted {
        Some(uid) => pair_capture(layout.pair, uid)
            .and_then(|capture| screen_stream::ScreenConnection::start_capture(uid, capture))
            .map_err(|e| eprintln!("stream-recorder: could not start screen capture: {e:#}"))
            .ok()
            .inspect(|connection| {
                println!(
                    "stream-recorder: screen capture live at {}×{}",
                    connection.width, connection.height
                )
            }),
        None => None,
    };
    // Only forget the remembered display when we actually tried and failed. A
    // talking-head layout produces no stream by design, and dropping the
    // operator's display choice for that reason would silently un-pick their
    // monitor the moment they framed a camera-only chapter.
    let screen_uid = if wanted.is_some() && screen.is_none() {
        None
    } else {
        screen_uid
    };

    let connection = av::Connection::start_capture(&camera_uid, &audio_uid)?;
    println!("stream-recorder: warming up capture...");
    // Proves both streams are actually delivering before the window opens.
    // It is NOT what makes chapter 1 safe — no fixed delay can be, since the
    // audio device renegotiates again when AppKit comes up. The first writer
    // is created by the first New Chapter press instead; see the module docs.
    connection.wait_for_warmup(Duration::from_secs(5))?;
    println!(
        "stream-recorder: capture live (session running = {}) — press New Chapter to start recording to {}",
        connection.is_running(),
        session.dir.display()
    );

    let event_loop = winit::event_loop::EventLoop::builder()
        .with_activation_policy(winit::platform::macos::ActivationPolicy::Accessory)
        .with_default_menu(false)
        .build()?;

    let notes_providers = crate::notes::load_providers();
    // A saved provider OpenRouter no longer lists is dropped rather than sent:
    // the models endpoint ignores a `providers` value it does not recognise and
    // answers with everything, so a stale name reads as a working filter.
    let notes_provider = cfg
        .notes_provider
        .clone()
        .filter(|p| notes_providers.iter().any(|n| n == p));
    let notes_pick = crate::notes::Picker::restore(notes_provider, cfg.notes_model.clone());
    let notes_prompt = cfg.notes_prompt.clone().unwrap_or_default();
    let (notes_tx, notes_rx) = std::sync::mpsc::channel();
    let (render_tx, render_rx) = std::sync::mpsc::channel();
    let (posts_tx, posts_rx) = std::sync::mpsc::channel();
    let (titles_tx, titles_rx) = std::sync::mpsc::channel();
    let (distribute_tx, distribute_rx) = std::sync::mpsc::channel();
    let (schedule_tx, schedule_rx) = std::sync::mpsc::channel();
    let (analytics_tx, analytics_rx) = std::sync::mpsc::channel();
    let (reflect_tx, reflect_rx) = std::sync::mpsc::channel();
    let (thumbnail_tx, thumbnail_rx) = std::sync::mpsc::channel();
    let (figure_tx, figure_rx) = std::sync::mpsc::channel();
    let (card_tx, card_rx) = std::sync::mpsc::channel();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel();
    let (substack_tx, substack_rx) = std::sync::mpsc::channel();
    let (blog_tx, blog_rx) = std::sync::mpsc::channel();
    // The Post tab owns its choice, so a saved one is restored rather than
    // overwritten. Only a machine that has never picked one falls back to
    // mirroring Notes — which is also the cheap path, reusing the catalog
    // already fetched instead of asking for a second one.
    let posts_pick = match (&cfg.posts_model, &cfg.posts_provider) {
        (None, None) => notes_pick.mirror(),
        (model, provider) => crate::notes::Picker::restore(provider.clone(), model.clone()),
    };
    let mut app = App {
        session,
        notes_tx,
        notes_rx,
        render_tx,
        render_rx,
        render_busy: false,
        pipeline: false,
        posts_tx,
        posts_rx,
        substack_tx,
        substack_rx,
        substack_busy: false,
        blog_tx,
        blog_rx,
        blog_busy: false,
        titles_tx,
        titles_rx,
        distribute_tx,
        distribute_rx,
        distribute_busy: false,
        schedule_tx,
        schedule_rx,
        analytics_tx,
        analytics_rx,
        reflect_tx,
        reflect_rx,
        reflect_busy: false,
        thumbnail_tx,
        thumbnail_rx,
        thumbnail_busy: false,
        figure_tx,
        figure_rx,
        figure_busy: false,
        aside: None,
        card_tx,
        card_rx,
        card_raster: None,
        card_pending: None,
        video_copy_job: None,
        analytics_busy: false,
        publish_tx,
        publish_rx,
        publish_busy: false,
        open_edit_chapter: None,
        last_report: None,
        clock: crate::app::clock::RecordClock::default(),
        due_checked: None,
        schedule_busy: false,
        live: None,
        router: None,
        connection: Some(connection),
        next_chapter: 1,
        cameras,
        mics,
        displays,
        camera_uid,
        audio_uid,
        screen_uid,
        screen,
        pair: layout.pair,
        pending_pair: None,
        orientation: layout.orientation,
        geometry: None,
        placements: BTreeMap::new(),
        regions: BTreeMap::new(),
        renderer: crate::ops::Renderer::new()
            .map_err(|e| {
                eprintln!("stream-recorder: no preview renderer: {e:#}");
                e
            })
            .ok(),
        // Loaded, not started. The detector build needs a window to report
        // back to, so `App::start` kicks it once the window exists — see
        // `app::face`.
        youtube_privacy: cfg.youtube_privacy,
        face_config: cfg.face_tracking.clone(),
        face_tracker: None,
        face_loading: false,
        pointer_config: cfg.mouse_tracking,
        pointer_tracker: None,
        notes_pick,
        notes_providers,
        notes_prompt,
        posts_pick,
        posts_prompt: cfg.posts_prompt.clone().unwrap_or_default(),
        posts_manifest: None,
    };
    // Fills `geometry` and both Split regions from the selected display, so the
    // overlay and the next chapter agree with the stream that just came up.
    app.rebuild_regions();
    app.apply_region_change();
    event_loop.run_app(&mut app)?;

    println!("stream-recorder: done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(uid: &str) -> CaptureDevice {
        CaptureDevice {
            uid: uid.into(),
            name: format!("device {uid}"),
        }
    }

    #[test]
    fn a_saved_device_that_is_connected_is_used() {
        let devices = [device("loopback"), device("xlr")];
        assert_eq!(
            resolve_device(Some("xlr"), &devices, Some(&device("builtin"))),
            Choice::Saved("xlr".into())
        );
    }

    /// The regression: the saved XLR dock is unplugged, the list happens to
    /// start with a loopback device, and the system default is the built-in mic.
    #[test]
    fn a_missing_saved_device_stands_in_the_system_default_not_the_first_listed() {
        let devices = [device("loopback"), device("builtin")];
        assert_eq!(
            resolve_device(Some("xlr"), &devices, Some(&device("builtin"))),
            Choice::StandIn {
                uid: "builtin".into(),
                saved: "xlr".into(),
            }
        );
    }

    #[test]
    fn the_first_listed_is_the_last_resort() {
        let devices = [device("loopback"), device("builtin")];
        assert_eq!(
            resolve_device(Some("xlr"), &devices, None).uid(),
            "loopback"
        );
        // A default AVFoundation names but does not list is no better than none.
        assert_eq!(
            resolve_device(Some("xlr"), &devices, Some(&device("ghost"))).uid(),
            "loopback"
        );
    }

    #[test]
    fn nothing_saved_takes_the_system_default_without_calling_it_a_stand_in() {
        let devices = [device("loopback"), device("builtin")];
        assert_eq!(
            resolve_device(None, &devices, Some(&device("builtin"))),
            Choice::Unsaved("builtin".into())
        );
    }
}
