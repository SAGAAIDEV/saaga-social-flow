//! The per-frame video operation seam, shared by the camera and screen
//! pipelines and sitting between capture and encode:
//!
//! ```text
//! capture (CVPixelBuffer, CMTime) -> Graph of VideoOps -> AVAssetWriterInput
//! ```
//!
//! This module replaces the single-processor `pipeline::FrameProcessor` stub it
//! grew out of. That stub could not carry real work: `process()` returned
//! nothing, both delegates appended the *original* `CMSampleBuffer` regardless,
//! there was exactly one processor fixed at delegate construction, and it had
//! no lifecycle at all — no place to allocate anything at chapter open, and no
//! place to flush anything at chapter close.
//!
//! `AVCaptureMovieFileOutput` is deliberately not used for camera capture, and
//! this module is the reason why: it muxes directly inside the OS with no seam
//! to intercept pixels, which rules it out the moment a processing stage (face
//! tracking, reframing, a composite of camera over screen) is required.
//! Sample-buffer-driven capture is what exposes raw pixels here at all, and
//! paying its cost is only worth it if something can actually reach them.
//!
//! ## What stage 1 is, and is not
//!
//! Stage 1 is the seam and nothing else. **Not one byte that reaches an
//! `AVAssetWriterInput` changes.** Both delegates still call
//! `appendSampleBuffer` with the original, unmodified buffer, in the same
//! place, under the same `isReadyForMoreMediaData` guard, and the `Flow` a
//! graph returns is deliberately discarded until stage 3. What is new is a
//! graph with a real per-chapter lifecycle and a place for ops to emit data.
//!
//! [`Frame::replace`](frame::Frame::replace) exists as the stage-2 seam and
//! changes nothing today — honouring it needs the
//! `AVAssetWriterInputPixelBufferAdaptor` that stage 2 builds between
//! `addInput` and `startWriting`. Ops that never call it cost nothing.
//!
//! ## One graph per stream
//!
//! A [`Graph`] is rooted at exactly one [`StreamId`], and each stream owns its
//! own. Camera frames arrive on `stream-recorder.av-video` and
//! screen frames on `stream-recorder.screen`, two serial dispatch queues on two
//! OS threads; a single graph reachable from both would need its own `Mutex`
//! and each queue would then block the other for the whole of every frame's op
//! chain. The cross-stream edge is a [`Tap`] — a shared latest-frame cell —
//! precisely because the join cannot be synchronous, so two graphs plus a tap
//! is the same topology expressed in a way that keeps the queues independent.
//!
//! ## Graphs live in `AvState` / `ScreenState`, i.e. per chapter
//!
//! Not as a long-lived delegate ivar. `Router::cut_chapter` installs the
//! replacement chapter *before* it finishes the old one, so a long-lived graph
//! closed in `finish_chapter` would already have run chapter N+1's frames into
//! chapter N's sidecar, and would be closed while live. Because
//! `retake_chapter` and `stop` both take the current state out first, that
//! would have shipped as a cut-only, chapter-2-onwards silent data corruption —
//! the same shape as the audio-format bug documented in `av_delegate`. With the
//! graph inside the state being displaced, the graph that gets closed is by
//! construction the one that owned those frames.
//!
//! The cost is that op state is rebuilt each chapter. For stage 1 (a counter
//! and an accumulator) that is free. Anything expensive to build — stage 2's
//! `CIContext`, a detector's model — should be built once and handed in through
//! [`StreamCtx`] rather than constructed per chapter inside an op.

pub mod composite;
pub mod crop;
pub mod face_track;
pub mod mouse_track;
pub mod frame;
pub mod graph;
pub mod graphs;
pub mod passthrough;
pub mod preview;
pub mod render;
pub mod sidecar;
pub mod sink;
pub mod stats;
pub mod tap;

use anyhow::Result;

// The module's public surface, named in one place. Several of these have no
// caller until stage 3 (`OutputSpec`) or stage 4 (`Tap`, `TapFrame`), and
// `GraphBuilder`/`NodeId` are reached through `graphs.rs` rather than through
// this re-export today — they are listed anyway so the seam's vocabulary is
// visible without reading five files.
#[allow(unused_imports)]
pub use frame::{Frame, StreamId};
#[allow(unused_imports)]
pub use graph::{Flow, Graph, GraphBuilder, NodeId, StreamCtx};
#[allow(unused_imports)]
pub use sidecar::Sidecar;
#[allow(unused_imports)]
pub use preview::{PreviewPort, PreviewSpec};
#[allow(unused_imports)]
pub use render::Renderer;
#[allow(unused_imports)]
pub use sink::OutputSpec;
#[allow(unused_imports)]
pub use tap::{Tap, TapFrame};

/// One video operation: a node in a stream's graph.
///
/// **No `Send` supertrait**, despite the obvious instinct to add one. Three
/// facts, all verified against the vendored crate sources, make it a trap
/// rather than free documentation:
///
/// - The bound would not be *enforced* anywhere. The delegates reach
///   Objective-C as raw pointers and the `Arc<Mutex<…>>` handles are only
///   cloned and locked, so nothing in this crate ever demands `Send` of the
///   thing holding an op — `FrameProcessor: Send` was unchecked for its whole
///   life.
/// - It would become a hard compile error the moment an op caches a CoreVideo
///   handle. `objc2-core-video` 0.3.2 emits `Send`/`Sync` for no CoreFoundation
///   object type at all (`cf_type!` generates `Type`/`Deref`/`Eq`/`Hash`/
///   `Debug` and nothing else), and `CFRetained<T>` is `Send` only where
///   `T: Send + Sync` — so a `CFRetained<CVPixelBufferPool>` field in a stage-2
///   op would stop that op coercing to `Box<dyn VideoOp>`, and every op written
///   before then would have to be revisited.
/// - Each stream's graph runs on exactly one serial queue, so the bound would
///   be claiming a property nothing needs.
///
/// The one place a CoreVideo handle genuinely crosses a thread boundary is the
/// [`Tap`], which carries a narrow, justified `unsafe impl Send + Sync` on the
/// type that actually crosses instead of a blanket bound on every op.
///
/// ## Contract
///
/// [`apply`](VideoOp::apply) runs **on a capture queue**, inside an
/// Objective-C callback, while the delegate holds its writer-state `Mutex`. It
/// must not block. On the camera path the audio callback contends for that same
/// guard, so a slow `apply` stalls audio as well as video — that was always
/// true of this seam, a no-op `process()` merely made it invisible.
///
/// [`open`](VideoOp::open) and [`close`](VideoOp::close) run on the main thread
/// from the Router, at the chapter boundary, and default to doing nothing so a
/// pure analyzer or a passthrough is three lines.
pub trait VideoOp {
    /// Stable identifier, used as the sidecar's key and in log lines. It ends
    /// up in a filename (`chapter-01.<name>.json`), so keep it filename-safe.
    fn name(&self) -> &'static str;

    /// Chapter open. Allocate here, not in `apply`.
    fn open(&mut self, ctx: &StreamCtx) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// One frame, on the capture queue. See the contract above.
    fn apply(&mut self, frame: &mut Frame) -> Result<Flow>;

    /// Chapter close. Flush whatever this op learned into the sidecar; it is
    /// written to disk immediately afterwards.
    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        let _ = sidecar;
        Ok(())
    }
}
