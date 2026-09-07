//! `AVCaptureVideoDataOutputSampleBufferDelegate` implementation for the legacy
//! `camera` subcommand: appends every captured sample buffer to one
//! `AVAssetWriterInput`, starting the writer's session on the first buffer's own
//! presentation timestamp.
//!
//! **There is no processing seam on this path, deliberately.** The module doc
//! here used to claim the frame passed through a `FrameProcessor` and that the
//! "(potentially modified) sample buffer" was then appended; neither was ever
//! true — the processor's return value was `()` and the append below has always
//! taken the original buffer. The processor is gone rather than replaced with an
//! empty [`crate::ops::Graph`], because a graph that is never opened and never
//! closed is exactly the silent-lifecycle shape this codebase documents against,
//! and this delegate has no chapter boundary to hang one on: it anchors to its
//! own first buffer and runs until the process stops.
//!
//! The graph lives on the `av` and `screen` paths, which do have chapters. This
//! one is reachable only from the `camera` subcommand and is kept as the
//! single-stream smoke test it has always been.

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVCaptureConnection, AVCaptureOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::NSObject;

pub struct VideoDelegateIvars {
    writer: Retained<AVAssetWriter>,
    input: Retained<AVAssetWriterInput>,
    /// Whether `startSessionAtSourceTime:` has been called yet — must happen
    /// exactly once, anchored to the first buffer actually delivered.
    started: std::cell::Cell<bool>,
    appended: std::cell::Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = VideoDelegateIvars]
    pub struct VideoDelegate;

    unsafe impl NSObjectProtocol for VideoDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for VideoDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn captureOutput_didOutputSampleBuffer_fromConnection(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let ivars = self.ivars();
            if !ivars.started.get() {
                let pts = unsafe { sample_buffer.presentation_time_stamp() };
                unsafe { ivars.writer.startSessionAtSourceTime(pts) };
                ivars.started.set(true);
            }
            if unsafe { ivars.input.isReadyForMoreMediaData() } {
                unsafe { ivars.input.appendSampleBuffer(sample_buffer) };
                ivars.appended.set(ivars.appended.get() + 1);
            }
        }
    }
);

impl VideoDelegate {
    pub fn new(
        writer: Retained<AVAssetWriter>,
        input: Retained<AVAssetWriterInput>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(VideoDelegateIvars {
            writer,
            input,
            started: std::cell::Cell::new(false),
            appended: std::cell::Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub fn buffers_appended(&self) -> u64 {
        self.ivars().appended.get()
    }
}
