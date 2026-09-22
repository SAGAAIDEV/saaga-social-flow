//! `outline/vN/outline.json`: the talking points of every outline chapter.
//!
//! Two kinds of field live here and they age differently. `text` and `anchor`
//! are what the model wrote and what a person edits — a manifest already on
//! disk keeps them, so an edit survives a re-render. `at` is derived: the
//! anchor placed against the current cut, rewritten on every render because a
//! hand edit to the keep-list moves every word after it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const OUTLINE_JSON: &str = "outline.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutlinePoint {
    pub text: String,
    /// The transcript words where the speaker begins this point, verbatim.
    /// What the placement matches; empty when a person typed the point in
    /// without one, which places it after the point before.
    #[serde(default)]
    pub anchor: String,
    /// Seconds into the *cut* chapter at which the point appears. Derived —
    /// see the module docs — and `None` until it has been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterOutline {
    pub n: u32,
    #[serde(default)]
    pub points: Vec<OutlinePoint>,
    /// Set by a person who has read the points. Nothing gates on it yet; it is
    /// carried so a review pane has somewhere to put its tick, the way titles do.
    #[serde(default)]
    pub approved: bool,
    /// Where the face sits in the vertical master, as a fraction of its height,
    /// so the bottom band the card pushes the camera into is centred on it.
    /// Read from the tracker's sidecar once, then kept — so a value corrected
    /// here by hand stands. `None` means the composition's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_y: Option<f64>,
}

impl ChapterOutline {
    /// The JSON the composition parses: text and time only, in order, and
    /// only points that have been placed. A point without a time would have
    /// no moment to appear at, so it is left out rather than shown at zero.
    pub fn variable_json(&self) -> String {
        let points: Vec<serde_json::Value> = self
            .points
            .iter()
            .filter_map(|point| {
                let at = point.at?;
                Some(serde_json::json!({ "text": point.text, "at": at }))
            })
            .collect();
        serde_json::json!({ "points": points }).to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OutlineManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default)]
    pub chapters: Vec<ChapterOutline>,
}

impl OutlineManifest {
    pub fn chapter(&self, n: u32) -> Option<&ChapterOutline> {
        self.chapters.iter().find(|chapter| chapter.n == n)
    }

    /// Replaces the entry for the chapter or appends it, keeping the list in
    /// chapter order.
    pub fn put(&mut self, outline: ChapterOutline) {
        match self.chapters.iter_mut().find(|c| c.n == outline.n) {
            Some(existing) => *existing = outline,
            None => {
                self.chapters.push(outline);
                self.chapters.sort_by_key(|c| c.n);
            }
        }
    }
}

pub fn save(dir: &Path, manifest: &OutlineManifest) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(OUTLINE_JSON);
    let text = serde_json::to_string_pretty(manifest).context("serializing outline")? + "\n";
    // Only when it differs: the render's freshness checks are mtime-based, and
    // this file is what the placed times come from.
    if std::fs::read_to_string(&path).ok().as_deref() != Some(&text) {
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

pub fn load(dir: &Path) -> Result<OutlineManifest> {
    let path = dir.join(OUTLINE_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(text: &str, at: Option<f64>) -> OutlinePoint {
        OutlinePoint {
            text: text.into(),
            anchor: String::new(),
            at,
        }
    }

    #[test]
    fn the_manifest_round_trips_and_keeps_chapter_order() {
        let dir = std::env::temp_dir().join(format!("outline-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut manifest = OutlineManifest {
            version: Some(2),
            chapters: Vec::new(),
        };
        manifest.put(ChapterOutline {
            n: 3,
            points: vec![point("Later", Some(4.5))],
            approved: false,
            face_y: None,
        });
        manifest.put(ChapterOutline {
            n: 1,
            points: vec![point("First", None)],
            approved: true,
            face_y: None,
        });
        save(&dir, &manifest).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(
            back.chapters.iter().map(|c| c.n).collect::<Vec<_>>(),
            [1, 3]
        );
        assert!(back.chapter(1).unwrap().approved);
        // Replacing keeps one entry per chapter.
        let mut again = back;
        again.put(ChapterOutline {
            n: 3,
            points: Vec::new(),
            approved: false,
            face_y: None,
        });
        assert_eq!(again.chapters.len(), 2);
        assert!(again.chapter(3).unwrap().points.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the block reads: placed points only, text and time, no anchors.
    #[test]
    fn the_variable_json_carries_placed_points_only() {
        let outline = ChapterOutline {
            n: 1,
            points: vec![
                point("Shown", Some(1.25)),
                point("Never placed", None),
                point("Also shown", Some(9.0)),
            ],
            approved: false,
            face_y: None,
        };
        let json: serde_json::Value = serde_json::from_str(&outline.variable_json()).unwrap();
        let points = json["points"].as_array().unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0]["text"], "Shown");
        assert_eq!(points[0]["at"], 1.25);
        assert_eq!(points[1]["text"], "Also shown");
        assert!(points[0].get("anchor").is_none());
    }

    /// A manifest from before a field existed still loads.
    #[test]
    fn an_older_manifest_still_loads() {
        let older: OutlineManifest =
            serde_json::from_str(r#"{"chapters":[{"n":2,"points":[{"text":"Hi"}]}]}"#).unwrap();
        assert_eq!(older.chapter(2).unwrap().points[0].at, None);
        assert_eq!(older.chapter(2).unwrap().points[0].anchor, "");
    }
}
