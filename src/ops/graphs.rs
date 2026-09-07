//! Where graphs are defined: plain Rust constructors, one function per preset.
//!
//! No config schema, no string→constructor registry, no dynamically loaded
//! plugins. A graph is code because a graph *is* code — the wiring is typed,
//! the compiler checks it, and a preset that does not build is a compile error
//! rather than a runtime surprise on the first frame of a shoot. (Plugins are
//! rejected on top of that for a harder reason: Rust has no stable dylib ABI,
//! and this is a single binary.)
//!
//! Adding an op is one file in `src/ops/` plus one line here. Adding an output
//! file, from stage 3 on, is one `sink` line.
//!
//! Both presets `.expect()` on `build()`. A graph defined in Rust in this file
//! can only fail to build if this file is wrong, which is a programmer error at
//! startup, not a runtime condition worth propagating through the Router.

use std::sync::Arc;

use super::composite::Composite;
use super::crop::Cover;
use super::face_track::FaceTrack;
use super::mouse_track::MouseTrack;
use super::graph::{Graph, GraphBuilder};
use super::passthrough::Passthrough;
use super::preview::PreviewSpec;
use super::sink::OutputSpec;
use super::stats::Stats;
use super::tap::Tap;
use super::StreamId;
use crate::config::FaceTracking;
use crate::face::FaceTracker;
use crate::layouts::{Layout, Orientation, Pair};
use crate::pointer::PointerTracker;
use crate::region::framing::Framing;
use crate::region::{DisplayGeometry, PointRect};

/// What every recording runs: source → passthrough.
///
/// Passthrough-only on purpose, so "stage 1 changes nothing" is a property of
/// the shipped default and not a claim about it — see
/// `the_default_graph_is_passthrough_only` below, and `Passthrough`'s docs for
/// why an op that records nothing also leaves the session directory unchanged.
pub fn default_graph(stream: StreamId) -> Graph {
    let mut builder = GraphBuilder::new(stream);
    let source = builder.source();
    builder.op(source, Passthrough::new());
    builder
        .build()
        .expect("default_graph is a two-node chain and cannot fail to build")
}

/// The bare camera, before any layout.
///
/// A third preview port hanging off the source node, so a still can be taken of
/// the camera *only*. The horizontal port is a composite in any layout with a
/// screen slot, which would put a screen share in the thumbnail — the one place
/// the composited view is exactly wrong.
pub const CAMERA_PREVIEW: &str = "camera";

/// How the camera is aimed in a preview graph: nothing, or a tracker plus the
/// per-layout punch-in it needs to have room to move.
///
/// A pair rather than two arguments because they are meaningless apart. A
/// tracker with no zoom cannot move the full-bleed longform; a zoom with no
/// tracker is a tighter but still fixed crop. `None` is "framing exactly as it
/// was before face tracking existed", and it is what a session gets until the
/// operator asks for otherwise and the detector has finished loading.
pub struct Tracking {
    pub tracker: Arc<FaceTracker>,
    pub config: FaceTracking,
}

impl Tracking {
    /// The framing for one layout: the tracker's shared cell, and this block's
    /// own zoom.
    fn framing(&self, layout: &Layout) -> Framing {
        Framing::tracked(
            self.config.zoom_for(layout.block),
            Some(self.tracker.anchor_cell()),
        )
    }
}

/// How the vertical screen crop follows the pointer: the tracker, and the
/// frame its readings are measured against.
///
/// The three travel together because a reading is meaningless without the
/// other two — an anchor is a fraction *of a region*, on *a display*. The
/// tracker outlives the graph and carries the smoothing across rebuilds; the
/// geometry and the region are re-read on every rebuild and are only ever
/// current. Splitting them into separate arguments would let a caller pair
/// this session's tracker with last session's region, which is exactly the
/// bug the parenting in `region::placement` exists to prevent elsewhere.
pub struct Pointing {
    pub tracker: Arc<PointerTracker>,
    pub geometry: DisplayGeometry,
    /// What the screen stream is capturing, display-local points. The space
    /// the published anchor is normalized to, and therefore the one thing both
    /// crops can agree on.
    pub capture: PointRect,
}

