//! Combined camera + microphone capture to a single muxed `.mp4`, with
//! synchronized audio and video tracks anchored to a single writer session.
//!
//! One `AVCaptureSession` with both camera and mic inputs, feeding both
//! a video and audio data output, both writing to the same `AVAssetWriter`.
//! The unified `AvDelegate` handles both callbacks concurrently on separate
//! dispatch queues, with shared writer state guarded by `Mutex`.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::{
    AVCaptureAudioDataOutput, AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoDataOutput,
    AVFileTypeAppleM4A, AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo,
};

use std::time::Duration;

use objc2::runtime::AnyObject;
use objc2_av_foundation::{AVAssetWriter, AVAssetWriterInput};
use objc2_core_media::CMClock;
use objc2_foundation::{NSDictionary, NSString, NSURL};

use super::av_delegate::AvDelegate;
use super::device_picker::{self, CaptureDevice};
use crate::config;
use crate::layouts::Pair;
use crate::ops::{graphs, PreviewPort, Renderer, StreamCtx, StreamId, Tap};
use crate::permissions;

pub type AudioDevice = CaptureDevice;
pub type CameraDevice = CaptureDevice;

pub fn list_audio_devices() -> Result<Vec<AudioDevice>> {
    let media_type =
        unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
    device_picker::list_devices(&media_type)
}

pub fn list_camera_devices() -> Result<Vec<CameraDevice>> {
    let media_type =
        unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
    device_picker::list_devices(&media_type)
}

/// The microphone System Settings › Sound has as its input, when AVFoundation
/// knows one. The launch-time stand-in for a saved mic that is not connected.
pub fn default_audio_device() -> Option<AudioDevice> {
    let media_type = unsafe { AVMediaTypeAudio }?;
    device_picker::default_device(media_type)
}

/// The primary camera, by the same rule.
pub fn default_camera_device() -> Option<CameraDevice> {
    let media_type = unsafe { AVMediaTypeVideo }?;
    device_picker::default_device(media_type)
}

/// Writer + inputs for a single chapter.
pub struct ChapterWriter {
    pub writer: Retained<AVAssetWriter>,
    pub video_input: Retained<AVAssetWriterInput>,
    pub audio_input: Retained<AVAssetWriterInput>,
}

/// Writer + input for an audio-only file: a figure's aside — see
/// [`crate::figure::aside`].
pub struct AudioWriter {
    pub writer: Retained<AVAssetWriter>,
    pub audio_input: Retained<AVAssetWriterInput>,
}

/// A running combined capture session against one camera and one microphone.
/// Starts **writerless**: buffers are counted and dropped until a writer is
/// installed (by [`Connection::install_writer`] or the Router). This ordering
/// is deliberate — writer inputs lock onto the format of the first buffers
/// they receive, and external devices (USB audio interfaces especially) can
/// renegotiate their format while the session settles. A writer built from
/// cold-start buffers once produced an entire chapter of static.
pub struct Connection {
    pub session: Retained<AVCaptureSession>,
    /// Keep the delegate alive for as long as the session runs.
    pub delegate: Retained<AvDelegate>,
    /// Keep both dispatch queues alive.
    _video_queue: DispatchRetained<DispatchQueue>,
    _audio_queue: DispatchRetained<DispatchQueue>,
    /// The recommended encoder settings queried from the live session at
    /// startup. Chapter writers MUST reuse these: they're the only settings
    /// that match what the capture outputs actually deliver. Creating a
    /// writer input with `None` instead means passthrough (no encoding), so
    /// the session's raw LPCM/frames land in the mp4 unencoded — which is
    /// what players render as static.
    pub video_settings: Retained<NSDictionary<NSString, AnyObject>>,
    pub audio_settings: Retained<NSDictionary<NSString, AnyObject>>,
}

