//! Emptying the Buffer queue — the undo for a Queue press that went wrong.
//!
//! Two scopes, because "clear the queue" means two different things depending on
//! whose queue it is. [`Scope::Project`] deletes only the posts this project's
//! ledger recorded, which is the set this tool created and the only set it can
//! honestly claim to own. [`Scope::Everything`] deletes every pending post in the
//! Buffer organization, including work queued by hand or by another project.
//!
//! Neither is reversible at Buffer's end, so the caller confirms first and this
//! module reports per-post rather than aborting: one refused delete must not
//! strand the rest of a queue half-emptied with no record of which half.

use std::sync::mpsc::Sender;

use anyhow::{Context, Result};

use crate::session::Session;

use super::buffer::BufferClient;
use super::ledger;
use super::schema::ScheduleRow;
use super::ScheduleEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only what this project queued, by post id from `schedule.jsonl`.
    Project,
    /// Every pending post in the organization, whatever queued it.
    Everything,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Project => "this project",
            Scope::Everything => "the whole Buffer queue",
        }
    }
}

/// What one clear actually did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClearOutcome {
    pub deleted: usize,
    /// Rows whose post was already gone from Buffer — deleted by hand, most
    /// likely — and which this run tombstoned without asking Buffer to delete
    /// anything. Reconciling them is the whole reason a clear reads the queue
    /// first: they are otherwise a permanent "already queued" against nothing.
    pub reconciled: usize,
    /// Rows whose post has already been published. Left completely alone: the
    /// video is out in the world, and tombstoning it would invite the planner to
    /// post the same thing twice.
    pub published: usize,
    /// Posts Buffer refused to delete, each with its reason.
    pub failed: Vec<String>,
    /// Deleted at Buffer but not tombstoned in the ledger — those will keep
    /// blocking a re-queue as "already queued" against a post that is gone, which
    /// is the one outcome worth shouting about.
    pub unrecorded: Vec<String>,
}

impl ClearOutcome {
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("Deleted {} post(s)", self.deleted)];
        if self.reconciled > 0 {
            parts.push(format!("{} already gone", self.reconciled));
        }
        if self.published > 0 {
            parts.push(format!("{} left published", self.published));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} refused", self.failed.len()));
        }
        if !self.unrecorded.is_empty() {
            parts.push(format!(
                "{} NOT recorded in the ledger",
                self.unrecorded.len()
            ));
        }
        parts.join(", ")
    }
}

/// How many posts a clear would delete, for the confirmation that precedes it.
///
/// Project scope answers from the ledger without asking Buffer anything; the
/// whole-queue scope has to ask, because only Buffer knows what else is in there.
pub fn count(session: &Session, scope: Scope) -> Result<usize> {
    match scope {
        Scope::Project => Ok(ledger::live_rows(&ledger::load_rows(&session.root)?).len()),
        Scope::Everything => Ok(BufferClient::from_env()?.pending_posts()?.len()),
    }
}

