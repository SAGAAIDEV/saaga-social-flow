//! Assembling a graph: nodes, the ids that name them, and the builder that
//! keeps them in a runnable order.
//!
//! Split from the running half in [`super`] because the two answer different
//! questions. That file is the hot path — what happens to one frame, thirty
//! times a second, on a capture queue. This one runs once per chapter and its
//! whole job is to make the hot path's assumptions true before it starts.
//!
//! The assumption in question: **`nodes` is in topological order**. It holds by
//! construction rather than by sorting — a [`NodeId`] can only be handed to
//! `op`/`sink`/`tap_into` after the node it names exists, so an input always
//! precedes its consumer — which is what lets `Graph::run` resolve the whole
//! DAG in one forward pass with no bookkeeping.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};

use super::{Graph, StreamId};
use crate::ops::preview::{PreviewPort, PreviewSpec};
use crate::ops::sidecar::{Counters, Sidecar};
use crate::ops::sink::OutputSpec;
use crate::ops::tap::Tap;
use crate::ops::VideoOp;

/// A handle on a node in one specific builder's graph.
///
/// Tagged with the builder that issued it, not a bare index. Mixing a `NodeId`
/// obtained from a different builder is the one real way to break the
/// topological invariant below, and an untagged index would make that
/// undetectable whenever the two graphs happened to be the same size — which,
/// for two graphs built from the same preset function, is always.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId {
    graph: u64,
    pub(crate) index: usize,
}

pub(crate) enum Node {
    Source,
    Op {
        op: Box<dyn VideoOp>,
        inputs: Vec<NodeId>,
    },
    Sink {
        spec: OutputSpec,
        input: NodeId,
    },
    Preview {
        port: Arc<PreviewPort>,
        input: NodeId,
    },
    Tap {
        /// Held so the tap outlives the builder that made it; `run` skips this
        /// node until stage 4 wires the publish.
        #[allow(dead_code)]
        tap: Arc<Tap>,
        input: NodeId,
    },
}

impl Node {
    pub(crate) fn inputs(&self) -> &[NodeId] {
        match self {
            Node::Source => &[],
            Node::Op { inputs, .. } => inputs,
            Node::Sink { input, .. } | Node::Preview { input, .. } | Node::Tap { input, .. } => {
                std::slice::from_ref(input)
            }
        }
    }
}

/// Issues the builder tags that make a foreign [`NodeId`] detectable. Process
/// lifetime, monotonic, never reused.
static NEXT_GRAPH_ID: AtomicU64 = AtomicU64::new(1);

/// Assembles a graph. See `graphs.rs` for the presets built with it.
pub struct GraphBuilder {
    id: u64,
    stream: StreamId,
    nodes: Vec<Node>,
}

impl GraphBuilder {
    pub fn new(stream: StreamId) -> GraphBuilder {
        GraphBuilder {
            id: NEXT_GRAPH_ID.fetch_add(1, Ordering::Relaxed),
            stream,
            nodes: vec![Node::Source],
        }
    }

    /// The stream's frames, entering the graph. Always node 0.
    pub fn source(&self) -> NodeId {
        NodeId {
            graph: self.id,
            index: 0,
        }
    }

    pub fn op(&mut self, input: NodeId, op: impl VideoOp + 'static) -> NodeId {
        self.push(Node::Op {
            op: Box::new(op),
            inputs: vec![input],
        })
    }

    /// A multi-input op — the shape stage 4's camera-over-screen composite
    /// needs, where the secondary input is a tap read rather than a frame that
    /// arrived on this queue.
    #[allow(dead_code)] // stage 4.
    pub fn join(&mut self, inputs: Vec<NodeId>, op: impl VideoOp + 'static) -> NodeId {
        self.push(Node::Op {
            op: Box::new(op),
            inputs,
        })
    }

    pub fn sink(&mut self, input: NodeId, spec: OutputSpec) -> NodeId {
        self.push(Node::Sink { spec, input })
    }

    /// A live view of `input`. The UI binds to the returned port; nothing is
    /// written to disk.
    pub fn preview(&mut self, input: NodeId, spec: PreviewSpec) -> Arc<PreviewPort> {
        let port = PreviewPort::new(spec);
        self.push(Node::Preview {
            port: Arc::clone(&port),
            input,
        });
        port
    }

    #[allow(dead_code)] // stage 4.
    pub fn tap(&mut self, input: NodeId) -> Arc<Tap> {
        let tap = Tap::new();
        self.push(Node::Tap {
            tap: Arc::clone(&tap),
            input,
        });
        tap
    }

    fn push(&mut self, node: Node) -> NodeId {
        let index = self.nodes.len();
        self.nodes.push(node);
        NodeId {
            graph: self.id,
            index,
        }
    }

    /// Verify the graph and freeze it.
    ///
    /// Topological order is an *invariant of construction* rather than the
    /// result of a sorting pass: a [`NodeId`] can only be handed to
    /// `op`/`join`/`sink`/`tap` after the node it names has been pushed, so
    /// inputs always precede their consumer. This still verifies it rather than
    /// trusting it, for two reasons — there is one real way to break it (mixing
    /// a `NodeId` from a different builder, which the tag catches), and a cycle
    /// in a video graph is a deadlock rather than an error message, so it has
    /// to be impossible rather than unlikely.
    ///
    /// Duplicate sink names are rejected here too: two sinks with one name are
    /// two writers racing for one file path, and `AVAssetWriter` refuses to
    /// `startWriting` over an existing file with an error (-11823 "Cannot
    /// Save") that names neither writer.
    pub fn build(self) -> Result<Graph> {
        for (index, node) in self.nodes.iter().enumerate() {
            for input in node.inputs() {
                if input.graph != self.id {
                    bail!(
                        "node {index} was wired to a NodeId from a different graph — \
                         a builder's ids are only meaningful to that builder"
                    );
                }
                if input.index >= index {
                    bail!(
                        "node {index} consumes node {}, which does not precede it — \
                         the graph is cyclic or out of order",
                        input.index
                    );
                }
            }
        }

        let mut names: Vec<&str> = Vec::new();
        let mut preview_names: Vec<&str> = Vec::new();
        for node in &self.nodes {
            match node {
                Node::Sink { spec, .. } => {
                    if names.contains(&spec.name()) {
                        bail!(
                            "two sinks are both named {:?} — they would write the same file",
                            spec.name()
                        );
                    }
                    names.push(spec.name());
                }
                Node::Preview { port, .. } => {
                    let name = port.spec().name();
                    if preview_names.contains(&name) {
                        bail!(
                            "two preview sinks are both named {name:?} — the UI could not tell them apart"
                        );
                    }
                    preview_names.push(name);
                }
                _ => {}
            }
        }

        Ok(Graph {
            stream: self.stream,
            nodes: self.nodes,
            sidecar: Sidecar::new(),
            clock: None,
            counters: Counters::default(),
            bypassed: false,
        })
    }
}