impl Connection {
    /// Assemble and start the capture session with no writer attached.
    pub fn start_capture(camera_uid: &str, audio_uid: &str) -> Result<Connection> {
        let video_type =
            unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
        let audio_type =
            unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
        let file_type =
            unsafe { AVFileTypeMPEG4 }.ok_or_else(|| anyhow!("AVFileTypeMPEG4 unavailable"))?;

        // Resolve both input devices.
        let video_devices = super::device_picker::devices_of(video_type);
        let video_device = video_devices
            .iter()
            .find(|d| unsafe { d.uniqueID() }.to_string() == camera_uid)
            .ok_or_else(|| anyhow!("no camera device with uid {camera_uid}"))?;

        let audio_devices = super::device_picker::devices_of(audio_type);
        let audio_device = audio_devices
            .iter()
            .find(|d| unsafe { d.uniqueID() }.to_string() == audio_uid)
            .ok_or_else(|| anyhow!("no audio device with uid {audio_uid}"))?;

        // Create inputs.
        let video_input_dev =
            unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&video_device) }
                .map_err(|e| anyhow!("could not open video input: {e:?}"))?;
        let audio_input_dev =
            unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&audio_device) }
                .map_err(|e| anyhow!("could not open audio input: {e:?}"))?;

        // Create outputs.
        let video_output = unsafe { AVCaptureVideoDataOutput::new() };
        let audio_output = unsafe { AVCaptureAudioDataOutput::new() };

        // Create and configure the session.
        let session = unsafe { AVCaptureSession::new() };
        unsafe {
            if !session.canAddInput(&video_input_dev) {
                bail!("session refused the video input");
            }
            session.addInput(&video_input_dev);

            if !session.canAddInput(&audio_input_dev) {
                bail!("session refused the audio input");
            }
            session.addInput(&audio_input_dev);

            if !session.canAddOutput(&video_output) {
                bail!("session refused the video output");
            }
            session.addOutput(&video_output);

            if !session.canAddOutput(&audio_output) {
                bail!("session refused the audio output");
            }
            session.addOutput(&audio_output);
        }

        // Query recommended settings AFTER adding inputs/outputs (order matters).
        // These must exist: without them writer inputs fall back to passthrough
        // (no encoding) and the output is unplayable. Fail fast instead.
        let video_settings = unsafe {
            video_output.recommendedVideoSettingsForAssetWriterWithOutputFileType(&file_type)
        }
        .ok_or_else(|| anyhow!("no recommended video settings for this session"))?;
        let audio_settings = unsafe {
            audio_output.recommendedAudioSettingsForAssetWriterWithOutputFileType(&file_type)
        }
        .ok_or_else(|| anyhow!("no recommended audio settings for this session"))?;

        // Create separate serial queues for video and audio to preserve
        // sample buffer ordering within each stream.
        let video_queue = DispatchQueue::new("stream-recorder.av-video", DispatchQueueAttr::SERIAL);
        let audio_queue = DispatchQueue::new("stream-recorder.av-audio", DispatchQueueAttr::SERIAL);

        // Create the unified delegate — writerless until install_writer or
        // the Router attaches one. The op graph arrives with the writer, inside
        // the per-chapter state, rather than being fixed here the way the old
        // single processor was.
        let delegate = AvDelegate::new();

        // Register the delegate for both outputs on their respective queues.
        unsafe {
            video_output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&video_queue),
            );
            audio_output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&audio_queue),
            );
            session.startRunning();
        }

        Ok(Connection {
            session,
            delegate,
            _video_queue: video_queue,
            _audio_queue: audio_queue,
            video_settings,
            audio_settings,
        })
    }

    pub fn is_running(&self) -> bool {
        unsafe { self.session.isRunning() }
    }

    /// Block until both streams are actually delivering buffers, plus a short
    /// settle so any device-side format renegotiation has happened before a
    /// writer locks onto the stream format. See the struct docs for why.
    pub fn wait_for_warmup(&self, timeout: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.delegate.video_buffers_seen() >= 3 && self.delegate.audio_buffers_seen() >= 5 {
                break;
            }
            if std::time::Instant::now() > deadline {
                bail!(
                    "capture warmup timed out: saw {} video / {} audio buffers",
                    self.delegate.video_buffers_seen(),
                    self.delegate.audio_buffers_seen()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Grace period: buffers flowing isn't proof the format has settled.
        std::thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// Build a writer for `out_path` from the live session's settings and
    /// attach it. Recording starts with the next buffer that arrives.
    pub fn install_writer(&self, out_path: &Path) -> Result<()> {
        let chapter_writer =
            create_chapter_writer(&self.video_settings, &self.audio_settings, out_path)?;
        // Anchor at "now" on this session's own clock. There is no second file
        // to line up against on this path (it is the standalone `av` command),
        // so the anchor only has to precede the buffers that follow it.
        let clock = self.sync_clock()?;
        let anchor = crate::timesync::TimeSync::now_on(&clock);

        // This path has exactly one chapter, so its graph is opened here and
        // never closed — `stop_and_finish` drops the state rather than routing
        // it through the Router's finish arm. Passthrough records nothing, so
        // there is no sidecar to lose; an op that emitted data would need this
        // path to grow a close, which is why the standalone `av` subcommand is
        // not where new ops get exercised.
        let mut graph = graphs::default_graph(StreamId::Camera);
        graph.open(&StreamCtx {
            stream: StreamId::Camera,
            chapter: 1,
            clock: clock.clone(),
            // See `StreamCtx::size`: nothing here configures the camera's
            // output format or size, so neither is known before a buffer lands.
            size: None,
            fps: None,
            // The standalone `av` subcommand writes one file and runs no pixel
            // ops, so it needs neither the GPU context nor a pool.
            renderer: None,
            pool: None,
        })?;

        let state = crate::capture::av_delegate::AvState::new_for_chapter(
            chapter_writer,
            Vec::new(),
            anchor,
            graph,
        );
        *self.delegate.state_arc().lock().unwrap() = Some(state);
        Ok(())
    }

    /// The clock this session timestamps its sample buffers against.
    ///
    /// Not necessarily the host clock: a session with an audio input commonly
    /// runs on the audio interface's clock instead. Anything comparing these
    /// timestamps with another stream's must convert — see [`crate::timesync`].
    pub fn sync_clock(&self) -> Result<Retained<CMClock>> {
        unsafe { self.session.synchronizationClock() }
            .ok_or_else(|| anyhow!("the capture session exposes no synchronization clock"))
    }

    /// Install the session-scoped preview graph for `pair`. Replaces any
    /// previous preview. Returns the ports the UI should bind to.
    ///
    /// `tracking` is `Some` once the operator has asked for face tracking *and*
    /// the detector has finished loading — see `crate::face`. Reinstalling with
    /// it flipped either way is how the feature is switched on and off mid
    /// session: the graph is rebuilt, and because the tracker outlives the
    /// graph, its smoothing state survives the rebuild rather than swooping in
    /// from centre on the next frame.
    pub fn install_preview(
        &self,
        pair: Pair,
        renderer: &Renderer,
        screen: Option<std::sync::Arc<Tap>>,
        crops: [Option<(f64, f64, f64, f64)>; 2],
        tracking: Option<&graphs::Tracking>,
        pointing: Option<&graphs::Pointing>,
    ) -> Result<Vec<std::sync::Arc<PreviewPort>>> {
        let clock = self.sync_clock()?;
        let mut graph = graphs::preview_graph(pair, screen, crops, tracking, pointing);
        graph
            .open(&StreamCtx {
                stream: StreamId::Camera,
                chapter: 0,
                clock,
                size: None,
                fps: None,
                renderer: Some(renderer.clone()),
                pool: None,
            })
            .context("opening the preview graph")?;
        let ports = graph.preview_ports();
        self.delegate.set_preview(graph);
        Ok(ports)
    }

    pub fn video_buffers_captured(&self) -> u64 {
        self.delegate.video_frames_appended()
    }

    pub fn audio_buffers_captured(&self) -> u64 {
        self.delegate.audio_frames_appended()
    }

    /// Stop capturing and flush the file to disk.
    pub fn stop_and_finish(self) -> Result<()> {
        unsafe { self.session.stopRunning() };
        let state = self
            .delegate
            .state_arc()
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow!("no writer was installed"))?;
        state.finish()
    }
}

/// Create a new AVAssetWriter + inputs for a single chapter file.
/// Used by Router to create fresh writer+inputs when cutting chapters.
///
/// `video_settings`/`audio_settings` MUST be the dictionaries queried from
/// the live capture session at startup ([`Connection::start_recording`]
/// stores them). They are the only settings that match what the session's
/// outputs actually deliver; passing `None` to the writer inputs instead
/// would mean passthrough (no encoding) — raw LPCM/frames in an mp4, which
/// players render as static. That exact bug shipped once already.
pub fn create_chapter_writer(
    video_settings: &NSDictionary<NSString, AnyObject>,
    audio_settings: &NSDictionary<NSString, AnyObject>,
    out_path: &std::path::Path,
) -> Result<ChapterWriter> {
    let video_type =
        unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
    let audio_type =
        unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
    let file_type =
        unsafe { AVFileTypeMPEG4 }.ok_or_else(|| anyhow!("AVFileTypeMPEG4 unavailable"))?;

    // The session directory is made once at startup, but a whole recording
    // session can outlive it — a cleanup script, a Drive sync, or a stray
    // `rm` between chapters is enough. AVAssetWriter reports that only as
    // NSURLErrorNoPermissionsToReadFile (-3000) "Cannot create file", which
    // names neither the path nor the real cause, so re-create the directory
    // instead of letting a chapter die on it.
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating output directory {}", dir.display()))?;
    }

    // Create the AVAssetWriter.
    let url_string = NSString::from_str(&out_path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&url_string);
    let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, &file_type) }
        .map_err(|e| anyhow!("could not create asset writer: {e:?}"))?;

    let video_writer_input = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            &video_type,
            Some(video_settings),
        )
    };
    unsafe { video_writer_input.setExpectsMediaDataInRealTime(true) };

    let audio_writer_input = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            &audio_type,
            Some(audio_settings),
        )
    };
    unsafe { audio_writer_input.setExpectsMediaDataInRealTime(true) };

    // Add both inputs to the writer.
    unsafe {
        if !writer.canAddInput(&video_writer_input) {
            bail!("writer refused the video input");
        }
        writer.addInput(&video_writer_input);

        if !writer.canAddInput(&audio_writer_input) {
            bail!("writer refused the audio input");
        }
        writer.addInput(&audio_writer_input);

        if !writer.startWriting() {
            bail!("startWriting failed: {:?}", writer.error());
        }
    }

    Ok(ChapterWriter {
        writer,
        video_input: video_writer_input,
        audio_input: audio_writer_input,
    })
}

