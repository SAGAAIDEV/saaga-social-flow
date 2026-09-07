//! Microphone device enumeration, connection, and recording to a `.m4a`.
//!
//! Session-start timing note: `start_recording` anchors the writer's session
//! to *this stream's own* first sample buffer — fine with only one stream,
//! but wrong once camera/screen exist alongside it, since each capture
//! source starts and delivers its first buffer at a slightly different real
//! moment. That will need to change to a shared `TimeSync` epoch (one
//! reference time, handed to all writers) once those pipelines land — noted
//! here so the swap isn't a surprise later.

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::{
    AVCaptureAudioDataOutput, AVCaptureDeviceInput, AVCaptureSession,
    AVFileTypeAppleM4A, AVMediaTypeAudio,
};

use super::audio_delegate::AudioDelegate;
use super::device_picker::{self, CaptureDevice};
use crate::config;
use crate::permissions;
use crate::writer::{AudioFileWriter, MediaFileWriter};

pub type AudioDevice = CaptureDevice;

pub fn list_input_devices() -> Result<Vec<AudioDevice>> {
    let media_type =
        unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
    device_picker::list_devices(media_type)
}

/// A running capture session against one microphone, writing every captured
/// buffer to an `AVAssetWriter`-backed `.m4a`.
pub struct Connection {
    session: Retained<AVCaptureSession>,
    /// `setSampleBufferDelegate:queue:` does not necessarily retain the
    /// delegate strongly, so this is the thing keeping it alive for as long
    /// as the session runs.
    delegate: Retained<AudioDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    writer: AudioFileWriter,
}

impl Connection {
    pub fn start_recording(uid: &str, out_path: &Path) -> Result<Connection> {
        let media_type =
            unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
        let file_type = unsafe { AVFileTypeAppleM4A }
            .ok_or_else(|| anyhow!("AVFileTypeAppleM4A unavailable"))?;
        let devices = super::device_picker::devices_of(media_type);
        let device = devices
            .iter()
            .find(|d| unsafe { d.uniqueID() }.to_string() == uid)
            .ok_or_else(|| anyhow!("no audio device with uid {uid}"))?;

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| anyhow!("could not open input: {e:?}"))?;

        let output = unsafe { AVCaptureAudioDataOutput::new() };

        let session = unsafe { AVCaptureSession::new() };
        unsafe {
            if !session.canAddInput(&input) {
                bail!("session refused the microphone input");
            }
            session.addInput(&input);
            if !session.canAddOutput(&output) {
                bail!("session refused the audio data output");
            }
            session.addOutput(&output);
        }

        // The recommended-settings dictionary depends on the session's
        // current input configuration, so this must come after addInput —
        // querying it earlier silently returns settings AVAssetWriter then
        // refuses.
        let settings =
            unsafe { output.recommendedAudioSettingsForAssetWriterWithOutputFileType(file_type) };
        let writer = MediaFileWriter::create(out_path, file_type, media_type, settings)?;
        // Serial: audio samples must be delivered (and appended) in order.
        let queue = DispatchQueue::new("stream-recorder.mic", DispatchQueueAttr::SERIAL);
        let delegate = AudioDelegate::new(writer.writer(), writer.input.clone());
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
/// default mic if one is set and still present, otherwise list devices,
/// prompt, and persist the choice as the new default.
pub fn interactive_connect(reselect: bool) -> Result<()> {
    let mut cfg = config::load();
    let devices = list_input_devices()?;
    if devices.is_empty() {
        bail!("no audio input devices found");
    }

    let chosen_uid = match device_picker::resolve_default(cfg.audio_device_uid.as_deref(), &devices, reselect) {
        Some(uid) => uid,
        None => {
            if let Some(saved) = &cfg.audio_device_uid {
                if !devices.iter().any(|d| &d.uid == saved) {
                    eprintln!(
                        "stream-recorder: saved default mic is no longer connected, pick again"
                    );
                }
            }
            device_picker::prompt_select("microphone", &devices)?
        }
    };

    if cfg.audio_device_uid.as_deref() != Some(chosen_uid.as_str()) {
        cfg.audio_device_uid = Some(chosen_uid.clone());
        config::save(&cfg)?;
        println!("stream-recorder: saved as default mic");
    }

    let chosen = devices.iter().find(|d| d.uid == chosen_uid).unwrap();
    println!("stream-recorder: using mic \"{}\"", chosen.name);

    println!("stream-recorder: requesting microphone access...");
    if !permissions::ensure_audio_access()? {
        bail!(
            "microphone access denied — enable it in System Settings > Privacy & Security > Microphone"
        );
    }

    let out_dir = crate::session::output_dir()?;
    let out_path = out_dir.join("mic.m4a");

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

    match crate::transcode::to_mp3(&out_path) {
        Ok(mp3_path) => println!("stream-recorder: transcoded -> {}", mp3_path.display()),
        // The recording itself already succeeded and is safely on disk;
        // a failed transcode (e.g. ffmpeg missing) shouldn't undo that.
        Err(e) => eprintln!("stream-recorder: mp3 transcode failed: {e:#}"),
    }
    Ok(())
}

