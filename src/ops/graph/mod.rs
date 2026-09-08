//! The graph: how ops are wired together, and the one call each delegate makes
//! per frame.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use objc2::rc::Retained;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMClock, CMTime};
use objc2_core_video::CVImageBuffer;

use super::frame::{self, Frame, StreamId};
use super::preview::PreviewPort;
use super::render::{Pool, Renderer};
use super::sidecar::{Counters, Sidecar};
use super::sink::OutputSpec;

/// What the graph decided about a frame.
///
/// Discarded by both delegates in stage 1 — the append is byte-for-byte what it
/// has always been. Stage 3 is where it starts to matter, once a sink is a node
/// rather than a Router-side hardcoded pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// No shipped op returns this yet — the presets in `graphs.rs` are
    /// analyzers — so it is exercised by tests until an op has a reason to
    /// throw a frame away.
    #[allow(dead_code)]
    Drop,
}

/// What one frame's walk of the graph produced.
///
/// A `Vec` rather than one frame, because a graph is a graph: a screen capture
/// branching into a horizontal and a vertical crop yields two buffers from one
/// delivered frame, bound for two different writers.
///
/// **The ordinal is positional, not a `NodeId`.** It counts `Node::Sink`s in
/// node order, which is the same order [`Graph::sinks`] yields specs in and
/// therefore the same order the Router created writers in. A sink that produced
/// nothing this frame still consumes its ordinal, so a branch dropping a frame
/// can never shift its sibling's output onto the wrong writer — which would
/// write a vertical crop into the horizontal file and report nothing.
pub struct Run {
    // Read once the delegates hold one writer per sink instead of one writer
    // each. Until then both still append the original `CMSampleBuffer`, so the
    // default passthrough graph records byte-for-byte the file it always has —
    // the branching is proved by `run`'s own tests rather than by changing what
    // reaches disk.
    #[allow(dead_code)]
    pub appended: Vec<(usize, CFRetained<CVImageBuffer>)>,
    /// Whether any op returned [`Flow::Drop`] this frame. Only for counters;
    /// which branch dropped is already implicit in `appended`.
    pub dropped_any: bool,
}

impl Run {
    /// The result of a graph that did nothing — bypassed, unopened, or blown up
    /// mid-walk. Deliberately not an error: a delegate on a capture queue has
    /// nothing useful to do with one, and the house rule there is "return,
    /// never unwrap".
    pub fn nothing() -> Run {
        Run {
            appended: Vec::new(),
            dropped_any: false,
        }
    }
}

/// Everything an op is told at chapter open.
#[allow(dead_code)]
pub struct StreamCtx {
    pub stream: StreamId,
    pub chapter: u32,
    /// The clock this stream's buffers are timestamped against, used to
    /// normalize every PTS to host time at graph entry.
    pub clock: Retained<CMClock>,
    /// `Some` for the screen, `None` for the camera — and that asymmetry is
    /// real, not laziness. `screen_stream` sets `SCStreamConfiguration`'s pixel
    /// format and size explicitly, so the screen's dimensions are known before
    /// a frame arrives. Nothing in `capture::av` ever calls `setVideoSettings`
    /// on its `AVCaptureVideoDataOutput`, so the camera delivers the device's
    /// native format at the device's native size and neither is known until a
    /// buffer shows up. Encoding that honestly here is cheaper than discovering
    /// it in stage 2 as a pixel buffer pool built to the wrong size; stage 2
    /// pins the camera by reading `CVPixelBufferGetWidth`/`GetHeight`/
    /// `GetPixelFormatType` off a live warmed-up buffer.
    pub size: Option<(usize, usize)>,
    pub fps: Option<f64>,
    /// The session's Core Image context, for ops that draw.
    ///
    /// Cloning it is a retain, not a rebuild — see [`Renderer`]'s docs for why
    /// that distinction is worth a shared handle rather than a per-chapter
    /// construction. `None` on a machine with no Metal device, where a pixel op
    /// should refuse to open rather than silently pass frames through
    /// untransformed: a crop that quietly does nothing is a wrong-aspect file
    /// discovered in the edit.
    pub renderer: Option<Renderer>,
    /// Buffers to render into, vended by the sink's writer.
    ///
    /// `None` until a sink is attached, which is why an op that draws checks
    /// for it at `open` rather than discovering it missing on the first frame.
    pub pool: Option<Arc<Pool>>,
}