/// Create an `AVAssetWriter` with one audio input, writing an `.m4a` from the
/// running session's microphone while nothing else records.
///
/// `audio_settings` must be the session's own, for the reason
/// [`create_chapter_writer`] gives: they are the only settings that match what
/// the audio output actually delivers. A second capture session on the same mic
/// would not do here either — the format the writer locks onto has to be the
/// one the device has already settled on, which is the whole reason the aside
/// rides the session that is already running.
pub fn create_audio_writer(
    audio_settings: &NSDictionary<NSString, AnyObject>,
    out_path: &std::path::Path,
) -> Result<AudioWriter> {
    let audio_type =
        unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
    let file_type =
        unsafe { AVFileTypeAppleM4A }.ok_or_else(|| anyhow!("AVFileTypeAppleM4A unavailable"))?;

    // Same reasoning as the chapter writer: the folder can have gone away, and
    // AVAssetWriter's error for that names neither the path nor the cause.
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating output directory {}", dir.display()))?;
    }

    let url_string = NSString::from_str(&out_path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&url_string);
    let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, &file_type) }
        .map_err(|e| anyhow!("could not create asset writer: {e:?}"))?;

    let audio_input = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            &audio_type,
            Some(audio_settings),
        )
    };
    unsafe { audio_input.setExpectsMediaDataInRealTime(true) };

    unsafe {
        if !writer.canAddInput(&audio_input) {
            bail!("writer refused the audio input");
        }
        writer.addInput(&audio_input);
        if !writer.startWriting() {
            bail!("startWriting failed: {:?}", writer.error());
        }
    }

    Ok(AudioWriter {
        writer,
        audio_input,
    })
}