pub fn run(session: &Session, scope: Scope, tx: &Sender<ScheduleEvent>) -> Result<ClearOutcome> {
    let status = |msg: String| {
        let _ = tx.send(ScheduleEvent::Status(msg));
    };
    let client = BufferClient::from_env()?;
    let rows = ledger::load_rows(&session.root)?;
    let mut outcome = ClearOutcome::default();

    // Post id → the ledger row that recorded it, so a whole-queue clear still
    // tombstones the ones this project owns instead of losing track of them.
    let owned: Vec<&ScheduleRow> = ledger::live_rows(&rows);

    // Read the queue before touching it. The ledger says what this project once
    // sent, not what is still there — posts get deleted in Buffer's own UI, and
    // then every one of those rows blocks a re-queue forever against a post that
    // does not exist. Asking first is what lets those rows be reconciled instead.
    status("Asking Buffer what is still queued…".to_string());
    let existing = client
        .posts_by_status(&crate::schedule::buffer::ALL_STATUSES)
        .context("listing buffer posts")?;
    let deletable: std::collections::BTreeSet<&str> = existing
        .iter()
        .filter(|post| crate::schedule::buffer::PENDING_STATUSES.contains(&post.status.as_str()))
        .map(|post| post.id.as_str())
        .collect();
    let known: std::collections::BTreeSet<&str> =
        existing.iter().map(|post| post.id.as_str()).collect();

    if scope == Scope::Project {
        // Rows Buffer has never heard of are already gone; tombstone them here so
        // the delete loop below only handles posts that really are in the queue.
        for row in &owned {
            let id = row.buffer_post_id.as_str();
            if known.contains(id) {
                continue;
            }
            match ledger::append_row(&session.root, &row.tombstone(ledger::now_rfc3339())) {
                Ok(()) => outcome.reconciled += 1,
                Err(err) => {
                    eprintln!("stream-recorder: could not reconcile {id}: {err:#}");
                    outcome.unrecorded.push(id.to_string());
                }
            }
        }
        // Published is not queued. Counted so the total adds up, never touched.
        outcome.published = owned
            .iter()
            .filter(|row| {
                let id = row.buffer_post_id.as_str();
                known.contains(id) && !deletable.contains(id)
            })
            .count();
    }

    let targets: Vec<String> = match scope {
        Scope::Project => owned
            .iter()
            .map(|row| row.buffer_post_id.clone())
            .filter(|id| deletable.contains(id.as_str()))
            .collect(),
        Scope::Everything => deletable.iter().map(|id| (*id).to_string()).collect(),
    };

    if targets.is_empty() {
        status(format!(
            "Nothing left to delete in {} ({}).",
            scope.label(),
            outcome.summary()
        ));
        return Ok(outcome);
    }
    status(format!("Deleting {} post(s) from Buffer…", targets.len()));

    for (index, post_id) in targets.iter().enumerate() {
        status(format!(
            "Deleting {} of {}…",
            index + 1,
            targets.len()
        ));
        if let Err(err) = client.delete_post(post_id) {
            eprintln!("stream-recorder: could not delete {post_id}: {err:#}");
            outcome.failed.push(format!("{post_id}: {err:#}"));
            continue;
        }
        outcome.deleted += 1;

        // Tombstone immediately, one row at a time: a crash mid-clear must leave
        // the ledger truthful about exactly which posts are already gone.
        let Some(row) = owned.iter().find(|row| &row.buffer_post_id == post_id) else {
            continue;
        };
        let stone = row.tombstone(ledger::now_rfc3339());
        if let Err(err) = ledger::append_row(&session.root, &stone) {
            eprintln!("stream-recorder: deleted {post_id} but could not record it: {err:#}");
            outcome.unrecorded.push(post_id.clone());
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, post: &str, deleted: bool) -> ScheduleRow {
        ScheduleRow {
            id: id.into(),
            buffer_post_id: post.into(),
            project: "vd-42".into(),
            version: Some(1),
            video_id: "chapter-01".into(),
            platform: "tiktok".into(),
            channel_id: "chan".into(),
            url: "https://cdn.example.com/c1.mp4".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "hash".into(),
            queued_at: "2026-08-15T20:33:42Z".into(),
            deleted_at: deleted.then(|| "2026-08-15T21:00:00Z".to_string()),
        }
    }

    #[test]
    fn live_rows_are_what_a_project_clear_targets() {
        let rows = vec![row("a", "post-a", false), row("b", "post-b", false)];
        let live: Vec<&str> = ledger::live_rows(&rows)
            .iter()
            .map(|row| row.buffer_post_id.as_str())
            .collect();
        assert_eq!(live, ["post-a", "post-b"]);
    }

    /// The tombstone is a second row with the same key, so the newest one wins
    /// and the post drops out of the live set without the original being touched.
    #[test]
    fn a_tombstoned_post_is_no_longer_live() {
        let rows = vec![row("a", "post-a", false), row("a", "post-a", true)];
        assert!(ledger::live_rows(&rows).is_empty());
        // And the original row is still there to say it once went out.
        assert_eq!(rows[0].deleted_at, None);
    }

    /// The point of tombstoning: clearing the queue has to make the item sendable
    /// again, or the plan skips it forever against a post that no longer exists.
    #[test]
    fn clearing_makes_an_item_queueable_again() {
        let rows = vec![row("a", "post-a", false)];
        assert!(ledger::queued_row(&rows, "chapter-01", "tiktok", "hash").is_some());
        let mut cleared = rows.clone();
        cleared.push(rows[0].tombstone("2026-08-15T21:00:00Z".into()));
        assert!(ledger::queued_row(&cleared, "chapter-01", "tiktok", "hash").is_none());
        // The different-copy warning goes quiet too — nothing is live to warn about.
        assert!(ledger::prior_row(&cleared, "chapter-01", "tiktok").is_none());
    }

    #[test]
    fn the_summary_names_every_count_that_is_not_zero() {
        let clean = ClearOutcome { deleted: 16, ..ClearOutcome::default() };
        assert_eq!(clean.summary(), "Deleted 16 post(s)");
        let messy = ClearOutcome {
            deleted: 2,
            reconciled: 3,
            published: 1,
            failed: vec!["post-x: nope".into()],
            unrecorded: vec!["post-y".into()],
        };
        assert_eq!(
            messy.summary(),
            "Deleted 2 post(s), 3 already gone, 1 left published, 1 refused, \
             1 NOT recorded in the ledger"
        );
    }

    /// The state a hand-cleared queue leaves behind: the ledger still claims 16
    /// live posts and Buffer has none of them. Reconciling has to free those rows
    /// without a delete call, or Build Plan skips every chapter forever.
    #[test]
    fn a_post_deleted_by_hand_is_reconciled_rather_than_deleted_again() {
        let queued = row("a", "post-a", false);
        let known: std::collections::BTreeSet<&str> = ["post-b"].into_iter().collect();
        assert!(!known.contains(queued.buffer_post_id.as_str()));

        // What run() does for that row: tombstone it, ask Buffer for nothing.
        let mut rows = vec![queued.clone()];
        rows.push(queued.tombstone("2026-08-15T21:00:00Z".into()));
        assert!(ledger::live_rows(&rows).is_empty());
        assert!(ledger::queued_row(&rows, "chapter-01", "tiktok", "hash").is_none());
    }
}
