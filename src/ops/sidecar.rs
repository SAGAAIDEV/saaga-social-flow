//! Where an op's findings go when a chapter closes: one small JSON file per op,
//! written next to the media it describes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Graph-level truth about one chapter, copied into the sidecar before the ops
/// get their turn to write so any op can report it alongside its own numbers
/// without every op threading a counter of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Frames handed to the graph.
    pub seen: u64,
    /// Frames an op asked to drop.
    ///
    /// Deliberately **not** the same number as frames the writer refused. The
    /// graph runs *before* the `isReadyForMoreMediaData` check on both paths,
    /// so ops burn their budget on frames a stalled writer then discards;
    /// conflating the two would make this counter lie about which side lost the
    /// frame.
    pub dropped: u64,
    /// Whether the graph latched its bypass after an op panicked or errored.
    /// If this is true, the numbers above stopped meaning much partway through.
    pub bypassed: bool,
}

/// The per-chapter collection point for op output.
///
/// One file per op rather than one combined file, so a downstream consumer can
/// read the op it cares about without knowing anything about the graph that
/// produced it — the shape `screencast`'s edit stage already expects of
/// `face_track.json`, which is what stage 5 will emit through this exact path.
pub struct Sidecar {
    entries: Vec<(&'static str, serde_json::Value)>,
    pub counters: Counters,
}

impl Sidecar {
    pub fn new() -> Sidecar {
        Sidecar {
            entries: Vec::new(),
            counters: Counters::default(),
        }
    }

    /// Reset for a new chapter. Called by `Graph::open`, not by ops.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.counters = Counters::default();
    }

    /// Record one op's output. A later record under the same name replaces the
    /// earlier one, so an op that reports at close overwrites anything it wrote
    /// mid-chapter instead of producing two files that disagree.
    pub fn record(&mut self, op: &'static str, value: serde_json::Value) {
        match self.entries.iter_mut().find(|(name, _)| *name == op) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((op, value)),
        }
    }

    #[allow(dead_code)] // for ops that want to read back what they recorded.
    pub fn get(&self, op: &str) -> Option<&serde_json::Value> {
        self.entries
            .iter()
            .find(|(name, _)| *name == op)
            .map(|(_, value)| value)
    }

    #[allow(dead_code)] // for a caller that wants to know before writing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Write one `<stem>.<op>.json` beside `media`, returning what was written.
    ///
    /// `chapter-01.mp4` yields `chapter-01.stats.json` and
    /// `chapter-01-screen.mp4` yields `chapter-01-screen.stats.json`, so the
    /// camera and screen graphs of one chapter cannot collide even though they
    /// run the same ops.
    ///
    /// The destination is supplied here rather than captured at open because
    /// `Router::discard_chapter` moves a retaken take's files into
    /// `.discarded/` under a fresh timestamped name. Passing the destination at
    /// close is what puts a discarded take's analysis data beside its discarded
    /// media instead of orphaning it next to a path that no longer exists —
    /// `.discarded/` is documented as a *recoverable* location, and a take
    /// recovered without its data is not.
    ///
    /// **An op that records nothing produces no file.** That is what keeps
    /// `graphs::default_graph` byte-for-byte invisible: passthrough records
    /// nothing, so a default session's directory listing is unchanged.
    pub fn write_beside(&self, media: &Path) -> Result<Vec<PathBuf>> {
        if self.entries.is_empty() {
            return Ok(Vec::new());
        }
        let dir = media.parent().unwrap_or_else(|| Path::new("."));
        let stem = media
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "chapter".to_string());

        let mut written = Vec::with_capacity(self.entries.len());
        for (op, value) in &self.entries {
            let path = dir.join(format!("{stem}.{op}.json"));
            let json = serde_json::to_string_pretty(value)
                .with_context(|| format!("serializing the {op} sidecar"))?;
            std::fs::write(&path, json)
                .with_context(|| format!("writing {}", path.display()))?;
            written.push(path);
        }
        Ok(written)
    }
}

impl Default for Sidecar {
    fn default() -> Self {
        Sidecar::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-sidecar-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn an_op_that_records_nothing_writes_no_file() {
        let dir = temp_dir("empty");
        let sidecar = Sidecar::new();
        let written = sidecar
            .write_beside(&dir.join("chapter-01.mp4"))
            .expect("write");
        assert!(written.is_empty(), "a silent graph wrote {written:?}");
        assert!(
            !dir.join("chapter-01.stats.json").exists(),
            "a silent graph left a file behind"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_sidecar_lands_beside_the_media_file_named_for_its_op() {
        let dir = temp_dir("named");

        let mut camera = Sidecar::new();
        camera.record("stats", serde_json::json!({ "frames": 3 }));
        let written = camera
            .write_beside(&dir.join("chapter-01.mp4"))
            .expect("write");
        assert_eq!(written, vec![dir.join("chapter-01.stats.json")]);

        // The screen half of the same chapter must not overwrite it.
        let mut screen = Sidecar::new();
        screen.record("stats", serde_json::json!({ "frames": 5 }));
        let written = screen
            .write_beside(&dir.join("chapter-01-screen.mp4"))
            .expect("write");
        assert_eq!(written, vec![dir.join("chapter-01-screen.stats.json")]);

        let camera_json = std::fs::read_to_string(dir.join("chapter-01.stats.json")).unwrap();
        assert!(camera_json.contains("\"frames\": 3"), "{camera_json}");
        let screen_json =
            std::fs::read_to_string(dir.join("chapter-01-screen.stats.json")).unwrap();
        assert!(screen_json.contains("\"frames\": 5"), "{screen_json}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recording_twice_under_one_name_replaces_rather_than_duplicates() {
        let mut sidecar = Sidecar::new();
        sidecar.record("stats", serde_json::json!({ "frames": 1 }));
        sidecar.record("stats", serde_json::json!({ "frames": 2 }));
        assert_eq!(
            sidecar.get("stats"),
            Some(&serde_json::json!({ "frames": 2 }))
        );
    }
}
