//! Unit tests for the op graph.
//!
//! Split from `mod.rs` so the graph's own code reads as one piece; they are
//! a `#[cfg(test)] mod tests;` there and behave exactly as an inline module.
use super::*;
use crate::ops::frame::test_support::pixel_buffer;
use crate::ops::passthrough::Passthrough;
use crate::ops::sink::OutputSpec;
use crate::ops::{Frame, VideoOp};
use anyhow::Result;
use objc2_core_media::CMTimeFlags;
use objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Counts how many frames reached it.
pub(crate) struct CountingOp {
    pub(crate) name: &'static str,
    pub(crate) count: Arc<AtomicUsize>,
}

impl CountingOp {
    pub(crate) fn new(name: &'static str) -> (CountingOp, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        (
            CountingOp {
                name,
                count: Arc::clone(&count),
            },
            count,
        )
    }
}

impl VideoOp for CountingOp {
    fn name(&self) -> &'static str {
        self.name
    }
    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(Flow::Continue)
    }
}

struct DropOp;
impl VideoOp for DropOp {
    fn name(&self) -> &'static str {
        "drop"
    }
    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        Ok(Flow::Drop)
    }
}

struct PanicOp {
    entered: Arc<AtomicUsize>,
}
impl VideoOp for PanicOp {
    fn name(&self) -> &'static str {
        "panic"
    }
    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        self.entered.fetch_add(1, Ordering::Relaxed);
        panic!("an op did something a video op does: indexed off the end of a plane");
    }
}

struct ErrorOp {
    entered: Arc<AtomicUsize>,
}
impl VideoOp for ErrorOp {
    fn name(&self) -> &'static str {
        "error"
    }
    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        self.entered.fetch_add(1, Ordering::Relaxed);
        bail!("the buffer was not a format this op understands")
    }
}

/// Records the PTS of every frame it sees, so a test can assert the graph
/// normalized it.
struct PtsOp {
    seen: Arc<std::sync::Mutex<Vec<CMTime>>>,
}
impl VideoOp for PtsOp {
    fn name(&self) -> &'static str {
        "pts"
    }
    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        self.seen.lock().unwrap().push(frame.pts());
        Ok(Flow::Continue)
    }
}

pub(crate) fn host_ctx(stream: StreamId) -> StreamCtx {
    StreamCtx {
        stream,
        chapter: 1,
        clock: unsafe { CMClock::host_time_clock() }.into(),
        size: None,
        fps: None,
        renderer: None,
        pool: None,
    }
}

pub(crate) fn a_frame() -> CFRetained<CVImageBuffer> {
    pixel_buffer(16, 16, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, 64)
}

