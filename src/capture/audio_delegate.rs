//! `AVCaptureAudioDataOutputSampleBufferDelegate` implementation: hands each
//! captured sample buffer straight to the `AVAssetWriterInput`, starting the
//! writer's session on the first buffer's own presentation timestamp.
//!
//! This is the first `objc2::define_class!`+`extern_protocol!` conformance
//! in this codebase — everywhere else so far only *calls* Apple APIs, never
//! implements one of their protocols.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVCaptureAudioDataOutputSampleBufferDelegate,
    AVCaptureConnection, AVCaptureOutput,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::NSObject;

pub struct AudioDelegateIvars {
    writer: Retained<AVAssetWriter>,
    input: Retained<AVAssetWriterInput>,
    /// Whether `startSessionAtSourceTime:` has been called yet — must happen
    /// exactly once, anchored to the first buffer actually delivered.
    started: Cell<bool>,
    appended: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = AudioDelegateIvars]
    pub struct AudioDelegate;

    unsafe impl NSObjectProtocol for AudioDelegate {}

    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for AudioDelegate {
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

impl AudioDelegate {
    pub fn new(
        writer: Retained<AVAssetWriter>,
        input: Retained<AVAssetWriterInput>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(AudioDelegateIvars {
            writer,
            input,
            started: Cell::new(false),
            appended: Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub fn buffers_appended(&self) -> u64 {
        self.ivars().appended.get()
    }
}
