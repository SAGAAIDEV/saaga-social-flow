//! `SCStreamOutput` implementation: ScreenCaptureKit's side of the same shape
//! `av_delegate` implements for the camera — frames arrive on a dispatch
//! queue, run through this chapter's [`crate::ops`] graph, and are appended to
//! whatever `AVAssetWriterInput` is currently installed.
//!
//! **Writerless by default**, for the same reason as `AvDelegate`: the stream
//! runs from the moment a display is selected, but frames are counted and
//! dropped until the Router installs a writer at a chapter boundary. That
//! keeps chapter files free of the ragged first second of a stream that is
//! still negotiating its output size.
//!
//! **Not every delivered buffer is a frame.** ScreenCaptureKit sends buffers
//! with an `SCFrameStatus` of `Idle`/`Blank`/`Suspended` that carry no new
//! pixels — a static screen produces a steady drip of them. They must be
//! dropped rather than appended: their image buffer is stale or absent, so
//! encoding them inflates the file with duplicate frames and, worse, invents
//! motion timing that never happened. This is the screen-side analogue of the
//! audio-format trap in `av_delegate` — the writer reports no error either way.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, Message};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{kCMTimeInvalid, CMSampleBuffer, CMSampleTimingInfo, CMTime};
use objc2_foundation::{NSDictionary, NSError, NSNumber, NSObject, NSString};
use objc2_screen_capture_kit::{
    SCFrameStatus, SCStream, SCStreamDelegate, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};

use crate::capture::pause::{self, Pause};
use crate::ops::{Graph, StreamId, Tap};

/// Writer state for one screen chapter file.
pub struct ScreenState {
    pub writer: Retained<AVAssetWriter>,
    pub video_input: Retained<AVAssetWriterInput>,
    /// The seam an op's *modified* pixels reach the encoder through.
    ///
    /// Held even though nothing appends through it yet: it must be constructed
    /// between `addInput` and `startWriting` or AVFoundation throws, so it
    /// cannot be added later from the frame path that will need it. See
    /// `screen_writer::create_screen_writer`.
    #[allow(dead_code)]
    pub adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    /// This chapter's video op graph.
    ///
    /// It lives here rather than in the delegate's ivars for two reasons. It is
    /// per chapter by design (see the `ops` module docs), and — the reason this
    /// side used to be stricter than `AvDelegate`'s — a graph must not sit in a
    /// `!Sync` cell on this class at all: `ScreenDelegate` also serves
    /// `SCStreamDelegate` callbacks, which ScreenCaptureKit may deliver on a
    /// different queue from the sample queue, so a `RefCell` here would be
    /// unsound rather than merely unnecessary. Parking it in `ScreenState` puts
    /// it behind the `Mutex` the writer already uses, which satisfies that for
    /// free.
    pub graph: Graph,
    /// This chapter's t=0, on the *stream's* clock — the counterpart to
    /// [`crate::capture::av_delegate::AvState::anchor`], derived by the Router
    /// from the same instant in real time. That shared origin is what makes
    /// `chapter-NN.mp4` and `chapter-NN-screen.mp4` line up on a timeline.
    pub anchor: CMTime,
    /// The breaks taken during this chapter — see [`crate::capture::pause`].
    /// Fed by the Router from the same real instants as the camera file's, so
    /// the two files stay aligned across a break as they are across a cut.
    pub pause: Pause,
}

pub struct ScreenDelegateIvars {
    /// `None` means "running, but nothing is being written" — see module docs.
    /// `Arc` so the Router can hold the same handle and swap chapters.
    state: Arc<Mutex<Option<ScreenState>>>,
    /// Frames appended to the current writer.
    appended: AtomicU64,
    /// Complete frames delivered, whether or not a writer was attached. The
    /// warmup gate polls this from the main thread.
    seen: AtomicU64,
    /// Buffers dropped for carrying no new pixels (see module docs). Expected
    /// to be nonzero on a still screen — it is not an error count.
    skipped: AtomicU64,
    /// Set if the stream dies on its own (display sleep, disconnect, the user
    /// revoking permission mid-session). Screen capture otherwise stops
    /// silently and the chapter just ends early with no explanation.
    stop_error: Mutex<Option<String>>,
    /// The most recent complete frame, kept so a new chapter can open with
    /// the screen as it looked at the cut. See [`ScreenDelegate::seed_chapter`].
    last_frame: Mutex<Option<Retained<CMSampleBuffer>>>,
    /// Latest complete frame for the camera preview composite. Published even
    /// when no chapter writer is installed.
    tap: Arc<Tap>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = ScreenDelegateIvars]
    pub struct ScreenDelegate;

    unsafe impl NSObjectProtocol for ScreenDelegate {}

    unsafe impl SCStreamOutput for ScreenDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            output_type: SCStreamOutputType,
        ) {
            if output_type == SCStreamOutputType::Screen {
                self.handle_frame(sample_buffer);
            }
        }
    }

    unsafe impl SCStreamDelegate for ScreenDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_didStopWithError(&self, _stream: &SCStream, error: &NSError) {
            let message = error.localizedDescription().to_string();
            eprintln!("stream-recorder: screen capture stopped: {message}");
            *self.ivars().stop_error.lock().unwrap() = Some(message);
        }
    }
);