/// The whole select-and-connect flow: pick camera and mic (using saved
/// defaults if available), request permissions, then record until Enter.
pub fn interactive_connect(reselect_camera: bool, reselect_mic: bool) -> Result<()> {
    let mut cfg = config::load();

    // Select camera.
    let camera_devices = list_camera_devices()?;
    if camera_devices.is_empty() {
        bail!("no camera devices found");
    }

    let chosen_camera_uid = match device_picker::resolve_default(
        cfg.camera_device_uid.as_deref(),
        &camera_devices,
        reselect_camera,
    ) {
        Some(uid) => uid,
        None => {
            if let Some(saved) = &cfg.camera_device_uid {
                if !camera_devices.iter().any(|d| &d.uid == saved) {
                    eprintln!(
                        "stream-recorder: saved default camera is no longer connected, pick again"
                    );
                }
            }
            device_picker::prompt_select("camera", &camera_devices)?
        }
    };

    if cfg.camera_device_uid.as_deref() != Some(chosen_camera_uid.as_str()) {
        cfg.camera_device_uid = Some(chosen_camera_uid.clone());
        config::save(&cfg)?;
        println!("stream-recorder: saved as default camera");
    }

    let chosen_camera = camera_devices
        .iter()
        .find(|d| d.uid == chosen_camera_uid)
        .unwrap();
    println!("stream-recorder: using camera \"{}\"", chosen_camera.name);

    // Select microphone.
    let audio_devices = list_audio_devices()?;
    if audio_devices.is_empty() {
        bail!("no audio input devices found");
    }

    let chosen_audio_uid = match device_picker::resolve_default(
        cfg.audio_device_uid.as_deref(),
        &audio_devices,
        reselect_mic,
    ) {
        Some(uid) => uid,
        None => {
            if let Some(saved) = &cfg.audio_device_uid {
                if !audio_devices.iter().any(|d| &d.uid == saved) {
                    eprintln!(
                        "stream-recorder: saved default mic is no longer connected, pick again"
                    );
                }
            }
            device_picker::prompt_select("microphone", &audio_devices)?
        }
    };

    if cfg.audio_device_uid.as_deref() != Some(chosen_audio_uid.as_str()) {
        cfg.audio_device_uid = Some(chosen_audio_uid.clone());
        config::save(&cfg)?;
        println!("stream-recorder: saved as default mic");
    }

    let chosen_audio = audio_devices
        .iter()
        .find(|d| d.uid == chosen_audio_uid)
        .unwrap();
    println!("stream-recorder: using mic \"{}\"", chosen_audio.name);

    // Request permissions.
    println!("stream-recorder: requesting camera access...");
    if !permissions::ensure_video_access()? {
        bail!("camera access denied — enable it in System Settings > Privacy & Security > Camera");
    }

    println!("stream-recorder: requesting microphone access...");
    if !permissions::ensure_audio_access()? {
        bail!(
            "microphone access denied — enable it in System Settings > Privacy & Security > Microphone"
        );
    }

    // Start recording.
    let out_dir = crate::session::output_dir()?;
    let out_path = out_dir.join("av.mp4");

    let conn = Connection::start_capture(&chosen_camera_uid, &chosen_audio_uid)?;
    println!("stream-recorder: warming up capture...");
    conn.wait_for_warmup(Duration::from_secs(5))?;
    conn.install_writer(&out_path)?;
    println!(
        "stream-recorder: recording (session running = {})",
        conn.is_running()
    );
    println!("stream-recorder: press Enter to stop");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();

    let video_captured = conn.video_buffers_captured();
    let audio_captured = conn.audio_buffers_captured();
    conn.stop_and_finish()?;
    println!(
        "stream-recorder: wrote {} video buffers, {} audio buffers -> {}",
        video_captured,
        audio_captured,
        out_path.display()
    );

    Ok(())
}

