//! What a graph's output files are called, and how big they are.
//!
//! Types only in stage 1. No `AVAssetWriter` is created from an [`OutputSpec`]
//! yet — `router.rs` still builds its hardcoded camera/screen pair exactly as
//! before. Stage 3 replaces that pair with `graph.sinks()` and turns
//! create/finish/metadata-fix/discard into loops, at which point one screen
//! chain can write both the full-res capture and a 1080p proxy, and one camera
//! chain can write the landscape master, a cropped vertical, and a
//! camera-over-screen composite, from the same frames in one pass.

use std::path::{Path, PathBuf};

/// One output file a graph produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    name: String,
    width: usize,
    height: usize,
    audio: bool,
}

// Nothing constructs an OutputSpec outside the tests until stage 3 replaces the
// Router's hardcoded camera/screen pair with `graph.sinks()`. The naming rule
// below is the part that has to be settled first, because it is what keeps the
// camera master addressable by the name every downstream tool already uses.
impl OutputSpec {
    /// `name` is the filename suffix. The **empty** name is the camera master
    /// and is not decoration — see [`OutputSpec::file_name`].
    pub fn new(name: &str, width: usize, height: usize) -> OutputSpec {
        OutputSpec {
            name: name.to_string(),
            width,
            height,
            audio: false,
        }
    }

    /// Declare that this file carries the microphone.
    ///
    /// Audio is fanned out, never graphed: audio buffers do not enter a graph
    /// at all, and `av_delegate` appends each one directly to every sink whose
    /// spec sets this. That keeps the mic path structurally identical to
    /// today's — the one part of this codebase with a documented history of
    /// silent corruption — while still letting a composite or vertical file
    /// carry sound instead of shipping as silent video nobody can use
    /// standalone.
    pub fn with_audio(mut self) -> OutputSpec {
        self.audio = true;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn audio(&self) -> bool {
        self.audio
    }

    /// `""` → `chapter-01.mp4`; `"pip"` → `chapter-01-pip.mp4`.
    ///
    /// The empty name yielding the *bare* `chapter-NN.mp4` is backward
    /// compatibility, pinned here because this is the one piece of sink
    /// behaviour that can be tested before a writer exists.
    /// `transcode::fix_mp4_metadata`, `transcode::extract_audio_copy`,
    /// `markers`, and the downstream `screencast/` package all address the
    /// camera master by that exact name, so it must keep it when stage 3 turns
    /// the Router's hardcoded pair into `graph.sinks()`.
    pub fn file_name(&self, chapter: u32) -> String {
        if self.name.is_empty() {
            format!("chapter-{chapter:02}.mp4")
        } else {
            format!("chapter-{chapter:02}-{}.mp4", self.name)
        }
    }

    pub fn path_in(&self, dir: &Path, chapter: u32) -> PathBuf {
        dir.join(self.file_name(chapter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::graph::GraphBuilder;
    use crate::ops::passthrough::Passthrough;
    use crate::ops::StreamId;

    #[test]
    fn an_unnamed_sink_keeps_the_bare_chapter_name() {
        let spec = OutputSpec::new("", 1920, 1080);
        assert_eq!(spec.file_name(1), "chapter-01.mp4");
        assert_eq!(spec.file_name(10), "chapter-10.mp4");
        assert_eq!(
            spec.path_in(Path::new("/tmp/session"), 2),
            PathBuf::from("/tmp/session/chapter-02.mp4")
        );
    }

    #[test]
    fn a_named_sink_is_suffixed() {
        let spec = OutputSpec::new("pip", 1920, 1080);
        assert_eq!(spec.file_name(1), "chapter-01-pip.mp4");
        assert_eq!(spec.file_name(10), "chapter-10-pip.mp4");
        assert_eq!(
            OutputSpec::new("screen", 3456, 2234).file_name(9),
            "chapter-09-screen.mp4"
        );
    }

    #[test]
    fn audio_is_opt_in_per_sink() {
        assert!(!OutputSpec::new("pip", 1920, 1080).audio());
        assert!(OutputSpec::new("pip", 1920, 1080).with_audio().audio());
    }

    #[test]
    fn duplicate_sink_names_are_rejected_at_build() {
        let mut builder = GraphBuilder::new(StreamId::Camera);
        let source = builder.source();
        let op = builder.op(source, Passthrough::new());
        builder.sink(op, OutputSpec::new("pip", 1920, 1080));
        builder.sink(op, OutputSpec::new("pip", 1280, 720));

        let Err(error) = builder.build() else {
            panic!("two writers must not race for one file path");
        };
        assert!(
            error.to_string().contains("pip"),
            "the error should name the colliding sink: {error}"
        );
    }
}