impl ScreenDelegate {
    pub fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ScreenDelegateIvars {
            state: Arc::new(Mutex::new(None)),
            appended: AtomicU64::new(0),
            seen: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            stop_error: Mutex::new(None),
            last_frame: Mutex::new(None),
            tap: Tap::new(),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// The handle the Router swaps chapter writers through.
    pub fn state_arc(&self) -> Arc<Mutex<Option<ScreenState>>> {
        self.ivars().state.clone()
    }

    pub fn tap(&self) -> Arc<Tap> {
        Arc::clone(&self.ivars().tap)
    }

    fn handle_frame(&self, sample_buffer: &CMSampleBuffer) {
        let ivars = self.ivars();

        // Drop anything that isn't a real new frame, before it can reach a
        // writer. A missing status is treated as complete: the attachment is
        // documented as always present, and dropping every frame on an
        // unexpected shape would silently produce an empty file.
        if frame_status(sample_buffer).is_some_and(|s| s != SCFrameStatus::Complete) {
            ivars.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        ivars.seen.fetch_add(1, Ordering::Relaxed);
        *ivars.last_frame.lock().unwrap() = Some(sample_buffer.retain());

        let pts = unsafe { sample_buffer.presentation_time_stamp() };
        if let Some(pixels) = unsafe { sample_buffer.image_buffer() } {
            ivars.tap.publish_buffer(pixels, pts, StreamId::Screen);
        }

        let mut guard = ivars.state.lock().unwrap();
        // No writer installed (warmup, or between stop and exit): drop it.
        let Some(state) = guard.as_mut() else { return };

        // Where the frame lands in the file, or nowhere: before this chapter's
        // anchor, during a break, or straggling in from one — see `Pause::place`.
        let Some(placed) = state.pause.place(pts) else {
            return;
        };

        // Run this chapter's op graph. Its `Run` is deliberately discarded
        // until the writers are per-sink — the append below is byte-for-byte
        // what it has always been, so the default passthrough graph records
        // exactly the file it always did. `run` swallows its own errors and
        // panics rather than unwinding into ScreenCaptureKit; see its docs.
        if let Some(pixels) = unsafe { sample_buffer.image_buffer() } {
            let _run = state.graph.run(pixels, placed);
        }

        // Copied only once a break has been taken out; until then the frame
        // goes through exactly as it arrived.
        let shifted;
        let frame: &CMSampleBuffer = if state.pause.is_shifted() {
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
        if unsafe { state.video_input.isReadyForMoreMediaData() } {
            unsafe { state.video_input.appendSampleBuffer(frame) };
            ivars.appended.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Give the newly installed chapter its opening frame: the last frame the
    /// stream delivered, retimed to the chapter's anchor.
    ///
    /// ScreenCaptureKit is change-driven. It sends a frame when something on
    /// screen moves and nothing when it doesn't, which is efficient but means
    /// a chapter recorded while the screen sat still receives *no frames at
    /// all* — and an `AVAssetWriter` finished with zero samples writes a file
    /// that will not even open. A ten-minute chapter of someone talking to
    /// camera over a static diagram is not an edge case, it is the format.
    ///
    /// So each chapter opens with the screen as it looked at the cut. The
    /// frame has to be *retimed*: its own timestamp predates the anchor, and
    /// the writer discards anything before its session start.
    ///
    /// **This frame bypasses the op graph**, appending the retimed
    /// `CMSampleBuffer` straight to the input below. That is correct in stage 1
    /// — the graph cannot change a frame yet, so routing the seed through it
    /// would be a behaviour change for no gain, and it would also count a frame
    /// the stream never delivered.
    ///
    /// It is a trap for stage 2, and this note exists so that stage cannot miss
    /// it: the moment an op can change dimensions, the seeded frame is the
    /// *wrong size* for the writer that has just been installed, so the writer
    /// rejects it or emits a broken first frame — reintroducing exactly the
    /// zero-sample, unopenable file this function exists to prevent. Stage 2
    /// must either route the seed through the graph or make the sink's
    /// dimension check reject it loudly.
    pub fn seed_chapter(&self, anchor: CMTime) {
        let ivars = self.ivars();
        let frame = ivars.last_frame.lock().unwrap();
        let Some(frame) = frame.as_ref() else { return };

        let timing = CMSampleTimingInfo {
            // Invalid duration: this is a still frame held until the next real
            // one arrives, and the writer derives its span from what follows.
            duration: unsafe { kCMTimeInvalid },
            presentationTimeStamp: anchor,
            decodeTimeStamp: unsafe { kCMTimeInvalid },
        };
        let mut retimed: *mut CMSampleBuffer = std::ptr::null_mut();
        let status = unsafe {
            CMSampleBuffer::create_copy_with_new_timing(
                None,
                frame,
                1,
                &timing,
                NonNull::from(&mut retimed),
            )
        };
        if status != 0 || retimed.is_null() {
            eprintln!("stream-recorder: could not seed the screen chapter's first frame (status {status})");
            return;
        }
        let retimed = unsafe { CFRetained::from_raw(NonNull::new_unchecked(retimed)) };

        let mut guard = ivars.state.lock().unwrap();
        let Some(state) = guard.as_mut() else { return };
        if unsafe { state.video_input.isReadyForMoreMediaData() } {
            unsafe { state.video_input.appendSampleBuffer(&retimed) };
            ivars.appended.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn frames_appended(&self) -> u64 {
        self.ivars().appended.load(Ordering::Relaxed)
    }

    pub fn frames_seen(&self) -> u64 {
        self.ivars().seen.load(Ordering::Relaxed)
    }

    pub fn frames_skipped(&self) -> u64 {
        self.ivars().skipped.load(Ordering::Relaxed)
    }

    /// The error the stream died with, if it stopped on its own.
    pub fn stop_error(&self) -> Option<String> {
        self.ivars().stop_error.lock().unwrap().clone()
    }
}

/// Read the `SCStreamFrameInfoStatus` attachment off a delivered buffer.
///
/// `None` means the attachment was absent or unreadable, which callers treat
/// as "assume it's a real frame" rather than dropping it.
fn frame_status(sample_buffer: &CMSampleBuffer) -> Option<SCFrameStatus> {
    // false: read the existing attachments, never fabricate an empty array.
    let attachments = unsafe { sample_buffer.sample_attachments_array(false) }?;
    if attachments.count() < 1 {
        return None;
    }
    let dict = unsafe { attachments.value_at_index(0) };
    if dict.is_null() {
        return None;
    }
    // Toll-free bridge: the attachment entries are CFDictionaries, and
    // NSDictionary keying is far more legible than CF's void-pointer
    // accessors for what is a single lookup by a Foundation string key.
    let dict: &NSDictionary<NSString, AnyObject> = unsafe { &*dict.cast() };
    let value = dict.objectForKey(unsafe { SCStreamFrameInfoStatus })?;
    let number = value.downcast::<NSNumber>().ok()?;
    Some(SCFrameStatus(number.integerValue()))
}

impl ScreenState {
    /// Create the state for one screen chapter and open its writer session at
    /// `anchor`, a time on the screen stream's clock.
    ///
    /// `graph` must already be opened; the Router does that on the main thread
    /// before building the chapter.
    pub fn new_for_chapter(
        writer: super::screen_writer::ScreenWriter,
        anchor: CMTime,
        graph: Graph,
    ) -> ScreenState {
        let super::screen_writer::ScreenWriter {
            writer,
            input: video_input,
            adaptor,
        } = writer;
        unsafe { writer.startSessionAtSourceTime(anchor) };
        ScreenState {
            writer,
            video_input,
            adaptor,
            graph,
            anchor,
            pause: Pause::new(anchor),
        }
    }

    /// Mark the input finished and wait for the writer to flush to disk.
    pub fn finish(&self) -> anyhow::Result<()> {
        use anyhow::{bail, Context};
        use block2::RcBlock;
        use objc2_av_foundation::AVAssetWriterStatus;
        use std::sync::mpsc;
        use std::time::Duration;

        unsafe { self.video_input.markAsFinished() };

        let (tx, rx) = mpsc::channel::<()>();
        let handler = RcBlock::new(move || {
            let _ = tx.send(());
        });
        unsafe { self.writer.finishWritingWithCompletionHandler(&handler) };
        rx.recv_timeout(Duration::from_secs(30))
            .context("timed out waiting for the screen asset writer to finish")?;

        let status = unsafe { self.writer.status() };
        if status == AVAssetWriterStatus::Completed {
            Ok(())
        } else {
            bail!(
                "screen asset writer finished with status {status:?}: {:?}",
                unsafe { self.writer.error() }
            )
        }
    }
}
