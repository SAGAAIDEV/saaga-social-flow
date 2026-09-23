//! Shorts: standalone vertical takes recorded beside a video, not inside it.
//!
//! A short is an aside — something worth its own 30–60 seconds on a phone —
//! and it must not end up in the longform, the notes, the blog or the chapter
//! clips. So it is not a chapter. It is its own project, nested in the one it
//! was recorded from:
//!
//! ```text
//! {root}/shorts/suggestions.json   what the suggest pass proposed
//! {root}/shorts/short-01/          a project like any other — drafts, notes,
//! {root}/shorts/short-02/          edit, render, titles, YouTube ledger
//! ```
//!
//! Laid out that way, recording, retake, the cut, the render and the upload
//! all run on a short unchanged, and nothing in the parent notices it: every
//! parent stage scans its own `drafts/`, never `shorts/`. What *is* different
//! about a short is said in exactly two places — it always renders vertical
//! ([`crate::config::RenderTargets::for_session`]) and it uploads as a Short
//! alone ([`crate::publish`]).
//!
//! Shorts belong to the project, not to a version: an aside recorded during v1
//! is still worth posting after v2 re-records the video around it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::notes::{Chapter, NotesData};
use crate::session::Session;

pub const SHORTS_DIR: &str = "shorts";
pub const SUGGESTIONS_JSON: &str = "suggestions.json";
pub const SUGGESTIONS_HTML: &str = "suggestions.html";
const PREFIX: &str = "short-";

/// One aside the suggest pass thinks can stand on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub title: String,
    /// The opening line, word for word: a Short is decided in its first second.
    pub hook: String,
    /// The beats to hit after the hook.
    #[serde(default)]
    pub points: Vec<String>,
    /// Why it works on its own — what the viewer walks away with.
    #[serde(default)]
    pub why: String,
    /// The chapters of the take it came from, for finding it again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<u32>,
}

impl Suggestion {
    /// The short's own teleprompter: one slide, hook first.
    fn slide(&self) -> Chapter {
        Chapter {
            title: self.title.clone(),
            points: self.points.clone(),
            verbatim: Some(self.hook.clone()).filter(|h| !h.trim().is_empty()),
            cues: Vec::new(),
        }
    }
}

/// Everything one suggest pass proposed, and which take it read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub shorts: Vec<Suggestion>,
}

impl Suggestions {
    /// The suggestions as a deck to read through, the reason on each slide.
    fn deck(&self, title: &str) -> NotesData {
        NotesData {
            title: title.to_string(),
            version: self.version,
            chapters: self
                .shorts
                .iter()
                .map(|short| {
                    let mut slide = short.slide();
                    slide.cues = Some(short.why.trim().to_string())
                        .filter(|why| !why.is_empty())
                        .into_iter()
                        .collect();
                    slide
                })
                .collect(),
        }
    }
}

/// Whether `root` is a short's project folder: `{parent}/shorts/short-NN`.
pub fn is_short(root: &Path) -> bool {
    number(root).is_some()
}

/// The project a short was recorded from, or `None` for a project that is not
/// a short.
pub fn parent_of(root: &Path) -> Option<&Path> {
    number(root)?;
    root.parent()?.parent()
}

/// A short's number, from its folder name.
pub fn number(root: &Path) -> Option<u32> {
    if root.parent()?.file_name()? != SHORTS_DIR {
        return None;
    }
    root.file_name()?
        .to_str()?
        .strip_prefix(PREFIX)?
        .parse()
        .ok()
}

