//! Unified `AVCaptureVideoDataOutputSampleBufferDelegate` AND
//! `AVCaptureAudioDataOutputSampleBufferDelegate` implementation: both capture
//! streams deliver to the same `AVAssetWriter` with synchronized playback,
//! anchored to whichever stream delivers its first sample buffer.
//!
//! **Concurrency**: Video and audio callbacks run on *separate* dispatch
//! queues (one serial queue per stream, mirroring the single-capture pattern),
//! which means they can run *concurrently on different OS threads*. Shared
//! state (the writer and both inputs) is guarded by a `std::sync::Mutex`,
//! not `RefCell` — `RefCell` is deliberately !Sync and is unsound when
//! accessed from multiple threads.
//!
//! **The op graph lives in [`AvState`]**, not in this delegate's ivars, where
//! the frame processor it replaced used to sit. It lives and dies with the
//! chapter it describes, so op state resets at a cut and a sidecar cannot
//! straddle one — see the `ops` module docs for the cut-only corruption the
//! other arrangement would have shipped. That also puts it under the same
//! `Mutex` the video callback already held across the old processor call, so
//! the contention profile is unchanged: it was, and still is, true that a slow
//! op stalls the *audio* callback too, because audio contends for that same
//! guard below. A no-op `process()` merely made that invisible.
//!
//! **One method, two protocols**: Both video and audio capture outputs send
//! `captureOutput:didOutputSampleBuffer:fromConnection:` callbacks (same
//! Objective-C selector). In Objective-C, a single method can satisfy both
//! protocols if they both declare that method. We register this one delegate
//! for both outputs, and the shared callback dispatches to the appropriate
//! handler (video with frame processing, audio without) based on which input
//! is ready in the writer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVCaptureAudioDataOutputSampleBufferDelegate,
    AVCaptureConnection, AVCaptureOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_foundation::NSObject;

use crate::capture::composed::ComposedWriter;
use crate::capture::level::SpeechMeter;
use crate::capture::pause::{self, Pause};
use crate::ops::Graph;

/// The mutable state shared between video and audio callbacks.
/// Guarded by Arc<Mutex<>> in the AvDelegate's ivars so it can be swapped
/// by the Router when cutting chapters.
pub struct AvState {
    pub writer: Retained<AVAssetWriter>,
    pub video_input: Retained<AVAssetWriterInput>,
    pub audio_input: Retained<AVAssetWriterInput>,
    /// This chapter's video op graph, opened by the Router before this state
    /// was installed and closed by it after this state is displaced.
    ///
    /// Per chapter rather than per delegate, deliberately: the graph that gets
    /// closed is then by construction the one that owned this chapter's frames,
    /// which is what makes cut/retake/stop correct without any new Router
    /// sequencing. See the `ops` module docs.
    pub graph: Graph,
    /// Composed H/V files, fed from the session preview graph's file sinks.
    pub composed: Vec<ComposedWriter>,
    /// Where every buffer lands in this chapter's files — see
    /// [`crate::capture::pause`]. It starts from the chapter's t=0 on the
    /// capture session's clock, chosen by the Router when the chapter was
    /// opened and already passed to `startSessionAtSourceTime:`, and moves with
    /// every break taken since.
    ///
    /// Anchoring to a chosen instant rather than to the first buffer is what
    /// lets the camera file and the screen file line up: the Router derives
    /// both anchors from one moment in real time, so both files put the same
    /// instant at t=0 even though their buffers arrive at different times on
    /// different clocks. Buffers older than the anchor belong to the previous
    /// chapter and are dropped.
    pub pause: Pause,
    /// The audio layout this chapter's writer input locked onto, taken from
    /// the first audio buffer appended. See [`AvDelegate::check_audio_format`].
    pub audio_asbd: Option<AudioStreamBasicDescription>,
    /// One warning per chapter, not one per buffer.
    pub asbd_warned: bool,
}

