//! Critique the take, and rewrite the speaking notes for the next one.
//!
//! The author records a take, reads it back as a critique — what each chapter
//! got right, the one change that would help most, how to reorder the whole —
//! and gives a direction of their own ("lead with the demo, merge 2 and 3").
//! Then the plan is rewritten from what was actually said, the critique and
//! that direction, as a new plan version, and approved. Approving is what
//! writes the speaking notes (see [`crate::plan::approve`]), so the teleprompter
//! on the Record tab, the chapter cards and the deck all move to the new take's
//! plan at once. Nothing is lost: the plan it replaced is still a version on
//! the Plan tab.
//!
//! Two model calls, not one, so each stays what it is best at: the critique is
//! [`crate::agent::critique`], and the rewrite is the plan extractor itself
//! (`agent::plan`), refining the plan the take was recorded against — which
//! means the new plan has the same shape rules as any other: hook first, CTA
//! last.
//!
//! The critique is of this recording version, and kept beside its takes as
//! `drafts/vN/critique.json`.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::session::Session;

pub const CRITIQUE_JSON: &str = "critique.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterCritique {
    pub n: u32,
    #[serde(default)]
    pub title: String,
    /// What to keep.
    #[serde(default)]
    pub worked: String,
    /// The single most useful change.
    #[serde(default)]
    pub fix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Critique {
    #[serde(default)]
    pub overall: String,
    #[serde(default)]
    pub chapters: Vec<ChapterCritique>,
    /// How to reorder, merge, split or cut for the next take.
    #[serde(default)]
    pub reorganize: String,
    /// The author's own direction it was asked with.
    #[serde(default)]
    pub direction: String,
    /// The plan version rewritten from it, which is now the speaking notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_written: Option<u32>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub created_at: String,
}

pub fn path(session: &Session) -> PathBuf {
    session.dir.join(CRITIQUE_JSON)
}