pub(crate) fn time(value: i64, timescale: i32) -> CMTime {
    CMTime {
        value,
        timescale,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

#[test]
fn every_node_follows_its_inputs() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let first = builder.op(source, Passthrough::new());
    let second = builder.op(first, Passthrough::new());
    builder.sink(second, OutputSpec::new("", 1920, 1080));
    let tap = builder.tap(second);

    let graph = builder.build().expect("a linear chain is well formed");
    for (index, node) in graph.nodes.iter().enumerate() {
        for input in node.inputs() {
            assert!(
                input.index < index,
                "node {index} consumes node {} which does not precede it",
                input.index
            );
        }
    }
    assert!(tap.latest().is_none());
}

#[test]
fn a_node_id_from_another_graph_is_rejected() {
    let mut other = GraphBuilder::new(StreamId::Camera);
    let other_source = other.source();
    let foreign = other.op(other_source, Passthrough::new());

    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let _ = builder.op(source, Passthrough::new());
    // Index 1 exists in this builder too, so only the tag can catch it.
    builder.op(foreign, Passthrough::new());

    let Err(error) = builder.build() else {
        panic!("a foreign NodeId must not build");
    };
    assert!(
        error.to_string().contains("different graph"),
        "unexpected error: {error}"
    );
}

#[test]
fn continue_runs_every_op_in_order() {
    let (first_op, first) = CountingOp::new("first");
    let (second_op, second) = CountingOp::new("second");

    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let a = builder.op(source, first_op);
    builder.op(a, second_op);
    let mut graph = builder.build().expect("build");
    graph.open(&host_ctx(StreamId::Screen)).expect("open");

    assert!(!graph.run(a_frame(), time(1, 1000)).dropped_any);
    assert_eq!(first.load(Ordering::Relaxed), 1);
    assert_eq!(second.load(Ordering::Relaxed), 1);
    assert_eq!(graph.counters().seen, 1);
    assert_eq!(graph.counters().dropped, 0);
}

#[test]
fn drop_short_circuits_the_rest_of_the_chain() {
    let (upstream_op, upstream) = CountingOp::new("upstream");
    let (downstream_op, downstream) = CountingOp::new("downstream");

    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let a = builder.op(source, upstream_op);
    let b = builder.op(a, DropOp);
    builder.op(b, downstream_op);
    let mut graph = builder.build().expect("build");
    graph.open(&host_ctx(StreamId::Screen)).expect("open");

    assert!(graph.run(a_frame(), time(1, 1000)).dropped_any);
    assert_eq!(
        upstream.load(Ordering::Relaxed),
        1,
        "upstream must have run"
    );
    assert_eq!(
        downstream.load(Ordering::Relaxed),
        0,
        "a dropped frame must not reach the rest of the chain"
    );
    assert_eq!(graph.counters().dropped, 1);
}

#[test]
fn an_op_that_panics_bypasses_the_graph_instead_of_unwinding() {
    let entered = Arc::new(AtomicUsize::new(0));
    let (downstream_op, downstream) = CountingOp::new("downstream");

    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    let a = builder.op(
        source,
        PanicOp {
            entered: Arc::clone(&entered),
        },
    );
    builder.op(a, downstream_op);
    let mut graph = builder.build().expect("build");
    graph.open(&host_ctx(StreamId::Camera)).expect("open");

    // The panic message below is expected test output: it is the default
    // panic hook printing before catch_unwind swallows the unwind.
    assert!(
        !graph.run(a_frame(), time(1, 1000)).dropped_any,
        "a panicking op must not be reported as an op drop"
    );
    assert_eq!(entered.load(Ordering::Relaxed), 1);
    assert!(graph.counters().bypassed, "the bypass must latch");
    assert_eq!(downstream.load(Ordering::Relaxed), 0);

    assert!(!graph.run(a_frame(), time(2, 1000)).dropped_any);
    assert_eq!(
        entered.load(Ordering::Relaxed),
        1,
        "a bypassed graph must not re-enter the op"
    );
}

#[test]
fn an_op_that_errors_bypasses_the_graph() {
    let entered = Arc::new(AtomicUsize::new(0));

    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    builder.op(
        source,
        ErrorOp {
            entered: Arc::clone(&entered),
        },
    );
    let mut graph = builder.build().expect("build");
    graph.open(&host_ctx(StreamId::Camera)).expect("open");

    assert!(!graph.run(a_frame(), time(1, 1000)).dropped_any);
    assert!(graph.counters().bypassed);
    graph.run(a_frame(), time(2, 1000));
    assert_eq!(
        entered.load(Ordering::Relaxed),
        1,
        "a bypassed graph must not re-enter the op"
    );
}

#[test]
fn a_graph_that_was_never_opened_runs_nothing() {
    let (op, count) = CountingOp::new("counting");
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    builder.op(source, op);
    let mut graph = builder.build().expect("build");

    assert!(!graph.run(a_frame(), time(1, 1000)).dropped_any);
    assert_eq!(count.load(Ordering::Relaxed), 0);
}

#[test]
fn a_frame_carries_the_normalized_pts() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    builder.op(
        source,
        PtsOp {
            seen: Arc::clone(&seen),
        },
    );
    let mut graph = builder.build().expect("build");
    let ctx = host_ctx(StreamId::Screen);
    let clock = ctx.clock.clone();
    graph.open(&ctx).expect("open");

    let raw = time(987_654, 1_000);
    graph.run(a_frame(), raw);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let expected = frame::to_host_time(raw, &clock);
    assert!(
        (crate::timesync::seconds(seen[0]) - crate::timesync::seconds(expected)).abs() < 1e-6,
        "the op saw {} but the host-clock time is {}",
        crate::timesync::seconds(seen[0]),
        crate::timesync::seconds(expected),
    );
}