pub struct AvDelegateIvars {
    /// Shared mutable state guarded by Mutex because video and audio callbacks
    /// run concurrently on separate dispatch queues (separate OS threads).
    /// Wrapped in Arc so the Router can keep a reference to it for swapping.
    ///
    /// `None` means capture is running with no writer attached: buffers are
    /// counted (see the `*_seen` counters) and dropped. This is the warmup
    /// state — the session runs writerless until both streams are flowing,
    /// because writer inputs lock onto the format of the first buffers they
    /// see, and external devices (USB audio interfaces especially) can
    /// renegotiate their format while the session settles. A writer built
    /// from cold-start buffers came out as full-chapter static once.
    state: std::sync::Arc<Mutex<Option<AvState>>>,
    /// Session-scoped preview graph. Runs on every video frame, including
    /// while no chapter writer is installed, so the UI can show H/V before
    /// the first New Chapter press.
    preview: Mutex<Option<Graph>>,
    /// A figure's aside — see [`crate::figure::aside`] — while one is being
    /// recorded. Fed every audio buffer and nothing else, independently of
    /// `state`: the chapter is paused while this records, and the two never
    /// take the same second of sound.
    aside: Mutex<Option<AsideState>>,
    /// The input level and speech-time meter, fed by every audio buffer whether
    /// or not a writer is installed. Shared rather than owned so the app can
    /// read it from the main thread on its tick — see `app::clock`.
    meter: std::sync::Arc<SpeechMeter>,
    /// Buffers appended to the current writer. Atomics, not Cells: the main
    /// thread reads these live (summary printing) while capture threads write.
    video_frames: AtomicU64,
    audio_frames: AtomicU64,
    /// Buffers *delivered* by the session, whether or not a writer was
    /// attached. The warmup gate polls these from the main thread.
    video_seen: AtomicU64,
    audio_seen: AtomicU64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = AvDelegateIvars]
    pub struct AvDelegate;

    unsafe impl NSObjectProtocol for AvDelegate {}

    // Implement the video protocol. The Objective-C selector
    // captureOutput:didOutputSampleBuffer:fromConnection: will be registered
    // and will satisfy both the video and audio delegate protocols, since
    // both declare a method with that same selector.
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for AvDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn captureOutput_didOutputSampleBuffer_fromConnection(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.handle_sample_buffer(sample_buffer);
        }
    }

    // Also explicitly satisfy the audio protocol by reusing the same method.
    // In Objective-C, when two protocols declare the same method selector,
    // a single implementation satisfies both. This is declared as a separate
    // trait, but points to the same underlying Objective-C method.
    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for AvDelegate {}
);

impl AvDelegate {
    /// Create a delegate with no writer attached — capture runs and buffers
    /// are counted-and-dropped until a writer is installed via `state_arc()`
    /// (normally by the Router, or `Connection::install_writer`).
    // `state` is shared with the capture callbacks, which the system runs on
    // its own queue, so the `Arc` is real; its payload is an Objective-C handle
    // this crate cannot mark `Send`, which is all the lint sees.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(AvDelegateIvars {
            state: std::sync::Arc::new(Mutex::new(None)),
            preview: Mutex::new(None),
            aside: Mutex::new(None),
            video_frames: AtomicU64::new(0),
            meter: std::sync::Arc::new(SpeechMeter::from_config()),
            audio_frames: AtomicU64::new(0),
            video_seen: AtomicU64::new(0),
            audio_seen: AtomicU64::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Get a reference to the shared state Arc for external manipulation (e.g., by Router).
    pub fn state_arc(&self) -> std::sync::Arc<Mutex<Option<AvState>>> {
        self.ivars().state.clone()
    }

    pub fn set_preview(&self, graph: Graph) {
        *self.ivars().preview.lock().unwrap() = Some(graph);
    }

    /// Start feeding audio to an aside. Refused while one is already recording:
    /// replacing it would drop a writer that was never finished, and an `.m4a`
    /// that never got `finishWriting` does not play.
    pub fn install_aside(&self, state: AsideState) -> anyhow::Result<()> {
        let mut guard = self.ivars().aside.lock().unwrap();
        if guard.is_some() {
            anyhow::bail!("an aside is already being recorded");
        }
        *guard = Some(state);
        Ok(())
    }

    /// Stop feeding the aside and hand its writer back to be finished. Taken
    /// out before it is finished, so a buffer still in flight is dropped rather
    /// than appended to a writer that has been told it is done.
    pub fn take_aside(&self) -> Option<AsideState> {
        self.ivars().aside.lock().unwrap().take()
    }