pub fn load(session: &Session) -> Option<Critique> {
    let text = std::fs::read_to_string(path(session)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save(session: &Session, critique: &Critique) -> Result<()> {
    let path = path(session);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(critique)? + "\n")
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
}

/// The critique as text — what the plan rewrite is given, and what Copy puts
/// on the pasteboard.
pub fn plain_text(critique: &Critique) -> String {
    let mut out = critique.overall.trim().to_string();
    for chapter in &critique.chapters {
        let name = if chapter.title.is_empty() {
            format!("Chapter {}", chapter.n)
        } else {
            format!("Chapter {} — {}", chapter.n, chapter.title)
        };
        out.push_str(&format!("\n\n{name}"));
        if !chapter.worked.is_empty() {
            out.push_str(&format!("\nKeep: {}", chapter.worked));
        }
        if !chapter.fix.is_empty() {
            out.push_str(&format!("\nChange: {}", chapter.fix));
        }
    }
    if !critique.reorganize.trim().is_empty() {
        out.push_str(&format!("\n\nReorganize: {}", critique.reorganize.trim()));
    }
    out.trim().to_string()
}

/// Why a critique cannot start now, if it cannot: nothing recorded, or a
/// chapter still transcribing (its words would be missing from the critique).
pub fn refusal(session: &Session) -> Option<String> {
    let closed = crate::notes::closed_chapter_numbers(&session.dir);
    if closed.is_empty() {
        return Some("Record a take first — there are no chapters to critique.".into());
    }
    let running = crate::notes::still_running(&session.dir, &closed);
    if !running.is_empty() {
        return Some(format!(
            "{} — critique once every chapter has its words.",
            crate::notes::waiting_message(&running)
        ));
    }
    None
}

/// Critique the take and rewrite the speaking notes from it. Blocking — two
/// model calls; run it off the main thread.
pub fn run(
    session: &Session,
    direction: &str,
    model: &str,
    provider: Option<&str>,
) -> Result<Critique> {
    let chapters = crate::summary::final_transcript(session);
    if chapters.is_empty() {
        bail!("no chapter of this take has any words to critique");
    }
    let project = session.name().unwrap_or_else(|| "Video".into());
    let base = crate::plan::plan_for_recording(session);
    let (mut critique, step) = crate::agent::critique::critique(
        &project,
        &chapters,
        base.as_ref().map(|plan| &plan.body),
        direction,
        model,
        provider,
        Some(&session.root),
    )?;
    critique.direction = direction.trim().to_string();
    critique.model = model.to_string();
    critique.created_at = chrono::Local::now().to_rfc3339();
    crate::agent::trace::write_step(&session.dir, &session.root, &step)?;

    // The rewrite: what was said is the material, the critique and the
    // direction are the note, and the plan the take followed — when it had one —
    // is what is refined.
    let plan_dir = crate::plan::dir(session);
    let input = crate::plan::load_input(&plan_dir);
    let note = rewrite_note(&critique);
    let sources = crate::agent::plan::Sources {
        instructions: input.instructions,
        typed: String::new(),
        takes: Vec::new(),
        rehearsal: chapters.iter().map(|c| (c.n, c.text.clone())).collect(),
    };
    let refine = base
        .as_ref()
        .map(|base| crate::agent::plan::Refine { base, note: &note });
    let (body, step) = crate::agent::plan::build_plan(
        &project,
        &sources,
        refine.as_ref(),
        model,
        provider,
        Some(&session.root),
    )?;
    let version = session.version.map_or(String::new(), |v| format!(" v{v}"));
    let written = crate::plan::save_new(
        &plan_dir,
        crate::plan::Plan {
            number: 0,
            body,
            approved: false,
            refined_from: base.as_ref().map(|plan| plan.number),
            refine_note: if critique.direction.is_empty() {
                "Rewritten from the critique of the take".into()
            } else {
                critique.direction.clone()
            },
            sources: vec![format!("critique of take{version}")],
            created_at: chrono::Local::now().to_rfc3339(),
        },
    )?;
    crate::agent::trace::write_step(&plan_dir, &session.root, &step)?;
    // Approving is what makes it the speaking notes — the teleprompter, the
    // cards and the deck — and un-approves the plan it replaced.
    crate::plan::approve_for(session, written.number)?;
    critique.plan_written = Some(written.number);
    save(session, &critique)?;
    Ok(critique)
}

/// The plan rewrite's refine note: the author's direction first, since it is
/// theirs, then the critique that informs it.
fn rewrite_note(critique: &Critique) -> String {
    let direction = if critique.direction.is_empty() {
        "(none — follow the critique)".to_string()
    } else {
        critique.direction.clone()
    };
    format!(
        "Rewrite this plan for the next take, from the take just recorded (the rehearsal \
         chapters) and this critique of it. Keep what worked; make the changes; reorganize \
         as the direction and the critique say.\n\nAuthor's direction: {direction}\n\n\
         Critique:\n{}",
        plain_text(critique)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn critique() -> Critique {
        Critique {
            overall: "Strong hook, slow middle.".into(),
            chapters: vec![
                ChapterCritique {
                    n: 1,
                    title: "The hour".into(),
                    worked: "The number lands.".into(),
                    fix: "Say it first.".into(),
                },
                ChapterCritique {
                    n: 2,
                    title: String::new(),
                    worked: String::new(),
                    fix: "Cut the aside.".into(),
                },
            ],
            reorganize: "Merge 2 into 1.".into(),
            direction: "Lead with the demo".into(),
            ..Critique::default()
        }
    }

    #[test]
    fn the_critique_reads_as_text_chapter_by_chapter() {
        assert_eq!(
            plain_text(&critique()),
            "Strong hook, slow middle.\n\nChapter 1 — The hour\nKeep: The number lands.\n\
             Change: Say it first.\n\nChapter 2\nChange: Cut the aside.\n\nReorganize: Merge 2 \
             into 1."
        );
    }

    #[test]
    fn the_rewrite_puts_the_authors_direction_before_the_critique() {
        let note = rewrite_note(&critique());
        let direction = note.find("Author's direction: Lead with the demo").unwrap();
        let body = note.find("Critique:\nStrong hook").unwrap();
        assert!(direction < body);
        let mut none = critique();
        none.direction.clear();
        assert!(rewrite_note(&none).contains("(none — follow the critique)"));
    }

    #[test]
    fn nothing_recorded_is_refused_before_any_model_call() {
        let root = std::env::temp_dir().join(format!("critique-{}-empty", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("drafts").join("v1");
        std::fs::create_dir_all(&dir).unwrap();
        let session = Session {
            root,
            dir,
            version: Some(1),
        };
        assert!(refusal(&session).unwrap().contains("Record a take first"));
        assert!(run(&session, "", "m", None).is_err());
    }

    #[test]
    fn a_critique_round_trips_beside_the_take() {
        let root = std::env::temp_dir().join(format!("critique-{}-save", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("drafts").join("v1");
        std::fs::create_dir_all(&dir).unwrap();
        let session = Session {
            root,
            dir,
            version: Some(1),
        };
        let mut saved = critique();
        saved.plan_written = Some(4);
        save(&session, &saved).unwrap();
        assert_eq!(load(&session).unwrap(), saved);
        assert!(path(&session).ends_with("drafts/v1/critique.json"));
    }
}
