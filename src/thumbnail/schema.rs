//! The thumbnail ledger: what was generated, from what, and what was live when.
//!
//! Append-only at `{root}/thumbnails.jsonl`, beside the schedule and analytics
//! ledgers.
//!
//! Two row kinds, and the distinction is the whole A/B story. A [`Row::Candidate`]
//! records an image and everything that produced it. A [`Row::Activated`] records
//! that one *became the live thumbnail at a moment in time* — an event, not a
//! flag. A flag can only ever answer "which one won"; a timestamped event can
//! answer "how did each perform while it was up", which is the question a real
//! A/B test asks. It costs one extra field now and cannot be reconstructed later.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const THUMBNAILS_JSONL: &str = "thumbnails.jsonl";
pub const CANDIDATES_DIR: &str = "thumbnails/candidates";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Row {
    Candidate(Candidate),
    /// This candidate became the live thumbnail at `at`.
    Activated { id: String, at: String },
    /// Everything drawn before `at` was retired — Regenerate starting a fresh set.
    ///
    /// An event rather than a rewrite, for the same reason the rest of this file
    /// is append-only: deleting the rows would take the answer to "which image
    /// was live in week two" with them, and that cannot be reconstructed.
    Cleared { at: String },
}

/// One generated image and the inputs that identify it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Stable across runs: same inputs, same id.
    pub id: String,
    pub model: String,
    pub file: String,
    pub created_at: String,
    /// The brief itself, hashed — an edited brief is different work.
    pub brief_hash: String,
    /// The camera still, by content.
    pub still: String,
    /// The screen grab taken with it, by content. Absent for a talking-head
    /// layout, which has no screen to catch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// The active reference set, order-independent.
    pub refs: String,
}

impl Candidate {
    pub fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.file)
    }
}

/// The identity of a candidate, before it exists.
///
/// Anything that would change the picture is in here, so re-running with the same
/// inputs recognises the work already done and re-running after an edit does not.
pub fn candidate_id(
    model: &str,
    brief_hash: &str,
    still: &str,
    screen: &str,
    refs: &str,
    nth: usize,
) -> String {
    let seed = format!("{model}\n{brief_hash}\n{still}\n{screen}\n{refs}\n{nth}");
    format!("thumb-{}", &crate::agent::prompt::hash_of(&seed)[..12])
}

pub fn append(root: &Path, row: &Row) -> Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let path = root.join(THUMBNAILS_JSONL);
    let line = serde_json::to_string(row).context("serializing thumbnail row")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

pub fn load(root: &Path) -> Vec<Row> {
    let Ok(text) = std::fs::read_to_string(root.join(THUMBNAILS_JSONL)) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(n, line)| match serde_json::from_str(line) {
            Ok(row) => Some(row),
            Err(err) => {
                eprintln!("stream-recorder: skipping thumbnails.jsonl line {}: {err}", n + 1);
                None
            }
        })
        .collect()
}

/// The candidates on the strip right now: everything drawn since the last clear.
///
/// This is the set every other question is asked against — what the pane shows,
/// which one is live, and whether a piece of work has already been done — so a
/// cleared candidate stops counting as generated and can be drawn again.
pub fn candidates(rows: &[Row]) -> Vec<&Candidate> {
    let from = rows
        .iter()
        .rposition(|row| matches!(row, Row::Cleared { .. }))
        .map(|at| at + 1)
        .unwrap_or(0);
    rows[from..]
        .iter()
        .filter_map(|row| match row {
            Row::Candidate(candidate) => Some(candidate),
            _ => None,
        })
        .collect()
}

/// Retires every candidate on the strip: records the event, then removes the
/// images it retired. Returns how many files went.
///
/// The row is appended *before* the deletions, so an interruption leaves the
/// ledger already saying those images are gone rather than pointing the pane at
/// half a set of missing files.
#[allow(dead_code)] // Retained for explicit clearing and older ledger workflows.
pub fn clear(root: &Path, rows: &mut Vec<Row>, at: String) -> Result<usize> {
    let retiring: Vec<PathBuf> = candidates(rows)
        .iter()
        .map(|candidate| candidate.path(root))
        .collect();
    if retiring.is_empty() {
        return Ok(0);
    }
    let row = Row::Cleared { at };
    append(root, &row)?;
    rows.push(row);

    let mut removed = 0;
    for path in retiring {
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            // Already gone is the outcome we wanted, not a failure. Anything else
            // is worth saying, but never worth losing the new batch over.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!("stream-recorder: could not remove {}: {err}", path.display())
            }
        }
    }
    Ok(removed)
}