/// Live H and V views of the current pair.
///
/// Talking Head covers the camera into each canvas. Split composites camera
/// and the latest screen tap into each layout. Session-scoped: the UI binds
/// to the two preview ports, and [`CAMERA_PREVIEW`] carries the raw source.
///
/// With `tracking` set, one [`FaceTrack`] node sits between the source and the
/// fork, so both orientations are framed from a single detection — see that
/// op's docs for why it is a node rather than a step inside each composite.
pub fn preview_graph(
    pair: Pair,
    screen: Option<Arc<Tap>>,
    crops: [Option<(f64, f64, f64, f64)>; 2],
    tracking: Option<&Tracking>,
    pointing: Option<&Pointing>,
) -> Graph {
    let mut builder = GraphBuilder::new(StreamId::Camera);
    let source = builder.source();
    // Straight off the source: no cover, no composite, no screen.
    let camera = Layout::get(Pair::TalkingHead, Orientation::Horizontal).canvas;
    let camera = crate::region::PixelSize::rounded(camera.0, camera.1);
    builder.preview(
        source,
        PreviewSpec::new(CAMERA_PREVIEW, camera.w, camera.h),
    );
    // The camera port hangs off the *source*, above this, so a thumbnail still
    // is of the untracked, unpunched camera — the one view where the framing
    // decision should not have been applied yet.
    let camera_in = match tracking {
        Some(tracking) => builder.op(source, FaceTrack::new(Arc::clone(&tracking.tracker))),
        None => source,
    };
    // Only Split has a screen crop for it to move, so on Talking Head the node
    // is left out entirely rather than left in reading a pointer nothing
    // consumes. Same shape as `tracking` gating `FaceTrack` above.
    let pointing = pointing.filter(|_| pair == Pair::Split);
    let camera_in = match pointing {
        Some(pointing) => builder.op(
            camera_in,
            MouseTrack::new(
                Arc::clone(&pointing.tracker),
                pointing.geometry,
                pointing.capture,
            ),
        ),
        None => camera_in,
    };
    for (index, orientation) in Orientation::ALL.iter().enumerate() {
        let layout = Layout::get(pair, *orientation);
        let name = match orientation {
            Orientation::Horizontal => "horizontal",
            Orientation::Vertical => "vertical",
        };
        let op_name = match (layout.screen_slot, orientation) {
            (Some(_), Orientation::Horizontal) => "composite-horizontal",
            (Some(_), Orientation::Vertical) => "composite-vertical",
            (None, Orientation::Horizontal) => "cover-horizontal",
            (None, Orientation::Vertical) => "cover-vertical",
        };
        let framing = tracking
            .map(|tracking| tracking.framing(layout))
            .unwrap_or_default();
        let node = if layout.screen_slot.is_some() {
            builder.op(
                camera_in,
                Composite::for_layout(
                    op_name,
                    layout,
                    screen.clone(),
                    crops[index],
                    // Both orientations, from the one reading. They punch in
                    // on the same point through different slots, which is
                    // exactly what an anchor in the capture's own space is
                    // for — see `region::framing::Track`.
                    pointing.map(|p| p.tracker.cell()),
                    framing,
                ),
            )
        } else {
            builder.op(camera_in, Cover::for_canvas(op_name, layout.canvas, framing))
        };
        let out = crate::region::PixelSize::rounded(layout.canvas.0, layout.canvas.1);
        builder.preview(node, PreviewSpec::new(name, out.w, out.h));
        builder.sink(node, OutputSpec::new(name, out.w, out.h).with_audio());
    }
    builder
        .build()
        .expect("preview_graph is a two-branch fork and cannot fail to build")
}

/// File sinks the preview graph declares: horizontal then vertical, both with audio.
pub fn preview_sinks(pair: Pair) -> Vec<OutputSpec> {
    Orientation::ALL
        .iter()
        .map(|orientation| {
            let layout = Layout::get(pair, *orientation);
            let out = crate::region::PixelSize::rounded(layout.canvas.0, layout.canvas.1);
            let name = match orientation {
                Orientation::Horizontal => "horizontal",
                Orientation::Vertical => "vertical",
            };
            OutputSpec::new(name, out.w, out.h).with_audio()
        })
        .collect()
}

