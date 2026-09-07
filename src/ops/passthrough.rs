//! The op that does nothing, on purpose.

use anyhow::Result;

use super::graph::Flow;
use super::{Frame, VideoOp};

/// Passes every frame through untouched and records nothing.
///
/// Two jobs. It proves the graph's plumbing — open, run, close, sidecar — end
/// to end without changing a recording, and it is what
/// [`graphs::default_graph`](super::graphs::default_graph) contains, so
/// shipping the whole ops module is provably a no-op rather than merely
/// believed to be one. `graphs::the_default_graph_is_passthrough_only` asserts
/// that, and because passthrough records nothing the sidecar writes no file, so
/// a default session's directory listing is unchanged too.
pub struct Passthrough;

impl Passthrough {
    pub fn new() -> Passthrough {
        Passthrough
    }
}

impl Default for Passthrough {
    fn default() -> Self {
        Passthrough::new()
    }
}

impl VideoOp for Passthrough {
    fn name(&self) -> &'static str {
        "passthrough"
    }

    fn apply(&mut self, _frame: &mut Frame) -> Result<Flow> {
        Ok(Flow::Continue)
    }
}