#[cfg(test)]
mod devices_live {
    /// What AVFoundation actually offers as audio inputs, in order.
    ///
    /// Order still matters a little: `startup::resolve_device` falls back to the
    /// system default input when a saved uid is gone, and to `devices[0]` only
    /// when AVFoundation names no default — and it no longer saves either pick.
    ///
    /// `cargo test capture::av::devices_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn list_the_audio_inputs() {
        let mics = super::list_audio_devices().expect("audio devices");
        println!("{} audio input(s):", mics.len());
        for (i, d) in mics.iter().enumerate() {
            println!("  [{i}] {:<28} uid = {}", d.name, d.uid);
        }
        let saved = crate::config::load().audio_device_uid;
        println!("\nconfig audio_device_uid = {saved:?}");
        match saved.as_deref() {
            Some(uid) if mics.iter().any(|d| d.uid == uid) => {
                println!(
                    "  -> matches {:?}",
                    mics.iter().find(|d| d.uid == uid).unwrap().name
                )
            }
            Some(_) => println!("  -> NO MATCH, falls back to [0] {:?}", mics[0].name),
            None => println!("  -> unset, falls back to [0] {:?}", mics[0].name),
        }
    }
}

#[cfg(test)]
mod capture_live {
    use std::time::{Duration, Instant};

