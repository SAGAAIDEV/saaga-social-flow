//! Camera device enumeration, connection, and recording to an `.mp4` —
//! **legacy**, reachable only from the `camera` subcommand.
//!
//! `start_recording` anchors the writer's session to *this stream's own* first
//! sample buffer, which is fine with only one stream and wrong the moment there
//! are two: each capture source starts and delivers its first buffer at a
//! slightly different real moment. The shared-epoch replacement landed
//! elsewhere rather than here — `Router::anchors` derives one instant in real
//! time and expresses it on each stream's own clock, and `capture::av` is the
//! camera path the record UI actually uses. This module stays as the
//! single-stream smoke test it always was; it has no chapter lifecycle and,
//! for the reason `video_delegate` records, no op graph.

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::{
    AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoDataOutput,
    AVFileTypeMPEG4, AVMediaTypeVideo,
};

use super::device_picker::{self, CaptureDevice};
use super::video_delegate::VideoDelegate;
use crate::config;
use crate::permissions;
use crate::writer::MediaFileWriter;

pub type CameraDevice = CaptureDevice;

pub fn list_camera_devices() -> Result<Vec<CameraDevice>> {
    let media_type =
        unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
    device_picker::list_devices(&media_type)
}

/// A running capture session against one camera, writing every captured
/// frame to an `AVAssetWriter`-backed `.mp4`.
pub struct Connection {
    session: Retained<AVCaptureSession>,
    /// `setSampleBufferDelegate:queue:` does not necessarily retain the
    /// delegate strongly, so this is the thing keeping it alive for as long
    /// as the session runs.
    delegate: Retained<VideoDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    writer: MediaFileWriter,
}

impl Connection {
    pub fn start_recording(uid: &str, out_path: &Path) -> Result<Connection> {
        let media_type =
            unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
        let file_type =
            unsafe { AVFileTypeMPEG4 }.ok_or_else(|| anyhow!("AVFileTypeMPEG4 unavailable"))?;
        let devices = super::device_picker::devices_of(media_type);
        let device = devices
            .iter()
            .find(|d| unsafe { d.uniqueID() }.to_string() == uid)
            .ok_or_else(|| anyhow!("no camera device with uid {uid}"))?;

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| anyhow!("could not open input: {e:?}"))?;

        let output = unsafe { AVCaptureVideoDataOutput::new() };

        let session = unsafe { AVCaptureSession::new() };
        unsafe {
            if !session.canAddInput(&input) {
                bail!("session refused the camera input");
            }
            session.addInput(&input);
            if !session.canAddOutput(&output) {
                bail!("session refused the video data output");
            }
            session.addOutput(&output);
        }

        // The recommended-settings dictionary depends on the session's
        // current input configuration, so this must come after addInput —
        // querying it earlier silently returns settings AVAssetWriter then
        // refuses.
        let settings =
            unsafe { output.recommendedVideoSettingsForAssetWriterWithOutputFileType(file_type) };
        let writer = MediaFileWriter::create(out_path, &file_type, &media_type, settings)?;
        // Serial: video frames must be delivered (and appended) in order.
        let queue = DispatchQueue::new("stream-recorder.camera", DispatchQueueAttr::SERIAL);
        let delegate = VideoDelegate::new(writer.writer(), writer.input.clone());
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
            session.startRunning();
        }

        Ok(Connection {
            session,
            delegate,
            _queue: queue,
            writer,
        })
    }

    pub fn is_running(&self) -> bool {
        unsafe { self.session.isRunning() }
    }

    pub fn buffers_captured(&self) -> u64 {
        self.delegate.buffers_appended()
    }

    /// Stop capturing and flush the file to disk.
    pub fn stop_and_finish(self) -> Result<()> {
        unsafe { self.session.stopRunning() };
        self.writer.finish()
    }
}

/// The whole select-and-connect flow, run from the terminal: use the saved
/// default camera if one is set and still present, otherwise list devices,
/// prompt, and persist the choice as the new default.
pub fn interactive_connect(reselect: bool) -> Result<()> {
    let mut cfg = config::load();
    let devices = list_camera_devices()?;
    if devices.is_empty() {
        bail!("no camera devices found");
    }

    let chosen_uid = match device_picker::resolve_default(cfg.camera_device_uid.as_deref(), &devices, reselect) {
        Some(uid) => uid,
        None => {
            if let Some(saved) = &cfg.camera_device_uid {
                if !devices.iter().any(|d| &d.uid == saved) {
                    eprintln!(
                        "stream-recorder: saved default camera is no longer connected, pick again"
                    );
                }
            }
            device_picker::prompt_select("camera", &devices)?
        }
    };

    if cfg.camera_device_uid.as_deref() != Some(chosen_uid.as_str()) {
        cfg.camera_device_uid = Some(chosen_uid.clone());
        config::save(&cfg)?;
        println!("stream-recorder: saved as default camera");
    }

    let chosen = devices.iter().find(|d| d.uid == chosen_uid).unwrap();
    println!("stream-recorder: using camera \"{}\"", chosen.name);

    println!("stream-recorder: requesting camera access...");
    if !permissions::ensure_video_access()? {
        bail!(
            "camera access denied — enable it in System Settings > Privacy & Security > Camera"
        );
    }

    let out_dir = crate::session::output_dir()?;
    let out_path = out_dir.join("camera.mp4");

    let conn = Connection::start_recording(&chosen_uid, &out_path)?;
    println!(
        "stream-recorder: recording (session running = {})",
        conn.is_running()
    );
    println!("stream-recorder: press Enter to stop");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();

    let captured = conn.buffers_captured();
    conn.stop_and_finish()?;
    println!(
        "stream-recorder: wrote {captured} buffers -> {}",
        out_path.display()
    );

    Ok(())
}
