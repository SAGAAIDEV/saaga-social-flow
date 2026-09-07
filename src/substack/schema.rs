//! What a Substack notes run leaves on disk.
//!
//! Two files, and the second one is the point. `substack.json` is the structured
//! record, attributable to a prompt version like every other generated artifact.
//! `notes.md` is the same content as plain text, because the way this actually
//! gets used is open beside the Substack editor while the essay is typed by hand.
//!
//! Nothing here is distribution. There is no client, no ledger and no dedupe key,
//! because nothing sends it — the transport is a person at a keyboard, and the
//! only thing this stage owes them is something worth typing from.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SUBSTACK_JSON: &str = "substack.json";
pub const NOTES_MD: &str = "notes.md";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub heading: String,
    /// One line each. A beat names the thing to say and the detail that proves
    /// it; turning that into sentences is the writer's job, and a beat that
    /// already did it is one more thing to delete before typing can start.
    pub beats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    /// Where this lands in the longform, as `04:12`. Computed from the chapter
    /// durations rather than asked of the model, which has no way to know it —
    /// and would answer anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    /// Verbatim from the transcript. The one thing on this page that must not be
    /// retyped from memory.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Re-exported so `schema::Link` keeps meaning what it did when the type lived
/// here — it moved to [`crate::longform`] once the blog stage needed it too.
pub use crate::longform::Link;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubstackNotes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Which `substack.notes` preamble wrote this, carried for the same reason
    /// `PostsManifest` carries it: performance is attributable to a prompt
    /// version, never to a timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_hash: String,
    /// Options, not a choice. Picking the title is the writer's first decision
    /// and the model is not equipped to make it.
    #[serde(default)]
    pub titles: Vec<String>,
    #[serde(default)]
    pub subtitles: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub quotes: Vec<Quote>,
    #[serde(default)]
    pub close: Vec<String>,
    #[serde(default)]
    pub links: Vec<Link>,
}

impl SubstackNotes {
    /// Nothing to type from. A run that produced only bookkeeping is a failed
    /// run, not an empty page.
    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
            && self.hooks.is_empty()
            && self.sections.is_empty()
            && self.quotes.is_empty()
    }

    pub fn beat_count(&self) -> usize {
        self.sections.iter().map(|s| s.beats.len()).sum()
    }
}

pub fn save(dir: &Path, notes: &SubstackNotes) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let json_path = dir.join(SUBSTACK_JSON);
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(notes).context("serializing substack notes")? + "\n",
    )
    .with_context(|| format!("writing {}", json_path.display()))?;

    let md_path = dir.join(NOTES_MD);
    std::fs::write(&md_path, markdown(notes))
        .with_context(|| format!("writing {}", md_path.display()))?;
    Ok(json_path)
}

