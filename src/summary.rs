//! A summary of the final video: what the rendered cut actually says.
//!
//! Written from the transcript of the *cut*, not the takes: each chapter's
//! words, less the ones the edit removed (see [`crate::edit::keep`]), in the
//! order the longform plays them. A retaken chapter's first take is in
//! `.discarded/` and so is not read at all. What comes back is a short summary,
//! the takeaways, and a line per chapter — for the author to read before the
//! video goes out, and to paste wherever it is described.
//!
//! One per recording version, at `render/vN/summary.json`, beside the render it
//! describes. It records a hash of the transcript it was written from, so a
//! render that changed the cut is known to have made it stale.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::session::Session;

pub const SUMMARY_JSON: &str = "summary.json";

/// One chapter of the final video, as it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalChapter {
    pub n: u32,
    /// The card title, the same one the render drew; empty when there is none.
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterSummary {
    pub n: u32,
    #[serde(default)]
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub summary: String,
    #[serde(default)]
    pub takeaways: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<ChapterSummary>,
    /// Of the final transcript it was written from — see [`transcript_hash`].
    pub transcript_hash: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub created_at: String,
}

/// The final video's transcript, chapter by chapter.
///
/// A chapter that was cut has its words filtered by its keep-list; one that
/// was never cut (no keep-list yet) reads as recorded. A chapter with no words
/// at all is left out rather than sent as an empty section.
pub fn final_transcript(session: &Session) -> Vec<FinalChapter> {
    let numbers = crate::notes::closed_chapter_numbers(&session.dir);
    let titles = crate::edit::chapter_titles(session, &numbers);
    let edit_root = session.edit_dir();
    numbers
        .into_iter()
        .filter_map(|n| {
            let transcript = crate::notes::load_transcript(&session.dir, n)?;
            let keep = crate::edit::keep::load(&crate::edit::pane::chapter_dir(&edit_root, n));
            let text = match (&keep, transcript.words.is_empty()) {
                (Some(keep), false) => transcript
                    .words
                    .iter()
                    .filter(|word| keep.overlaps(word.start, word.end))
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => transcript.text.clone(),
            };
            let text = text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let title = titles
                .iter()
                .find(|(m, _)| *m == n)
                .map(|(_, title)| title.clone())
                .unwrap_or_default();
            Some(FinalChapter { n, title, text })
        })
        .collect()
}