/// The guard the whole watchdog exists for, at the two lifecycle ends the
/// hot path does not cover. `close` runs from the middle of
/// `Router::finish_chapter`, with a screen writer still to finalize; `open`
/// runs from `build_chapter`. An unwind out of either lands on the main
/// thread and kills a process holding an unfinalized mp4, so both must
/// come back as an ordinary `Err`.
#[test]
fn an_op_that_panics_at_the_chapter_boundary_errors_instead_of_unwinding() {
    struct PanicAt(&'static str);
    impl VideoOp for PanicAt {
        fn name(&self) -> &'static str {
            "panic-at"
        }
        fn open(&mut self, _ctx: &StreamCtx) -> Result<()> {
            if self.0 == "open" {
                panic!("an op panicked while allocating for a chapter");
            }
            Ok(())
        }
        fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
            Ok(Flow::Continue)
        }
        fn close(&mut self, _sidecar: &mut Sidecar) -> Result<()> {
            panic!("an op panicked while flushing at chapter close");
        }
    }

    // The panic messages below are expected test output: the default panic
    // hook prints before catch_unwind swallows the unwind.
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    builder.op(source, PanicAt("open"));
    let mut graph = builder.build().expect("build");
    let error = graph
        .open(&host_ctx(StreamId::Camera))
        .expect_err("a panicking open must surface as an error");
    assert!(error.to_string().contains("PANICKED"), "{error}");

    // A neighbouring op's data survives the one that blows up.
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    let a = builder.op(source, Recording);
    builder.op(a, PanicAt("close"));
    let mut graph = builder.build().expect("build");
    graph.open(&host_ctx(StreamId::Camera)).expect("open");
    graph.run(a_frame(), time(1, 1000));

    let dir = std::env::temp_dir().join(format!(
        "stream-recorder-graph-panic-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let beside = dir.join("chapter-01.mp4");
    let error = graph
        .close(&beside)
        .expect_err("a panicking close must surface as an error");
    assert!(error.to_string().contains("PANICKED"), "{error}");
    assert!(
        dir.join("chapter-01.recording.json").exists(),
        "the ops that did flush must still get their sidecar"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Records one line at close, so a test can prove its data survived a
/// neighbouring op's panic.
struct Recording;
impl VideoOp for Recording {
    fn name(&self) -> &'static str {
        "recording"
    }
    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        Ok(Flow::Continue)
    }
    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        let seen = sidecar.counters.seen;
        sidecar.record("recording", serde_json::json!({ "seen": seen }));
        Ok(())
    }
}

#[test]
fn close_hands_the_ops_the_graphs_own_counters() {
    struct ReportingOp;
    impl VideoOp for ReportingOp {
        fn name(&self) -> &'static str {
            "reporting"
        }
        fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
            Ok(Flow::Continue)
        }
        fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
            let seen = sidecar.counters.seen;
            sidecar.record("reporting", serde_json::json!({ "seen": seen }));
            Ok(())
        }
    }

    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    builder.op(source, ReportingOp);
    let mut graph = builder.build().expect("build");
    graph.open(&host_ctx(StreamId::Screen)).expect("open");
    graph.run(a_frame(), time(1, 1000));
    graph.run(a_frame(), time(2, 1000));

    let dir = std::env::temp_dir().join(format!(
        "stream-recorder-graph-close-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let written = graph.close(&dir.join("chapter-01.mp4")).expect("close");
    assert_eq!(written, vec![dir.join("chapter-01.reporting.json")]);
    let json = std::fs::read_to_string(&written[0]).unwrap();
    assert!(json.contains("\"seen\": 2"), "{json}");
    std::fs::remove_dir_all(&dir).ok();
}