pub fn load(dir: &Path) -> Result<SubstackNotes> {
    let path = dir.join(SUBSTACK_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The file that sits beside the editor.
///
/// Headings are shallow and the beats are bullets, so it reads at a glance while
/// you are looking somewhere else — this is a page you glance at mid-sentence,
/// not one you read. The quotes carry their timestamp so a doubtful one can be
/// checked against the video in a few seconds rather than by scrubbing.
pub fn markdown(notes: &SubstackNotes) -> String {
    let mut out = String::from("# Substack notes\n\n");
    out.push_str("Beats to type from — none of this is finished prose, by design.\n");

    options(&mut out, "Title options", &notes.titles);
    options(&mut out, "Subtitle options", &notes.subtitles);
    options(&mut out, "Opening lines", &notes.hooks);

    if !notes.sections.is_empty() {
        out.push_str("\n## Sections\n");
        for (index, section) in notes.sections.iter().enumerate() {
            out.push_str(&format!("\n### {}. {}", index + 1, section.heading));
            if let Some(marker) = marker(section.chapter, section.timestamp.as_deref()) {
                out.push_str(&format!("  ({marker})"));
            }
            out.push('\n');
            for beat in &section.beats {
                out.push_str(&format!("- {beat}\n"));
            }
        }
    }

    if !notes.quotes.is_empty() {
        out.push_str("\n## Quotes — verbatim, do not paraphrase\n");
        for quote in &notes.quotes {
            out.push_str(&format!("\n> {}\n", quote.text));
            if let Some(marker) = marker(quote.chapter, quote.timestamp.as_deref()) {
                out.push_str(&format!("> — {marker}\n"));
            }
        }
    }

    options(&mut out, "Closing options", &notes.close);

    if !notes.links.is_empty() {
        out.push_str("\n## Links\n");
        for link in &notes.links {
            out.push_str(&format!("- {}: {}\n", link.label, link.url));
        }
    }
    out
}

/// A heading and its bullets, or nothing at all when there are none — an empty
/// heading reads as a finding of "none", which is not the same as having nothing
/// to say about it.
fn options(out: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {heading}\n"));
    for item in items {
        out.push_str(&format!("- {item}\n"));
    }
}

/// "chapter 2 · 04:12", or whichever half exists.
pub fn marker(chapter: Option<u32>, timestamp: Option<&str>) -> Option<String> {
    match (chapter, timestamp) {
        (Some(n), Some(at)) => Some(format!("chapter {n} · {at}")),
        (Some(n), None) => Some(format!("chapter {n}")),
        (None, Some(at)) => Some(at.to_string()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SubstackNotes {
        SubstackNotes {
            version: Some(2),
            prompt_version: Some(1),
            prompt_hash: "feedfacefeedface".into(),
            titles: vec!["The retry that cost us a week".into()],
            subtitles: vec!["What a silent backoff hid".into()],
            hooks: vec!["The queue drained at 3am and nobody noticed.".into()],
            sections: vec![Section {
                heading: "Where it broke".into(),
                beats: vec!["the retry had no ceiling".into(), "12k duplicate rows".into()],
                chapter: Some(2),
                timestamp: Some("04:12".into()),
            }],
            quotes: vec![Quote {
                text: "I assumed the ledger would catch it. It could not.".into(),
                chapter: Some(2),
                timestamp: Some("05:01".into()),
            }],
            close: vec!["Check what your retries cannot see.".into()],
            links: vec![Link {
                label: "Watch".into(),
                url: "https://youtube.com/watch?v=abc".into(),
            }],
        }
    }

    #[test]
    fn notes_round_trip() {
        let dir = std::env::temp_dir().join(format!("substack-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = save(&dir, &sample()).expect("save");
        assert!(path.exists());
        assert!(dir.join(NOTES_MD).exists(), "the file you type from is written too");
        assert_eq!(load(&dir).expect("load"), sample());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_markdown_is_typeable() {
        let md = markdown(&sample());
        assert!(md.contains("## Title options"));
        assert!(md.contains("- The retry that cost us a week"));
        assert!(md.contains("### 1. Where it broke  (chapter 2 · 04:12)"));
        assert!(md.contains("- the retry had no ceiling"));
        assert!(md.contains("> I assumed the ledger would catch it."));
        assert!(md.contains("> — chapter 2 · 05:01"));
        assert!(md.contains("- Watch: https://youtube.com/watch?v=abc"));
    }

    /// An empty heading reads as "we looked and there were none", which is a
    /// different claim from having nothing in that category to offer.
    #[test]
    fn empty_groups_are_omitted_rather_than_shown_empty() {
        let md = markdown(&SubstackNotes {
            titles: vec!["Only a title".into()],
            ..SubstackNotes::default()
        });
        assert!(md.contains("## Title options"));
        assert!(!md.contains("## Subtitle options"));
        assert!(!md.contains("## Sections"));
        assert!(!md.contains("## Quotes"));
        assert!(!md.contains("## Links"));
    }

    #[test]
    fn a_marker_uses_whichever_half_it_has() {
        assert_eq!(marker(Some(3), Some("01:02")).as_deref(), Some("chapter 3 · 01:02"));
        assert_eq!(marker(Some(3), None).as_deref(), Some("chapter 3"));
        assert_eq!(marker(None, Some("01:02")).as_deref(), Some("01:02"));
        assert_eq!(marker(None, None), None);
    }

    /// Bookkeeping alone is a failed run. The pane has to be able to say so
    /// rather than showing an empty page that looks like a finished one.
    #[test]
    fn notes_with_nothing_to_type_read_as_empty() {
        let bare = SubstackNotes {
            version: Some(1),
            prompt_hash: "abc".into(),
            links: vec![Link { label: "Watch".into(), url: "u".into() }],
            ..SubstackNotes::default()
        };
        assert!(bare.is_empty(), "links alone are not something to type from");
        assert!(!sample().is_empty());
        assert_eq!(sample().beat_count(), 2);
    }
}