/// What a summary's freshness is judged by: the words, in order, and the
/// chapter each belongs to. Titles are left out — renaming a chapter does not
/// change what the video says.
pub fn transcript_hash(chapters: &[FinalChapter]) -> String {
    let joined = chapters
        .iter()
        .map(|chapter| format!("{}\n{}", chapter.n, chapter.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    crate::agent::prompt::hash_of(&joined)
}

pub fn path(session: &Session) -> PathBuf {
    session.render_dir().join(SUMMARY_JSON)
}

pub fn load(session: &Session) -> Option<Summary> {
    let text = std::fs::read_to_string(path(session)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(session: &Session, summary: &Summary) -> Result<()> {
    write(&path(session), summary)
}

fn write(path: &Path, summary: &Summary) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(summary)? + "\n")
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// Summarize `session`'s final video. Blocking — a model call; run it off the
/// main thread. Refuses with no transcript to summarize.
pub fn generate(session: &Session, model: &str, provider: Option<&str>) -> Result<Summary> {
    let chapters = final_transcript(session);
    if chapters.is_empty() {
        bail!(
            "no transcript for this version's chapters yet — record, wait for transcription, then \
             Render"
        );
    }
    let project = session.name().unwrap_or_else(|| "Video".into());
    let (mut summary, step) = crate::agent::summary::summarize(
        &project,
        &chapters,
        model,
        provider,
        Some(&session.root),
    )?;
    summary.transcript_hash = transcript_hash(&chapters);
    summary.model = model.to_string();
    summary.created_at = chrono::Local::now().to_rfc3339();
    save(session, &summary)?;
    crate::agent::trace::write_step(&session.render_dir(), &session.root, &step)?;
    Ok(summary)
}

/// The summary as plain text, for the Copy button.
pub fn plain_text(summary: &Summary) -> String {
    let mut out = summary.summary.trim().to_string();
    if !summary.takeaways.is_empty() {
        out.push_str("\n\nKey takeaways:\n");
        for line in &summary.takeaways {
            out.push_str(&format!("- {line}\n"));
        }
    }
    if !summary.chapters.is_empty() {
        out.push_str(if summary.takeaways.is_empty() {
            "\n\nChapters:\n"
        } else {
            "\nChapters:\n"
        });
        for chapter in &summary.chapters {
            if chapter.title.is_empty() {
                out.push_str(&format!("{}. {}\n", chapter.n, chapter.summary));
            } else {
                out.push_str(&format!(
                    "{}. {} — {}\n",
                    chapter.n, chapter.title, chapter.summary
                ));
            }
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(tag: &str) -> Session {
        let root = std::env::temp_dir().join(format!("summary-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("drafts").join("v1");
        std::fs::create_dir_all(&dir).unwrap();
        Session {
            root,
            dir,
            version: Some(1),
        }
    }

    fn record(session: &Session, n: u32, words: &[(&str, i64, i64)]) {
        std::fs::write(session.dir.join(format!("chapter-{n:02}.mp3")), "").unwrap();
        let words: Vec<serde_json::Value> = words
            .iter()
            .map(|(text, start, end)| serde_json::json!({"text": text, "start": start, "end": end}))
            .collect();
        let text = words
            .iter()
            .map(|w| w["text"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        std::fs::write(
            session.dir.join(format!("chapter-{n:02}.transcript.json")),
            serde_json::json!({"status": "completed", "text": text, "words": words}).to_string(),
        )
        .unwrap();
    }

    /// The summary is of what plays: words the cut removed are not in it, and
    /// a chapter never cut reads as recorded.
    #[test]
    fn the_final_transcript_drops_what_the_cut_removed() {
        let session = project("final");
        record(
            &session,
            1,
            &[("So", 0, 200), ("um", 300, 500), ("deploys", 600, 900)],
        );
        record(&session, 2, &[("Faster", 0, 400), ("now", 500, 800)]);
        let list = crate::edit::keep::KeepList::hand(
            1,
            1_000,
            vec![
                crate::edit::keep::Keep::new(0, 250),
                crate::edit::keep::Keep::new(550, 1_000),
            ],
        );
        crate::edit::keep::save(
            &crate::edit::pane::chapter_dir(&session.edit_dir(), 1),
            &list,
        )
        .unwrap();
        let chapters = final_transcript(&session);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].text, "So deploys");
        assert_eq!(chapters[1].text, "Faster now");
    }

    #[test]
    fn the_hash_moves_with_the_words_not_the_titles() {
        let a = vec![FinalChapter {
            n: 1,
            title: "One".into(),
            text: "So deploys".into(),
        }];
        let mut renamed = a.clone();
        renamed[0].title = "Renamed".into();
        let mut recut = a.clone();
        recut[0].text = "So um deploys".into();
        assert_eq!(transcript_hash(&a), transcript_hash(&renamed));
        assert_ne!(transcript_hash(&a), transcript_hash(&recut));
    }

    #[test]
    fn a_summary_round_trips_and_copies_as_text() {
        let session = project("save");
        let summary = Summary {
            summary: "How deploys got fast.".into(),
            takeaways: vec!["Cache the layers".into()],
            chapters: vec![
                ChapterSummary {
                    n: 1,
                    title: "The hour".into(),
                    summary: "Why it was slow.".into(),
                },
                ChapterSummary {
                    n: 2,
                    title: String::new(),
                    summary: "The fix.".into(),
                },
            ],
            transcript_hash: "abc".into(),
            model: "m".into(),
            created_at: String::new(),
        };
        save(&session, &summary).unwrap();
        assert_eq!(load(&session).unwrap(), summary);
        assert_eq!(
            plain_text(&summary),
            "How deploys got fast.\n\nKey takeaways:\n- Cache the layers\n\nChapters:\n\
             1. The hour — Why it was slow.\n2. The fix."
        );
    }

    #[test]
    fn nothing_to_summarize_is_refused() {
        let session = project("empty");
        assert!(generate(&session, "m", None).is_err());
    }
}
