//! What the Substack tab shows, decided in Rust.
//!
//! Markers ("chapter 2 · 04:12") are composed here rather than in the template,
//! so the pane and `notes.md` cannot drift into labelling the same beat two
//! different ways — the one thing on the page whose whole job is to be checkable
//! against the video.

use std::path::Path;

use serde::Serialize;

use super::schema::{self, Link};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pane {
    /// Why there is nothing to read, when there is nothing.
    pub blocked: Option<String>,
    pub can_generate: bool,
    pub prompt: PromptView,
    pub titles: Vec<String>,
    pub subtitles: Vec<String>,
    pub hooks: Vec<String>,
    pub sections: Vec<SectionView>,
    pub quotes: Vec<QuoteView>,
    pub close: Vec<String>,
    pub links: Vec<Link>,
    /// The whole of `notes.md`, carried so Copy All hands over exactly the file
    /// that is on disk rather than a second rendering of it.
    pub markdown: String,
    pub markdown_path: Option<String>,
    /// "5 sections · 18 beats · 3 quotes", or nothing before the first run.
    pub summary: Option<String>,
}

/// The preamble in effect, and the file to change it in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PromptView {
    /// "v0 (builtin)", "v2", "unversioned (a1b2c3d4)".
    pub label: String,
    /// Where an edit would land — the standing library copy, whether or not it
    /// exists yet, because "the file to edit" is the useful thing to show and it
    /// is created on demand.
    pub path: String,
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SectionView {
    pub index: usize,
    pub heading: String,
    pub beats: Vec<String>,
    pub marker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuoteView {
    pub text: String,
    pub marker: Option<String>,
}

/// `dir` is the version's substack folder, `root` the project — the prompt is
/// resolved against the project, the notes are read from the version.
pub fn build(root: &Path, dir: &Path, can_generate: bool, blocked: Option<String>) -> Pane {
    let prompt = prompt_view(root, crate::agent::prompt::library_dir().as_deref());
    let Ok(notes) = schema::load(dir) else {
        return Pane {
            blocked: blocked.or_else(|| Some("No notes yet — press Generate Notes.".into())),
            can_generate,
            prompt,
            titles: Vec::new(),
            subtitles: Vec::new(),
            hooks: Vec::new(),
            sections: Vec::new(),
            quotes: Vec::new(),
            close: Vec::new(),
            links: Vec::new(),
            markdown: String::new(),
            markdown_path: None,
            summary: None,
        };
    };
    let summary = format!(
        "{} section(s) · {} beat(s) · {} quote(s)",
        notes.sections.len(),
        notes.beat_count(),
        notes.quotes.len()
    );
    Pane {
        blocked,
        can_generate,
        prompt,
        markdown: schema::markdown(&notes),
        markdown_path: Some(dir.join(schema::NOTES_MD).display().to_string()),
        summary: Some(summary),
        sections: notes
            .sections
            .iter()
            .enumerate()
            .map(|(index, section)| SectionView {
                index: index + 1,
                heading: section.heading.clone(),
                beats: section.beats.clone(),
                marker: schema::marker(section.chapter, section.timestamp.as_deref()),
            })
            .collect(),
        quotes: notes
            .quotes
            .iter()
            .map(|quote| QuoteView {
                text: quote.text.clone(),
                marker: schema::marker(quote.chapter, quote.timestamp.as_deref()),
            })
            .collect(),
        titles: notes.titles,
        subtitles: notes.subtitles,
        hooks: notes.hooks,
        close: notes.close,
        links: notes.links,
    }
}

/// `library` is where a standing house prompt would live, passed in rather than
/// read from `$HOME` so this is testable on a machine that has one.
fn prompt_view(root: &Path, library: Option<&Path>) -> PromptView {
    let prompt_id = crate::agent::prompt::SUBSTACK;
    let live = crate::agent::prompt::live_in(prompt_id, Some(root), library);
    let builtin = live.as_ref().is_none_or(|resolved| resolved.is_builtin());
    PromptView {
        label: match live {
            Some(resolved) if resolved.is_builtin() => "v0 (builtin)".to_string(),
            Some(resolved) => resolved.label(),
            None => "v0 (builtin)".to_string(),
        },
        path: library
            .map(|dir| dir.join(format!("{prompt_id}.txt")).display().to_string())
            .unwrap_or_default(),
        builtin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::substack::schema::{Quote, Section, SubstackNotes};

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-substack-pane-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn written(dir: &Path) {
        schema::save(
            dir,
            &SubstackNotes {
                titles: vec!["A title".into()],
                hooks: vec!["A hook".into()],
                sections: vec![Section {
                    heading: "Where it broke".into(),
                    beats: vec!["no ceiling".into(), "12k rows".into()],
                    chapter: Some(2),
                    timestamp: Some("04:12".into()),
                }],
                quotes: vec![Quote {
                    text: "It could not.".into(),
                    chapter: Some(2),
                    timestamp: None,
                }],
                ..SubstackNotes::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn a_project_with_no_run_invites_the_first_one() {
        let dir = temp("empty");
        let pane = build(&dir, &dir, true, None);
        assert!(pane.blocked.unwrap().contains("press Generate Notes"));
        assert!(pane.sections.is_empty());
        assert_eq!(pane.summary, None);
        // No Copy All button before there is anything to copy: a button that
        // puts an empty string on the clipboard is worse than no button.
        assert!(pane.markdown.is_empty());
    }

    /// A real blocking reason outranks the invitation: "record a chapter first"
    /// is more useful than "press the button you cannot press".
    #[test]
    fn a_gate_reason_wins_over_the_invitation() {
        let dir = temp("gated");
        let pane = build(&dir, &dir, false, Some("No chapters recorded yet".into()));
        assert_eq!(pane.blocked.as_deref(), Some("No chapters recorded yet"));
        assert!(!pane.can_generate);
    }

    #[test]
    fn a_written_run_renders_its_sections_beats_and_markers() {
        let dir = temp("written");
        written(&dir);
        let pane = build(&dir, &dir, true, None);
        assert_eq!(pane.blocked, None);
        assert_eq!(pane.sections.len(), 1);
        assert_eq!(pane.sections[0].index, 1, "numbered for the reader, not from zero");
        assert_eq!(pane.sections[0].marker.as_deref(), Some("chapter 2 · 04:12"));
        assert_eq!(pane.quotes[0].marker.as_deref(), Some("chapter 2"));
        assert_eq!(
            pane.summary.as_deref(),
            Some("1 section(s) · 2 beat(s) · 1 quote(s)")
        );
        // Copy All hands over `markdown` verbatim, so it has to *be* the file —
        // a second rendering could quietly differ from what the reader opened.
        assert_eq!(
            pane.markdown,
            std::fs::read_to_string(dir.join(schema::NOTES_MD)).unwrap()
        );
        assert!(pane.markdown.contains("Where it broke"));
        assert!(pane.markdown_path.unwrap().ends_with("notes.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pane names the file an edit lands in even before that file exists —
    /// "where to change this" is the useful thing to show, and it is created on
    /// demand by the button beside it.
    #[test]
    fn the_prompt_view_names_the_library_file_while_it_is_still_the_builtin() {
        let dir = temp("prompt");
        std::fs::create_dir_all(&dir).unwrap();
        let view = prompt_view(&dir, Some(&dir.join("library")));
        assert!(view.builtin);
        assert_eq!(view.label, "v0 (builtin)");
        assert!(view.path.ends_with("substack.notes.txt"), "{}", view.path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