/// Every short recorded from `parent`, in number order.
pub fn roots(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent.join(SHORTS_DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<(u32, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| number(&path).map(|n| (n, path)))
        .collect();
    out.sort();
    out.into_iter().map(|(_, path)| path).collect()
}

/// The folder of the project whose shorts these are: `root` itself, or its
/// parent when `root` is already a short.
fn family(root: &Path) -> &Path {
    parent_of(root).unwrap_or(root)
}

pub fn load_suggestions(root: &Path) -> Option<Suggestions> {
    let path = family(root).join(SHORTS_DIR).join(SUGGESTIONS_JSON);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the suggestions and the deck that shows them, returning the deck.
pub fn save_suggestions(root: &Path, title: &str, suggestions: &Suggestions) -> Result<PathBuf> {
    let dir = family(root).join(SHORTS_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let json = dir.join(SUGGESTIONS_JSON);
    std::fs::write(
        &json,
        serde_json::to_string_pretty(suggestions).context("serializing the suggestions")? + "\n",
    )
    .with_context(|| format!("writing {}", json.display()))?;
    let html = dir.join(SUGGESTIONS_HTML);
    let deck = crate::notes::render_deck(&suggestions.deck(title), "Short");
    std::fs::write(&html, deck).with_context(|| format!("writing {}", html.display()))?;
    Ok(html)
}

/// The suggestions deck, when a suggest pass has written one.
pub fn suggestions_html(root: &Path) -> Option<PathBuf> {
    let html = family(root).join(SHORTS_DIR).join(SUGGESTIONS_HTML);
    html.is_file().then_some(html)
}

/// A fresh short beside the ones `from`'s project already has, at v1, with
/// `suggestion` as its name and teleprompter. Called from a short, it makes a
/// sibling rather than a short of a short.
pub fn create(from: &Session, suggestion: Option<&Suggestion>) -> Result<Session> {
    let parent = family(&from.root);
    let n = roots(parent)
        .iter()
        .filter_map(|root| number(root))
        .max()
        .unwrap_or(0)
        + 1;
    let root = parent.join(SHORTS_DIR).join(format!("{PREFIX}{n:02}"));
    let dir = root.join("drafts").join("v1");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let session = Session {
        root,
        dir,
        version: Some(1),
    };
    let name = suggestion
        .map(|s| s.title.trim().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| format!("Short {n:02}"));
    session.set_name(&name)?;
    if let Some(suggestion) = suggestion {
        let notes = NotesData {
            title: name,
            version: Some(1),
            chapters: vec![suggestion.slide()],
        };
        crate::notes::write_deck(&session.notes_dir()?, &notes, "Short")?;
    }
    println!(
        "stream-recorder: new short {n:02} → {}",
        session.root.display()
    );
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shorts-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn project(root: &Path) -> Session {
        let dir = root.join("drafts").join("v1");
        std::fs::create_dir_all(&dir).unwrap();
        Session {
            root: root.to_path_buf(),
            dir,
            version: Some(1),
        }
    }

    fn suggestion(title: &str) -> Suggestion {
        Suggestion {
            title: title.into(),
            hook: "Nobody reads the second line.".into(),
            points: vec!["show it".into()],
            why: "one idea, one minute".into(),
            chapters: vec![2],
        }
    }

    #[test]
    fn a_short_knows_its_number_and_its_parent() {
        let parent = Path::new("/x/sessions/2026-09-22_10-00-00");
        let short = parent.join("shorts/short-03");
        assert_eq!(number(&short), Some(3));
        assert_eq!(parent_of(&short), Some(parent));
        assert!(!is_short(parent));
        // A chapter folder, or anything else named like a short but not under
        // `shorts/`, is not one.
        assert!(!is_short(&parent.join("edit/short-01")));
        assert!(!is_short(&parent.join("shorts/suggestions")));
    }

    #[test]
    fn create_numbers_past_the_last_and_writes_the_teleprompter() {
        let root = temp("create");
        let main = project(&root);
        let first = create(&main, Some(&suggestion("The second line"))).unwrap();
        assert_eq!(first.root, root.join("shorts/short-01"));
        assert!(first.dir.ends_with("short-01/drafts/v1"));
        assert_eq!(first.name().as_deref(), Some("The second line"));
        let notes = crate::notes::load_notes(&first.notes_dir().unwrap()).unwrap();
        assert_eq!(notes.chapters.len(), 1);
        assert_eq!(
            notes.chapters[0].verbatim.as_deref(),
            Some("Nobody reads the second line.")
        );

        // From inside a short, the next is a sibling, never a short of a short.
        let second = create(&first, None).unwrap();
        assert_eq!(second.root, root.join("shorts/short-02"));
        assert_eq!(second.name().as_deref(), Some("Short 02"));
        assert!(crate::notes::existing_html(&second).is_none());
        assert_eq!(roots(&root), vec![first.root.clone(), second.root.clone()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn suggestions_round_trip_from_the_parent_or_a_short() {
        let root = temp("suggest");
        let suggestions = Suggestions {
            version: Some(2),
            shorts: vec![suggestion("One"), suggestion("Two")],
        };
        let html = save_suggestions(&root, "Demo", &suggestions).unwrap();
        let page = std::fs::read_to_string(&html).unwrap();
        assert!(page.contains("Short 1"), "{page}");
        assert!(page.contains("one idea, one minute"));
        assert_eq!(load_suggestions(&root), Some(suggestions.clone()));
        // A short reads its parent's list: that is where the picker is filled from.
        let short = root.join("shorts/short-01");
        assert_eq!(load_suggestions(&short), Some(suggestions));
        assert_eq!(suggestions_html(&short), Some(html));
        let _ = std::fs::remove_dir_all(&root);
    }
}