/// The candidate live right now — the most recent activation.
pub fn active(rows: &[Row]) -> Option<&Candidate> {
    let id = rows.iter().rev().find_map(|row| match row {
        Row::Activated { id, .. } => Some(id.as_str()),
        _ => None,
    })?;
    candidates(rows).into_iter().find(|c| c.id == id)
}

/// Every activation in order — the history an A/B comparison reads.
///
/// Nothing consumes this yet: it exists because the *recording* has to start now
/// to be worth anything later. A comparison written in six weeks cannot invent
/// which thumbnail was live in week two.
#[allow(dead_code)]
pub fn activations(rows: &[Row]) -> Vec<(&str, &str)> {
    rows.iter()
        .filter_map(|row| match row {
            Row::Activated { id, at } => Some((id.as_str(), at.as_str())),
            _ => None,
        })
        .collect()
}

/// True when this exact work has already been done.
pub fn already_generated(rows: &[Row], id: &str) -> bool {
    candidates(rows).iter().any(|candidate| candidate.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, brief: &str) -> Candidate {
        Candidate {
            id: id.into(),
            model: "google/gemini-3.1-flash-image".into(),
            file: format!("thumbnails/candidates/{id}.jpg"),
            created_at: "2026-08-15T09:00:00Z".into(),
            brief_hash: brief.into(),
            still: "still-abc".into(),
            screen: None,
            refs: "refs-def".into(),
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-thumbs-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_same_inputs_produce_the_same_id() {
        let a = candidate_id("m", "brief", "still", "screen", "refs", 0);
        assert_eq!(a, candidate_id("m", "brief", "still", "screen", "refs", 0));
        // Every input is part of the identity.
        assert_ne!(a, candidate_id("other", "brief", "still", "screen", "refs", 0));
        assert_ne!(a, candidate_id("m", "edited", "still", "screen", "refs", 0));
        assert_ne!(a, candidate_id("m", "brief", "other-still", "screen", "refs", 0));
        assert_ne!(a, candidate_id("m", "brief", "still", "screen", "other-refs", 0));
        // Two candidates from one model and one brief are still distinct.
        assert_ne!(a, candidate_id("m", "brief", "still", "screen", "refs", 1));
        // The screen grab is part of the identity too, or capturing a new slide
        // would recognise the old picture as already drawn.
        assert_ne!(a, candidate_id("m", "brief", "still", "other-screen", "refs", 0));
        assert_ne!(a, candidate_id("m", "brief", "still", "", "refs", 0));
        assert!(a.starts_with("thumb-"));
    }

    /// Writes a candidate's image where its row says it lives.
    fn draw(root: &Path, id: &str) -> Candidate {
        let made = candidate(id, "brief-1");
        let path = made.path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"jpg").unwrap();
        made
    }

    #[test]
    fn a_clear_retires_the_strip_and_deletes_its_images() {
        let root = temp("clear");
        let old = draw(&root, "thumb-old");
        let mut rows = vec![Row::Candidate(old.clone())];
        assert_eq!(clear(&root, &mut rows, "2026-08-15T10:00:00Z".into()).unwrap(), 1);
        assert!(!old.path(&root).exists(), "the retired image is gone");
        assert!(candidates(&rows).is_empty());

        // What is drawn afterwards is the whole strip.
        let fresh = draw(&root, "thumb-new");
        rows.push(Row::Candidate(fresh));
        let live: Vec<&str> = candidates(&rows).iter().map(|c| c.id.as_str()).collect();
        assert_eq!(live, ["thumb-new"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The point of clearing: redrawing the same brief has to actually redraw,
    /// and `already_generated` is what would otherwise skip it.
    #[test]
    fn a_cleared_candidate_can_be_drawn_again() {
        let root = temp("clear-redraw");
        draw(&root, "thumb-a");
        let mut rows = vec![Row::Candidate(candidate("thumb-a", "brief-1"))];
        assert!(already_generated(&rows, "thumb-a"));
        clear(&root, &mut rows, "2026-08-15T10:00:00Z".into()).unwrap();
        assert!(!already_generated(&rows, "thumb-a"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Clearing takes the pictures, never the record. A comparison written later
    /// still has to be able to say which thumbnail was live and when.
    #[test]
    fn a_clear_keeps_the_activation_history() {
        let root = temp("clear-history");
        draw(&root, "thumb-a");
        let mut rows = vec![
            Row::Candidate(candidate("thumb-a", "brief-1")),
            Row::Activated {
                id: "thumb-a".into(),
                at: "2026-08-15T09:30:00Z".into(),
            },
        ];
        clear(&root, &mut rows, "2026-08-15T10:00:00Z".into()).unwrap();
        assert_eq!(activations(&rows), [("thumb-a", "2026-08-15T09:30:00Z")]);
        // But it is no longer live: the image it named is gone.
        assert!(active(&rows).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_an_empty_strip_writes_nothing() {
        let root = temp("clear-empty");
        let mut rows = Vec::new();
        assert_eq!(clear(&root, &mut rows, "2026-08-15T10:00:00Z".into()).unwrap(), 0);
        assert!(rows.is_empty(), "no event for a clear that retired nothing");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rows_round_trip_through_the_ledger() {
        let root = temp("roundtrip");
        append(&root, &Row::Candidate(candidate("thumb-a", "brief-1"))).unwrap();
        append(
            &root,
            &Row::Activated {
                id: "thumb-a".into(),
                at: "2026-08-15T10:00:00Z".into(),
            },
        )
        .unwrap();
        let rows = load(&root);
        assert_eq!(rows.len(), 2);
        assert_eq!(candidates(&rows).len(), 1);
        assert_eq!(active(&rows).unwrap().id, "thumb-a");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The A/B property: activations are history, not a single winner.
    #[test]
    fn every_activation_is_kept_and_the_latest_is_live() {
        let root = temp("activations");
        append(&root, &Row::Candidate(candidate("thumb-a", "b"))).unwrap();
        append(&root, &Row::Candidate(candidate("thumb-b", "b"))).unwrap();
        append(&root, &Row::Activated { id: "thumb-a".into(), at: "day-1".into() }).unwrap();
        append(&root, &Row::Activated { id: "thumb-b".into(), at: "day-8".into() }).unwrap();
        append(&root, &Row::Activated { id: "thumb-a".into(), at: "day-15".into() }).unwrap();

        let rows = load(&root);
        assert_eq!(
            activations(&rows),
            vec![("thumb-a", "day-1"), ("thumb-b", "day-8"), ("thumb-a", "day-15")],
            "the whole history survives, so each window is measurable"
        );
        assert_eq!(active(&rows).unwrap().id, "thumb-a", "the latest wins");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_is_active_until_something_is_chosen() {
        let root = temp("unchosen");
        append(&root, &Row::Candidate(candidate("thumb-a", "b"))).unwrap();
        let rows = load(&root);
        assert!(active(&rows).is_none());
        assert!(activations(&rows).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An activation naming a candidate we do not have must not resolve to a
    /// different one.
    #[test]
    fn a_dangling_activation_yields_nothing() {
        let root = temp("dangling");
        append(&root, &Row::Candidate(candidate("thumb-a", "b"))).unwrap();
        append(&root, &Row::Activated { id: "thumb-missing".into(), at: "now".into() }).unwrap();
        assert!(active(&load(&root)).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn already_generated_recognises_finished_work() {
        let rows = vec![Row::Candidate(candidate("thumb-a", "b"))];
        assert!(already_generated(&rows, "thumb-a"));
        assert!(!already_generated(&rows, "thumb-b"));
    }

    #[test]
    fn a_corrupt_line_is_skipped_and_the_rest_survive() {
        let root = temp("corrupt");
        append(&root, &Row::Candidate(candidate("thumb-a", "b"))).unwrap();
        let path = root.join(THUMBNAILS_JSONL);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{{ truncated\n{text}")).unwrap();
        assert_eq!(load(&root).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_ledger_reads_as_empty() {
        assert!(load(&PathBuf::from("/nonexistent/stream-recorder-thumbs")).is_empty());
    }
}