/// One stream's frozen op chain, for the length of one chapter.
pub struct Graph {
    stream: StreamId,
    nodes: Vec<Node>,
    sidecar: Sidecar,
    /// `None` until [`Graph::open`]. A graph that was never opened runs nothing
    /// rather than guessing at a clock — see [`Graph::run`].
    clock: Option<Retained<CMClock>>,
    counters: Counters,
    /// Latched when an op panics or errors. Per chapter, because the graph is
    /// per chapter: the next chapter gets a clean one.
    bypassed: bool,
}

impl Graph {
    /// Chapter open: reset everything a chapter's worth of running accumulated,
    /// then let every op allocate. Runs on the main thread from the Router,
    /// before the state is installed in the delegate.
    ///
    /// The `catch_unwind` here is the same guard as [`Graph::run`]'s, at the
    /// other end of the lifecycle: "a panicking op must never take the process
    /// down" has to hold for every op entry point, not just the hot one. A
    /// panic escaping `open` would unwind out of `Router::build_chapter` on the
    /// main thread, taking down a process that has an unfinalized chapter mp4
    /// open. Contained, it is exactly the case `build_chapter` already handles:
    /// a graph that refused to open, aborting the cut with the current chapter
    /// still intact.
    pub fn open(&mut self, ctx: &StreamCtx) -> Result<()> {
        self.clock = Some(ctx.clock.clone());
        self.counters = Counters::default();
        self.bypassed = false;
        self.sidecar.clear();

        let nodes = &mut self.nodes;
        let opened = catch_unwind(AssertUnwindSafe(move || -> Result<()> {
            for node in nodes.iter_mut() {
                if let Node::Op { op, .. } = node {
                    let name = op.name();
                    op.open(ctx)
                        .with_context(|| format!("the {name} op failed to open"))?;
                }
            }
            Ok(())
        }));

        match opened {
            Ok(result) => result,
            // The default panic hook has already printed the panic and its
            // location; `catch_unwind` cannot say which op it was.
            Err(_) => bail!(
                "{} graph: an op PANICKED while opening (see the panic printed above)",
                self.stream.as_str()
            ),
        }
    }

