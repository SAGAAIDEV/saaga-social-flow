//! Tests for the graph being a *graph* rather than a chain: one source
//! feeding several branches, each reaching its own sink.
//!
//! Split from `tests.rs` because they exercise a different property. Those
//! cover one frame's journey through a chain — order, drops, panics; these
//! cover what a chain cannot express at all, which is the shape recording
//! both orientations from one capture takes.

use std::sync::atomic::Ordering;

use anyhow::Result;

use super::tests::*;
use super::*;
use crate::ops::sink::OutputSpec;
use crate::ops::{Frame, VideoOp};
/// An op that drops every frame, for testing that a drop stays on its branch.
struct DroppingOp;

impl VideoOp for DroppingOp {
    fn name(&self) -> &'static str {
        "dropping"
    }
    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        Ok(Flow::Drop)
    }
}

fn opened(builder: GraphBuilder) -> Graph {
    let mut graph = builder.build().expect("well formed");
    graph.open(&host_ctx(StreamId::Screen)).expect("opens");
    graph
}

/// The property the whole DAG walk exists for: one source, two branches, two
/// sinks — and *both* get a frame. A linear chain could not express this at
/// all, and this is exactly the shape recording both orientations from one
/// screen capture takes.
#[test]
fn one_source_branching_into_two_sinks_feeds_both() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let (left, left_count) = CountingOp::new("left");
    let (right, right_count) = CountingOp::new("right");
    let left = builder.op(source, left);
    let right = builder.op(source, right);
    builder.sink(left, OutputSpec::new("horizontal", 1402, 1080));
    builder.sink(right, OutputSpec::new("vertical", 1080, 1280));

    let run = opened(builder).run(a_frame(), time(1, 1000));

    assert_eq!(left_count.load(Ordering::Relaxed), 1, "left branch ran");
    assert_eq!(right_count.load(Ordering::Relaxed), 1, "right branch ran");
    assert_eq!(run.appended.len(), 2, "both sinks received a frame");
    assert_eq!(run.appended[0].0, 0, "first sink keeps ordinal 0");
    assert_eq!(run.appended[1].0, 1, "second sink keeps ordinal 1");
}

/// Each branch reads the **source**, not whatever its sibling produced. On a
/// linear chain the second crop would be handed the first crop's output and
/// would crop a crop — a picture of the wrong region that no dimension check
/// would catch.
#[test]
fn a_branch_reads_the_source_not_its_sibling() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let first = builder.op(source, DroppingOp);
    let (second, second_count) = CountingOp::new("second");
    let second = builder.op(source, second);
    builder.sink(first, OutputSpec::new("dropped", 100, 100));
    builder.sink(second, OutputSpec::new("kept", 100, 100));

    let run = opened(builder).run(a_frame(), time(1, 1000));

    assert_eq!(
        second_count.load(Ordering::Relaxed),
        1,
        "the surviving branch must still see the source frame",
    );
    assert_eq!(run.appended.len(), 1, "only the surviving branch appended");
    assert_eq!(
        run.appended[0].0, 1,
        "and it kept ordinal 1 — a dropped branch must not shift its sibling \
         onto the wrong writer",
    );
    assert!(run.dropped_any);
}

/// A drop short-circuits its own branch and nothing else: the op downstream of
/// the dropper never runs.
#[test]
fn a_drop_short_circuits_only_its_own_branch() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let dropper = builder.op(source, DroppingOp);
    let (downstream, downstream_count) = CountingOp::new("downstream");
    let downstream = builder.op(dropper, downstream);
    builder.sink(downstream, OutputSpec::new("after", 100, 100));

    let run = opened(builder).run(a_frame(), time(1, 1000));

    assert_eq!(
        downstream_count.load(Ordering::Relaxed),
        0,
        "an op downstream of a drop must not run",
    );
    assert!(run.appended.is_empty());
}

/// A graph with no sinks still runs its ops — that is every analyzer preset,
/// including the shipped default.
#[test]
fn a_sinkless_graph_still_runs_its_ops() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    let (counter, count) = CountingOp::new("analyzer");
    builder.op(source, counter);

    let run = opened(builder).run(a_frame(), time(1, 1000));

    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert!(run.appended.is_empty(), "nothing to append without a sink");
}

/// A sink hanging straight off the source needs no op at all — the passthrough
/// case, and what a chapter records before any transform is configured.
#[test]
fn a_sink_on_the_source_gets_the_delivered_frame() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    builder.sink(source, OutputSpec::new("", 1920, 1080));

    let run = opened(builder).run(a_frame(), time(1, 1000));
    assert_eq!(run.appended.len(), 1);
    assert_eq!(run.appended[0].0, 0);
}

/// The ordinals `run` reports must index the same list `sinks()` yields, since
/// the Router builds one writer per spec in that order and the delegate pairs
/// them up positionally.
#[test]
fn run_ordinals_line_up_with_the_sink_specs() {
    let mut builder = GraphBuilder::new(StreamId::Screen);
    let source = builder.source();
    builder.sink(source, OutputSpec::new("first", 100, 100));
    builder.sink(source, OutputSpec::new("second", 200, 200));
    let graph = opened(builder);

    let names: Vec<&str> = graph.sinks().map(|spec| spec.name()).collect();
    assert_eq!(names, vec!["first", "second"]);

    let mut graph = graph;
    let run = graph.run(a_frame(), time(1, 1000));
    let ordinals: Vec<usize> = run.appended.iter().map(|(o, _)| *o).collect();
    assert_eq!(ordinals, vec![0, 1]);
}

#[test]
fn duplicate_preview_names_are_rejected_at_build() {
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    builder.preview(source, crate::ops::PreviewSpec::new("horizontal", 1920, 1080));
    builder.preview(source, crate::ops::PreviewSpec::new("horizontal", 1080, 1920));
    let Err(error) = builder.build() else {
        panic!("two preview sinks must not share a name");
    };
    assert!(
        error.to_string().contains("horizontal"),
        "the error should name the colliding preview: {error}"
    );
}

#[test]
fn preview_sinks_publish_their_input() {
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    let port = builder.preview(source, crate::ops::PreviewSpec::new("horizontal", 16, 16));
    let mut graph = opened(builder);
    assert!(port.latest().is_none());
    graph.run(a_frame(), time(1, 1000));
    let latest = port.latest().expect("the preview sink published");
    assert_eq!(latest.size(), (16, 16));
}

#[test]
fn a_replacing_op_on_one_preview_branch_does_not_change_the_other() {
    struct StampOp {
        pixels: objc2_core_foundation::CFRetained<objc2_core_video::CVImageBuffer>,
    }
    impl VideoOp for StampOp {
        fn name(&self) -> &'static str {
            "stamp"
        }
        fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
            frame.replace(self.pixels.clone());
            Ok(Flow::Continue)
        }
    }

    let stamped = crate::ops::frame::test_support::pixel_buffer(
        32,
        32,
        objc2_core_video::kCVPixelFormatType_32BGRA,
        64,
    );
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    let stamped_node = builder.op(source, StampOp { pixels: stamped });
    let replaced = builder.preview(
        stamped_node,
        crate::ops::PreviewSpec::new("horizontal", 32, 32),
    );
    let original = builder.preview(source, crate::ops::PreviewSpec::new("vertical", 16, 16));
    let mut graph = opened(builder);
    graph.run(a_frame(), time(1, 1000));
    assert_eq!(replaced.latest().expect("h").size(), (32, 32));
    assert_eq!(original.latest().expect("v").size(), (16, 16));
}