    /// Opens the configured mic and watches the meter for three seconds.
    ///
    /// The one test that answers "is audio arriving at all": it reports buffer
    /// counts and levels straight off the capture queue, so a dead mic, a denied
    /// permission and a silent-but-connected device all look different.
    ///
    /// `cargo test capture::av::capture_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn watch_the_mic_for_three_seconds() {
        assert!(
            crate::permissions::ensure_audio_access().expect("permission query"),
            "microphone permission denied — System Settings > Privacy & Security > Microphone"
        );

        let cfg = crate::config::load();
        let mics = super::list_audio_devices().expect("audio devices");
        let uid = cfg
            .audio_device_uid
            .filter(|uid| mics.iter().any(|d| &d.uid == uid))
            .unwrap_or_else(|| mics[0].uid.clone());
        let name = &mics.iter().find(|d| d.uid == uid).unwrap().name;
        let camera = super::list_camera_devices().expect("cameras")[0]
            .uid
            .clone();
        println!("opening mic {name:?}");

        let conn = super::Connection::start_capture(&camera, &uid).expect("capture");
        let meter = conn.delegate.meter();

        let start = Instant::now();
        let mut last = 0;
        while start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(500));
            let s = meter.snapshot();
            println!(
                "  +{:.1}s  buffers {:<5} (+{:<3})  rms {:>7.1}  peak {:>7.1}  floor {:>7.1}  \
                 speaking {:<5} readable {}",
                start.elapsed().as_secs_f32(),
                s.buffers,
                s.buffers - last,
                s.level_dbfs,
                s.peak_dbfs,
                s.floor_dbfs,
                s.speaking,
                s.readable,
            );
            last = s.buffers;
        }

        let end = meter.snapshot();
        assert!(
            end.buffers > 0,
            "no audio buffers arrived from {name:?} in three seconds — the device is open but \
             delivering nothing"
        );
        assert!(
            end.readable,
            "buffers arrived in a layout the meter cannot read, so every level is wrong"
        );
    }
}