    /// One frame, on the capture queue. The hot path.
    ///
    /// Returns [`Flow`], never `Result`. The caller is an Objective-C callback
    /// with nothing useful to do about an error, and the house style there is
    /// "return, never unwrap"; errors and panics are dealt with here.
    ///
    /// ## Why `catch_unwind` is *inside* this function
    ///
    /// `define_class!` emits `extern "C-unwind"` with no `catch_unwind` of its
    /// own, so a panic in an op would unwind through AVFoundation and
    /// libdispatch off the top of a worker thread Rust never created: process
    /// abort, the Router never reaching `stop()`, and the in-progress chapter
    /// mp4 left unfinalized and unplayable.
    ///
    /// Catching it *here* rather than at each delegate's seam matters for a
    /// second reason. Both delegates hold their writer-state `Mutex` across
    /// this call, and the Router `.unwrap()`s that same mutex when it installs,
    /// takes, and finishes a chapter. A panic escaping `run` would poison it,
    /// so even a *recovered* panic would make `Router::stop()` panic on the
    /// main thread instead of finalizing the file — trading a lost op for a
    /// lost recording. Containing the unwind here means the guard is released
    /// normally, no mutex is ever poisoned, and the Router's existing
    /// `lock().unwrap()` calls stay valid exactly as written.
    ///
    /// This depends on the build not setting `panic = "abort"`; `Cargo.toml`'s
    /// `[profile.release]` sets only `opt-level = 3`. Adding `panic = "abort"`
    /// would silently turn every op panic back into a corrupted chapter.
    ///
    /// ## Two things the counters deliberately do not say
    ///
    /// The walk short-circuits the *whole remaining node list* on `Flow::Drop`,
    /// not just the dropping node's descendants. With a single linear chain —
    /// all stage 1 has — those are the same thing. Per-branch dropping needs
    /// per-branch buffers, which arrive with `Frame::replace` in stages 2 and
    /// 3; building it now would be untested machinery pretending to be a
    /// feature.
    ///
    /// And `dropped` counts *op* drops only, never frames the writer refused —
    /// see [`Counters::dropped`].
    pub fn run(&mut self, pixels: CFRetained<CVImageBuffer>, pts: CMTime) -> Run {
        if self.bypassed {
            return Run::nothing();
        }
        // Never opened: no clock, so no way to put this PTS on a timeline any
        // op could compare against. Run nothing rather than pass through a raw
        // capture time that looks like a host time.
        let Some(clock) = self.clock.as_ref() else {
            return Run::nothing();
        };
        let host_pts = frame::to_host_time(pts, clock);
        self.counters.seen += 1;

        // Disjoint field borrows: the walk needs `&mut nodes` and the frame
        // needs `&mut sidecar` at the same time.
        let Graph {
            nodes,
            sidecar,
            stream,
            ..
        } = self;
        let stream = *stream;

        let outcome = catch_unwind(AssertUnwindSafe(move || {
            // One buffer per node, not one frame threaded through a chain.
            // That is the whole difference between a chain and a graph: two
            // crops branching off one source each need the *source's* pixels,
            // so the second cannot be handed whatever the first produced.
            //
            // `nodes` is in topological order by construction — a `NodeId` can
            // only be passed to `op`/`sink`/`tap_into` after the node it names
            // exists — so one forward pass suffices and no node is ever read
            // before it is written.
            let mut outputs: Vec<Option<CFRetained<CVImageBuffer>>> = vec![None; nodes.len()];
            let mut appended: Vec<(usize, CFRetained<CVImageBuffer>)> = Vec::new();
            let mut sink_ordinal = 0usize;
            let mut dropped_any = false;

            for index in 0..nodes.len() {
                // Cloning a `CFRetained` is a retain, not a copy of the pixels.
                let input = nodes[index]
                    .inputs()
                    .first()
                    .and_then(|id| outputs[id.index].clone());

                match &mut nodes[index] {
                    Node::Source => outputs[index] = Some(pixels.clone()),
                    Node::Op { op, .. } => {
                        // An upstream drop takes this branch with it, and only
                        // this branch — a sibling reading the same source is
                        // unaffected.
                        let Some(input) = input else { continue };
                        let mut frame = Frame::new(input, host_pts, stream, sidecar);
                        match op.apply(&mut frame) {
                            Ok(Flow::Continue) => outputs[index] = Some(frame.into_pixels()),
                            Ok(Flow::Drop) => dropped_any = true,
                            Err(error) => return Err(format!("{} failed: {error:#}", op.name())),
                        }
                    }
                    Node::Sink { .. } => {
                        if let Some(input) = input {
                            appended.push((sink_ordinal, input));
                        }
                        // Counted whether or not it produced a frame, so the
                        // ordinal keeps naming the same writer every frame even
                        // when one branch drops.
                        sink_ordinal += 1;
                    }
                    Node::Preview { port, .. } => {
                        if let Some(input) = input {
                            port.publish(input.clone(), host_pts, stream);
                            outputs[index] = Some(input);
                        }
                    }
                    Node::Tap { tap, .. } => {
                        if let Some(input) = input {
                            let frame = Frame::new(input, host_pts, stream, sidecar);
                            tap.publish(&frame);
                        }
                    }
                }
            }
            Ok(Run {
                appended,
                dropped_any,
            })
        }));

        match outcome {
            Ok(Ok(run)) => {
                if run.dropped_any {
                    self.counters.dropped += 1;
                }
                return run;
            }
            Ok(Err(message)) => {
                self.bypass(&format!("{} graph: {message}", self.stream.as_str()));
                Run::nothing()
            }
            Err(_) => {
                // `catch_unwind` does not say which op it was; the default
                // panic hook has already printed the panic and its location on
                // the line above this one.
                self.bypass(&format!(
                    "{} graph: an op PANICKED (see the panic printed above)",
                    self.stream.as_str()
                ));
                Run::nothing()
            }
        }
    }