/// Source → passthrough → stats: the A/B against [`default_graph`].
///
/// Not the default. [`Stats`] walks the whole Y plane on the capture queue; its
/// own docs carry the cost. Nothing selects this yet — it exists so the
/// pixel-reading path is exercised by tests and can be switched on by hand
/// (a `--graph <name>` flag is the obvious next step, and deliberately not
/// taken in stage 1).
#[allow(dead_code)]
pub fn stats_graph(stream: StreamId) -> Graph {
    let mut builder = GraphBuilder::new(stream);
    let source = builder.source();
    let passed = builder.op(source, Passthrough::new());
    builder.op(passed, Stats::new());
    builder
        .build()
        .expect("stats_graph is a three-node chain and cannot fail to build")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_graph_is_passthrough_only() {
        for stream in [StreamId::Camera, StreamId::Screen] {
            let graph = default_graph(stream);
            assert_eq!(
                graph.op_names(),
                vec!["passthrough"],
                "the default graph for {} grew an op — every recording now pays for it",
                stream.as_str()
            );
            assert_eq!(
                graph.sinks().count(),
                0,
                "the default graph declares no sinks until stage 3"
            );
        }
    }

    /// A 2× display with the vertical region framed on it, which is all the
    /// pointer node needs to be constructed — no window server, no pointer.
    fn pointing_fixture() -> Pointing {
        Pointing {
            tracker: Arc::new(PointerTracker::new(
                &crate::config::MouseTracking::default(),
            )),
            geometry: DisplayGeometry {
                cg_origin: (0.0, 0.0),
                points: (1728.0, 1117.0),
                pixels: (3456, 2234),
                primary_height_points: 1117.0,
            },
            // The union both Split regions are captured through.
            capture: PointRect {
                x: 100.0,
                y: 100.0,
                w: 860.0,
                h: 900.0,
            },
        }
    }

    /// The pointer node leads both composites, for the same reason the face
    /// tracker does: one reading per frame, published before anything crops
    /// with it.
    ///
    /// And it is absent on Talking Head. There is no screen crop there for it
    /// to move, so a node that sampled the pointer would be pure cost — the
    /// same gating `tracking` gets, checked here because "it does nothing"
    /// is invisible in the output and would never be noticed.
    #[test]
    fn pointing_adds_one_node_and_only_where_there_is_a_screen_crop() {
        let pointing = pointing_fixture();

        let untracked = preview_graph(Pair::Split, None, [None, None], None, None);
        let tracked = preview_graph(Pair::Split, None, [None, None], None, Some(&pointing));
        let mut expected = vec!["mouse-track"];
        expected.extend(untracked.op_names());
        assert_eq!(
            tracked.op_names(),
            expected,
            "on Split the pointer node must lead, with the composites unchanged \
             behind it",
        );
        assert_eq!(
            tracked.sinks().count(),
            untracked.sinks().count(),
            "tracking changed the files, not just the framing",
        );

        let plain = preview_graph(Pair::TalkingHead, None, [None, None], None, None);
        let pointed = preview_graph(Pair::TalkingHead, None, [None, None], None, Some(&pointing));
        assert_eq!(
            pointed.op_names(),
            plain.op_names(),
            "Talking Head has no screen crop to move, so it must not pay for a \
             pointer node",
        );
    }

    #[test]
    fn the_stats_graph_analyzes_after_passing_through() {
        let graph = stats_graph(StreamId::Screen);
        assert_eq!(graph.op_names(), vec!["passthrough", "stats"]);
    }

    #[test]
    fn the_camera_port_is_present_in_every_pair() {
        for pair in [Pair::TalkingHead, Pair::Split] {
            let graph = preview_graph(pair, None, [None, None], None, None);
            let names: Vec<String> = graph
                .preview_ports()
                .iter()
                .map(|port| port.spec().name().to_string())
                .collect();
            assert!(
                names.contains(&CAMERA_PREVIEW.to_string()),
                "{pair:?} has a raw camera port: {names:?}"
            );
            // The composited views are still there — the camera port is additional.
            assert!(names.contains(&"horizontal".to_string()));
            assert!(names.contains(&"vertical".to_string()));
        }
    }

    #[test]
    fn the_preview_graph_has_horizontal_and_vertical_sinks() {
        let graph = preview_graph(Pair::TalkingHead, None, [None, None], None, None);
        assert_eq!(
            graph.op_names(),
            vec!["cover-horizontal", "cover-vertical"]
        );
        let names: Vec<String> = graph
            .preview_ports()
            .iter()
            .map(|port| port.spec().name().to_string())
            .collect();
        // The raw camera port leads, then the two composited views.
        assert_eq!(names, vec![CAMERA_PREVIEW, "horizontal", "vertical"]);
        assert_eq!(graph.sinks().count(), 2);
        let sink_names: Vec<&str> = graph.sinks().map(|s| s.name()).collect();
        assert_eq!(sink_names, vec!["horizontal", "vertical"]);
    }

    /// Tracking adds exactly one node, and it sits **before** the fork. If it
    /// ever lands after, each orientation gets its own detector: twice the cost
    /// for one camera, and two outputs that can disagree about where the
    /// subject is in the same frame.
    #[test]
    fn tracking_adds_one_node_ahead_of_both_orientations() {
        for pair in [Pair::TalkingHead, Pair::Split] {
            let untracked = preview_graph(pair, None, [None, None], None, None);
            let tracking = Tracking {
                tracker: std::sync::Arc::new(
                    match crate::face::FaceTracker::build(
                        crate::config::FaceTracking::default(),
                        match crate::ops::Renderer::new() {
                            Ok(renderer) => renderer,
                            Err(_) => {
                                println!("skipping: no Metal device on this machine");
                                return;
                            }
                        },
                    ) {
                        Ok(tracker) => tracker,
                        Err(e) => {
                            println!("skipping: no MediaPipe runtime available ({e:#})");
                            return;
                        }
                    },
                ),
                config: crate::config::FaceTracking::default(),
            };
            let tracked = preview_graph(pair, None, [None, None], Some(&tracking), None);

            let mut expected = vec!["face-track"];
            expected.extend(untracked.op_names());
            assert_eq!(
                tracked.op_names(),
                expected,
                "{pair:?}: the tracker must lead, with the composites unchanged behind it"
            );
            // Same outputs either way — tracking changes framing, not files.
            assert_eq!(
                tracked.sinks().count(),
                untracked.sinks().count(),
                "{pair:?} grew or lost a sink"
            );
            assert_eq!(
                tracked.preview_ports().len(),
                untracked.preview_ports().len(),
                "{pair:?} grew or lost a preview port"
            );
        }
    }

    /// Every layout gets a framing, and only the one that needs a punch-in gets
    /// one. This is the shipped default, so it is worth pinning against a
    /// well-meaning edit that applies the zoom everywhere.
    #[test]
    fn only_the_full_bleed_longform_ships_with_a_punch_in() {
        let config = crate::config::FaceTracking::default();
        assert!(
            config.zoom_for("talking-head-horizontal") > 1.0,
            "the one layout with no travel of its own must punch in"
        );
        for block in [
            "talking-head-vertical",
            "screen-camera-split",
            "screen-camera-vertical",
        ] {
            assert_eq!(
                config.zoom_for(block),
                1.0,
                "{block} already crops enough to track without losing resolution"
            );
        }
        // A layout nobody configured frames as it always has.
        assert_eq!(config.zoom_for("some-future-block"), 1.0);
    }

    #[test]
    fn the_split_preview_graph_composites() {
        let graph = preview_graph(Pair::Split, None, [None, None], None, None);
        assert_eq!(
            graph.op_names(),
            vec!["composite-horizontal", "composite-vertical"]
        );
        let names: Vec<String> = graph
            .preview_ports()
            .iter()
            .map(|port| port.spec().name().to_string())
            .collect();
        // The raw camera port leads, then the two composited views.
        assert_eq!(names, vec![CAMERA_PREVIEW, "horizontal", "vertical"]);
        assert_eq!(graph.sinks().count(), 2);
    }
}