    /// Unified handler for both video and audio sample buffers. Determines
    /// whether the buffer is from the video or audio stream by checking for
    /// the presence of a pixel buffer, then appends to the appropriate input.
    fn handle_sample_buffer(&self, sample_buffer: &CMSampleBuffer) {
        let ivars = self.ivars();

        // Determine if this is a video or audio buffer by checking for pixel data.
        let is_video = unsafe { sample_buffer.image_buffer() }.is_some();
        let pts = unsafe { sample_buffer.presentation_time_stamp() };
        let mut composed_frames = Vec::new();
        if is_video {
            ivars.video_seen.fetch_add(1, Ordering::Relaxed);
            if let Some(pixels) = unsafe { sample_buffer.image_buffer() } {
                if let Ok(mut preview) = ivars.preview.lock() {
                    if let Some(graph) = preview.as_mut() {
                        composed_frames = graph.run(pixels, pts).appended;
                    }
                }
            }
        } else {
            ivars.audio_seen.fetch_add(1, Ordering::Relaxed);
            // Before the state lock below, deliberately: the level meter has to
            // read during warmup and between takes too, and metering a buffer
            // must never wait on the thread appending video.
            ivars.meter.observe(sample_buffer);
            // The aside, when there is one, takes the sound as it arrives — its
            // own session starts at its own anchor, so nothing here retimes.
            if let Some(aside) = ivars.aside.lock().unwrap().as_ref() {
                if unsafe { pts.compare(aside.anchor) } >= 0
                    && unsafe { aside.audio_input.isReadyForMoreMediaData() }
                {
                    unsafe { aside.audio_input.appendSampleBuffer(sample_buffer) };
                }
            }
        }

        let mut guard = ivars.state.lock().unwrap();
        // No writer installed (warmup, or between stop and exit): drop the buffer.
        let Some(state) = guard.as_mut() else { return };

        // Where this buffer lands in the chapter's files, or nowhere: before
        // the chapter began, during a break, or straggling in from one — see
        // `Pause::place`. Dropping here rather than letting the writer refuse
        // keeps the appended counters honest.
        let Some(placed) = state.pause.place(pts) else {
            return;
        };
        // Copied only once there is a shift to apply: before any break, every
        // buffer is appended exactly as it arrived.
        let shifted;
        let sample: &CMSampleBuffer = if state.pause.is_shifted() {
            match pause::retime(sample_buffer, placed) {
                Some(buffer) => {
                    shifted = buffer;
                    &shifted
                }
                None => return,
            }
        } else {
            sample_buffer
        };

        if is_video {
            if let Some(pixels) = unsafe { sample_buffer.image_buffer() } {
                let _run = state.graph.run(pixels, placed);
            }

            if unsafe { state.video_input.isReadyForMoreMediaData() } {
                unsafe { state.video_input.appendSampleBuffer(sample) };
                ivars.video_frames.fetch_add(1, Ordering::Relaxed);
            }
            for (ordinal, pixels) in composed_frames {
                let Some(sink) = state.composed.get(ordinal) else {
                    continue;
                };
                if unsafe { sink.video_input.isReadyForMoreMediaData() } {
                    unsafe {
                        sink.adaptor
                            .appendPixelBuffer_withPresentationTime(&pixels, placed);
                    }
                }
            }
        } else {
            Self::check_audio_format(state, sample_buffer);
            if unsafe { state.audio_input.isReadyForMoreMediaData() } {
                unsafe { state.audio_input.appendSampleBuffer(sample) };
                ivars.audio_frames.fetch_add(1, Ordering::Relaxed);
            }
            for sink in &state.composed {
                if let Some(audio) = sink.audio_input.as_ref() {
                    if unsafe { audio.isReadyForMoreMediaData() } {
                        unsafe { audio.appendSampleBuffer(sample) };
                    }
                }
            }
        }
    }

    /// Warn — loudly, once per chapter — if the audio layout changes after
    /// the writer input has locked onto it.
    ///
    /// An `AVAssetWriterInput` takes its source format from the first buffer
    /// it is handed and reinterprets every later buffer through that layout.
    /// If the device renegotiates mid-chapter, nothing fails: the encoder
    /// happily reads the new bytes with the old stride and emits perfectly
    /// valid AAC containing pure static. That shipped — chapter 1's writer
    /// was installed while the Elgato XLR Dock was still on 24-bit packed
    /// (3 bytes/frame) and the dock moved to 32-bit float (4 bytes/frame) as
    /// AppKit spun up, so the whole chapter decoded 4 bytes of data as 3 and
    /// came out 4/3 too long and 100% noise, with no error anywhere.
    ///
    /// Opening chapter 1 from a button press (see `app.rs`) keeps the writer
    /// away from that transition; this is the backstop for a renegotiation
    /// that happens anyway. It cannot repair the chapter — by the time a
    /// mismatched buffer arrives the encoder is already committed — so the
    /// job here is purely to make the corruption visible while recording
    /// instead of hours later in the edit.
    fn check_audio_format(state: &mut AvState, sample_buffer: &CMSampleBuffer) {
        let Some(asbd) = crate::capture::level::asbd_of(sample_buffer) else {
            return;
        };
        match state.audio_asbd {
            None => state.audio_asbd = Some(asbd),
            Some(locked) if locked != asbd && !state.asbd_warned => {
                state.asbd_warned = true;
                eprintln!(
                    "stream-recorder: WARNING — the mic changed audio format mid-chapter; \
                     this chapter's audio is being encoded through the old layout and will \
                     be static. Cut a new chapter and re-record it.\n  \
                     locked: {:.0} Hz, {} bytes/frame, {} bits/ch, flags 0x{:x}\n  \
                     now:    {:.0} Hz, {} bytes/frame, {} bits/ch, flags 0x{:x}",
                    locked.mSampleRate,
                    locked.mBytesPerFrame,
                    locked.mBitsPerChannel,
                    locked.mFormatFlags,
                    asbd.mSampleRate,
                    asbd.mBytesPerFrame,
                    asbd.mBitsPerChannel,
                    asbd.mFormatFlags,
                );
            }
            _ => {}
        }
    }