    /// Latch the bypass and say so once, loudly. Every later frame this chapter
    /// returns `Continue` without touching an op, so a broken op costs its
    /// data and nothing else.
    fn bypass(&mut self, what: &str) {
        self.bypassed = true;
        self.counters.bypassed = true;
        eprintln!(
            "stream-recorder: {what} — bypassing the graph for the rest of this chapter. \
             Recording continues; this chapter's op data is incomplete."
        );
    }

    /// Chapter close: hand the ops the graph's own numbers, let them report,
    /// and write the sidecars beside `beside`.
    ///
    /// `beside` is the media file this chapter's data describes — the *final*
    /// path, which for a discarded take is the one inside `.discarded/`, not
    /// the `chapter-NN.mp4` it no longer occupies. Runs on the main thread from
    /// the Router.
    ///
    /// Guarded by `catch_unwind` for the same reason [`Graph::run`] and
    /// [`Graph::open`] are, and this is the entry point where it matters most:
    /// close is where an op flushes, so it is where an op is most likely to
    /// panic, and it is called from the middle of `finish_chapter` and
    /// `discard_chapter` — both of which still have a writer to finalize, files
    /// to move into `.discarded/`, and audio to extract when it runs. A panic
    /// escaping here would skip all of that, leaving an mp4 that never got
    /// `finishWriting` (no moov atom, unplayable) or a discarded take's file
    /// still sitting on the path its replacement needs. Contained, it costs one
    /// chapter's op data and nothing else, which is precisely what the Router's
    /// `close_graph` reports and continues past.
    pub fn close(&mut self, beside: &Path) -> Result<Vec<std::path::PathBuf>> {
        self.sidecar.counters = self.counters;

        let Graph { nodes, sidecar, .. } = self;
        let closed = catch_unwind(AssertUnwindSafe(move || -> Result<()> {
            for node in nodes.iter_mut() {
                if let Node::Op { op, .. } = node {
                    let name = op.name();
                    op.close(sidecar)
                        .with_context(|| format!("the {name} op failed to close"))?;
                }
            }
            Ok(())
        }));

        // Written either way: whatever the ops that did flush cleanly recorded
        // is still this chapter's data, and one op blowing up is no reason to
        // throw the rest of it away.
        let written = self.sidecar.write_beside(beside);
        match closed {
            Ok(Ok(())) => written,
            Ok(Err(error)) => Err(error),
            Err(_) => bail!(
                "{} graph: an op PANICKED while closing (see the panic printed above)",
                self.stream.as_str()
            ),
        }
    }

    /// The output files this graph declares.
    ///
    /// Present and unused: stage 3 is where the Router stops hardcoding its
    /// camera/screen pair and asks the graph instead.
    #[allow(dead_code)]
    pub fn sinks(&self) -> impl Iterator<Item = &OutputSpec> {
        self.nodes.iter().filter_map(|node| match node {
            Node::Sink { spec, .. } => Some(spec),
            _ => None,
        })
    }

    /// Live view ports this graph declares, in node order.
    pub fn preview_ports(&self) -> Vec<Arc<PreviewPort>> {
        self.nodes
            .iter()
            .filter_map(|node| match node {
                Node::Preview { port, .. } => Some(Arc::clone(port)),
                _ => None,
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn stream(&self) -> StreamId {
        self.stream
    }

    /// The ops in this graph, in the order they run. For tests and log lines.
    #[allow(dead_code)]
    pub fn op_names(&self) -> Vec<&'static str> {
        self.nodes
            .iter()
            .filter_map(|node| match node {
                Node::Op { op, .. } => Some(op.name()),
                _ => None,
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn counters(&self) -> Counters {
        self.counters
    }
}

mod build;

pub(crate) use build::Node;
pub use build::{GraphBuilder, NodeId};

#[cfg(test)]
mod branch_tests;
#[cfg(test)]
pub(crate) mod tests;