    /// The shared input-level and speech-time meter.
    pub fn meter(&self) -> std::sync::Arc<SpeechMeter> {
        self.ivars().meter.clone()
    }

    pub fn video_frames_appended(&self) -> u64 {
        self.ivars().video_frames.load(Ordering::Relaxed)
    }

    pub fn audio_frames_appended(&self) -> u64 {
        self.ivars().audio_frames.load(Ordering::Relaxed)
    }

    pub fn video_buffers_seen(&self) -> u64 {
        self.ivars().video_seen.load(Ordering::Relaxed)
    }

    pub fn audio_buffers_seen(&self) -> u64 {
        self.ivars().audio_seen.load(Ordering::Relaxed)
    }
}

impl AvState {
    /// Create a fresh AvState for a chapter file and open its writer session
    /// at `anchor`, a time on the capture session's clock.
    ///
    /// The session is started here rather than on the first buffer so that the
    /// caller — which is also opening the screen chapter — decides where t=0
    /// is for both files at once. See the `pause` field.
    ///
    /// `graph` must already be opened — the Router does that before it builds
    /// the chapter, on the main thread, so no allocation an op wants happens on
    /// a capture queue.
    pub fn new_for_chapter(
        chapter_writer: crate::capture::av::ChapterWriter,
        composed: Vec<ComposedWriter>,
        anchor: CMTime,
        graph: Graph,
    ) -> AvState {
        unsafe { chapter_writer.writer.startSessionAtSourceTime(anchor) };
        for sink in &composed {
            unsafe { sink.writer.startSessionAtSourceTime(anchor) };
        }
        AvState {
            writer: chapter_writer.writer,
            video_input: chapter_writer.video_input,
            audio_input: chapter_writer.audio_input,
            graph,
            composed,
            pause: Pause::new(anchor),
            audio_asbd: None,
            asbd_warned: false,
        }
    }

    /// Mark inputs finished and wait for the writer to complete.
    pub fn finish(&self) -> anyhow::Result<()> {
        unsafe {
            self.video_input.markAsFinished();
            self.audio_input.markAsFinished();
            for sink in &self.composed {
                sink.video_input.markAsFinished();
                if let Some(audio) = sink.audio_input.as_ref() {
                    audio.markAsFinished();
                }
            }
        }

        finish_writer(&self.writer)?;
        for sink in &self.composed {
            if let Err(error) = finish_writer(&sink.writer) {
                eprintln!(
                    "stream-recorder: composed {} writer failed to finish: {error:#}",
                    sink.spec.name()
                );
            }
        }
        Ok(())
    }
}

/// The writer for a figure's aside: the microphone alone, from the instant the
/// author began explaining — see [`crate::figure::aside`].
///
/// Beside the chapter state rather than a variant of it. It takes only audio,
/// it is never paused or retimed, and it lives in its own slot on the delegate
/// so the chapter's writers can stay installed — paused — while it records.
pub struct AsideState {
    pub writer: Retained<AVAssetWriter>,
    pub audio_input: Retained<AVAssetWriterInput>,
    /// The file's t=0, on the capture session's clock. Buffers older than this
    /// were the take's, not the aside's, and are not appended.
    pub anchor: CMTime,
}

impl AsideState {
    pub fn new(writer: crate::capture::av::AudioWriter, anchor: CMTime) -> AsideState {
        unsafe { writer.writer.startSessionAtSourceTime(anchor) };
        AsideState {
            writer: writer.writer,
            audio_input: writer.audio_input,
            anchor,
        }
    }

    /// Mark the input finished and wait for the writer to complete.
    pub fn finish(&self) -> anyhow::Result<()> {
        unsafe { self.audio_input.markAsFinished() };
        finish_writer(&self.writer)
    }
}

fn finish_writer(writer: &Retained<AVAssetWriter>) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    use block2::RcBlock;
    use objc2_av_foundation::AVAssetWriterStatus;
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel::<()>();
    let handler = RcBlock::new(move || {
        let _ = tx.send(());
    });
    unsafe { writer.finishWritingWithCompletionHandler(&handler) };
    rx.recv_timeout(Duration::from_secs(30))
        .context("timed out waiting for the asset writer to finish")?;

    let status = unsafe { writer.status() };
    if status == AVAssetWriterStatus::Completed {
        Ok(())
    } else {
        bail!(
            "asset writer finished with status {status:?}: {:?}",
            unsafe { writer.error() }
        )
    }
}
