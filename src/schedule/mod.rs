//! Buffer scheduling stage: plan first, queue second.
//!
//! [`spawn_plan`] is pure-ish reconnaissance — it reads `posts.json`, the S3
//! `links.json` and the ledger, asks Buffer which channels exist, and writes a
//! reviewable `schedule/vN/schedule.json` carrying the exact payload (metadata
//! included) each post would send. Nothing is posted.
//!
//! [`spawn_queue`] never re-plans: it loads the file a human just looked at and
//! sends exactly its queueable items. The *ledger* — not the plan — decides what
//! is already live, so pressing Queue twice cannot post twice. Each successful
//! `createPost` is appended to `schedule.jsonl` immediately and the saved plan is
//! rewritten at the end, so a crash halfway through still leaves a truthful record.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

use crate::session::Session;

pub mod approve;
pub mod buffer;
pub mod channels;
pub mod clear;
pub mod copy;
pub mod ledger;
pub mod meta;
pub mod metrics;
pub mod plan;
pub mod schema;
pub mod send;
pub mod view;

pub use schema::{load_plan, save_plan, SchedulePlan};
pub use view::ScheduleForm;

use buffer::BufferClient;
use schema::SCHEDULE_JSONL;
use send::{post_input, row_for};

/// What one Queue press actually did. `queued` counts posts that reached Buffer,
/// `unrecorded` names the subset whose ledger append then failed — those are live
/// but invisible to the next plan, which is the one case worth shouting about.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueueOutcome {
    pub ledger: PathBuf,
    pub queued: usize,
    pub failed: usize,
    pub already: usize,
    pub unrecorded: Vec<String>,
}

impl QueueOutcome {
    /// One line for the status bar, naming every count that is not zero.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("Queued {} post(s)", self.queued)];
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if self.already > 0 {
            parts.push(format!("{} already queued", self.already));
        }
        if !self.unrecorded.is_empty() {
            parts.push(format!("{} NOT recorded in the ledger", self.unrecorded.len()));
        }
        parts.join(", ")
    }
}

pub enum ScheduleEvent {
    Status(String),
    /// The saved `schedule.json` and the plan it holds.
    Planned(PathBuf, SchedulePlan),
    Queued(QueueOutcome),
    Cleared(clear::ClearOutcome),
    /// Terminal failure of a plan, queue or clear job.
    Failed(String),
}

#[tracing::instrument(skip_all, fields(scope = ?scope))]
pub fn spawn_clear(session: Session, scope: clear::Scope, tx: Sender<ScheduleEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("schedule-clear".into())
        .spawn(move || match clear::run(&session, scope, &tx) {
            Ok(outcome) => {
                eprintln!("stream-recorder: clear {} → {}", scope.label(), outcome.summary());
                let _ = tx.send(ScheduleEvent::Cleared(outcome));
            }
            Err(err) => {
                eprintln!("stream-recorder: clear failed: {err:#}");
                let _ = tx.send(ScheduleEvent::Failed(format!("Clear failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start schedule clear job: {err}");
        let _ = unstarted.send(ScheduleEvent::Failed(format!(
            "Could not start the clear job: {err}"
        )));
    }
}

#[tracing::instrument(skip_all, fields(version = ?session.version))]
pub fn spawn_plan(session: Session, tx: Sender<ScheduleEvent>) {
    if let Err(err) = thread::Builder::new()
        .name("schedule-plan".into())
        .spawn(move || match run_plan(&session, &tx) {
            Ok((path, plan)) => {
                eprintln!("stream-recorder: schedule plan → {}", path.display());
                let _ = tx.send(ScheduleEvent::Planned(path, plan));
            }
            Err(err) => {
                eprintln!("stream-recorder: schedule plan failed: {err:#}");
                let _ = tx.send(ScheduleEvent::Failed(format!("Plan failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start schedule plan job: {err}");
    }
}

#[tracing::instrument(skip_all, fields(version = ?session.version))]
pub fn spawn_queue(session: Session, tx: Sender<ScheduleEvent>) {
    if let Err(err) = thread::Builder::new()
        .name("schedule-queue".into())
        .spawn(move || match run_queue(&session, &tx) {
            Ok(outcome) => {
                eprintln!(
                    "stream-recorder: schedule {} → {}",
                    outcome.summary(),
                    outcome.ledger.display()
                );
                let _ = tx.send(ScheduleEvent::Queued(outcome));
            }
            Err(err) => {
                eprintln!("stream-recorder: schedule queue failed: {err:#}");
                let _ = tx.send(ScheduleEvent::Failed(format!("Queue failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start schedule queue job: {err}");
    }
}

fn run_plan(session: &Session, tx: &Sender<ScheduleEvent>) -> Result<(PathBuf, SchedulePlan)> {
    let status = |msg: String| {
        let _ = tx.send(ScheduleEvent::Status(msg));
    };

    let posts_dir = session.posts_dir();
    status(format!("Reading posts from {}…", posts_dir.display()));
    let posts = crate::posts::load_manifest(&posts_dir)
        .context("no generated posts — run the Post tab first")?;

    let distribute_dir = session.distribute_dir();
    status(format!("Reading S3 links from {}…", distribute_dir.display()));
    let links = crate::distribute::load(&distribute_dir)
        .context("no public urls — run Distribute first")?;

    if let Some(warning) = version_mismatch(session.version, posts.version, links.version) {
        eprintln!("stream-recorder: {warning}");
        status(warning);
    }

    let rows = ledger::load_rows(&session.root)?;
    status("Asking Buffer for connected channels…".to_string());
    let client = BufferClient::from_env()?;
    let channels = client.channels().context("listing buffer channels")?;
    status(format!("{} channel(s) connected to Buffer.", channels.len()));

    let project = project_name(session);
    let dir = session.schedule_dir();
    let youtube_category = crate::config::load().youtube_category_id;
    let mut built = plan::build_plan(
        &posts,
        &links,
        &channels,
        &rows,
        &project,
        session.version,
        &youtube_category,
    );

    // A re-plan must not silently discard review work, and must not silently keep
    // a tick that belongs to copy nobody has read. `carry_approvals` matches on the
    // copy hash, so identical captions stay approved and edited ones come back for review.
    if let Ok(prior) = load_plan(&dir) {
        approve::carry_approvals(&mut built, &prior);
        let kept = built.items.iter().filter(|item| item.approved).count();
        if kept > 0 {
            status(format!("Carried {kept} approval(s) forward from the last plan."));
        }
    }

    let ready = built.queueable().count();
    let approved = built.sendable().count();
    let skipped = built.items.len() - ready;
    status(format!(
        "Planned {ready} ready / {skipped} skipped — {approved} approved."
    ));

    let path = save_plan(&dir, &built)?;
    Ok((path, built))
}

fn run_queue(session: &Session, tx: &Sender<ScheduleEvent>) -> Result<QueueOutcome> {
    let status = |msg: String| {
        let _ = tx.send(ScheduleEvent::Status(msg));
    };

    let dir = session.schedule_dir();
    let mut saved = load_plan(&dir).context("no saved plan — press Build Plan first")?;
    let mut outcome = QueueOutcome {
        ledger: session.root.join(SCHEDULE_JSONL),
        ..QueueOutcome::default()
    };
    // Approved *and* unblocked. An unticked item is not "not yet done" — it is a
    // post a human declined to send, so Queue must never reach for it.
    let targets: Vec<usize> = saved
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.approved && item.skip.is_none())
        .map(|(index, _)| index)
        .collect();
    if targets.is_empty() {
        let ready = saved.queueable().count();
        status(if ready > 0 {
            format!("{ready} item(s) ready but none approved — tick the ones to send.")
        } else {
            "Nothing queueable in the saved plan — press Build Plan first.".to_string()
        });
        return Ok(outcome);
    }

    // The ledger is the only record of what actually reached Buffer. Reading it
    // here is what makes a second Queue press (or a double click) a no-op instead
    // of a duplicate post on every connected channel.
    let mut rows = ledger::load_rows(&session.root)?;
    let client = BufferClient::from_env()?;
    status(format!("Queueing {} post(s) to Buffer…", targets.len()));

    let project = project_name(session);
    let version = saved.version;
    for (step, index) in targets.iter().enumerate() {
        let Some(item) = saved.items.get(*index).cloned() else {
            continue;
        };
        let label = format!("{} → {}", item.video_id, item.platform);
        status(format!("[{}/{}] {label}…", step + 1, targets.len()));

        if let Some(row) = ledger::queued_row(&rows, &item.video_id, &item.platform, &item.copy_hash)
        {
            outcome.already += 1;
            let mark = format!("already queued {} on {}", row.buffer_post_id, row.queued_at);
            status(format!("{label} {mark} — skipped."));
            skip_item(&mut saved, *index, mark);
            continue;
        }

        // One item's failure is a report, not an abort: the rest still queue.
        match client.create_post(&post_input(&item)) {
            Ok(post) => {
                outcome.queued += 1;
                let row = row_for(&item, &post.id, &project, version);
                match ledger::append_row(&session.root, &row) {
                    Ok(()) => {
                        let mark = format!("queued {} on {}", row.buffer_post_id, row.queued_at);
                        status(format!("{label} {mark}."));
                        skip_item(&mut saved, *index, mark);
                        rows.push(row);
                    }
                    Err(err) => {
                        // Live at Buffer, absent from the ledger: the next plan
                        // cannot know. Stderr keeps it after the status bar moves on.
                        eprintln!(
                            "stream-recorder: {label} queued as {} but NOT recorded in {}: {err:#}",
                            post.id,
                            outcome.ledger.display()
                        );
                        outcome.unrecorded.push(format!("{label} = {}", post.id));
                        let mark = format!("queued {} but NOT recorded — do not re-queue", post.id);
                        status(format!("{label} {mark}: {err:#}"));
                        skip_item(&mut saved, *index, mark);
                    }
                }
            }
            Err(err) => {
                // No ledger row, so a retry is safe — but a timeout can hide a post
                // that did land, so the retry has to be a deliberate Build Plan
                // rather than an automatic second send.
                outcome.failed += 1;
                status(format!("{label} failed: {err:#}"));
                skip_item(&mut saved, *index, format!("failed: {err:#} — Build Plan to retry"));
            }
        }
    }

    // Write the plan back so the tab — and a second press — see what is left. A
    // failure here is cosmetic (the ledger already guards against re-posting) and
    // must not be reported as "Queue failed" when the posts are live.
    if let Err(err) = save_plan(&dir, &saved) {
        eprintln!("stream-recorder: queued posts but could not rewrite the plan: {err:#}");
        status(format!("Queued, but the plan file was not updated: {err:#}"));
    }
    Ok(outcome)
}

/// posts.json and links.json are both read from version-scoped directories, so a
/// version that disagrees with the session means one of them is stale — and pairing
/// v2 captions with v3 videos is both silent and public. Warn, do not block: the
/// human reviewing the plan is the one who can tell whether it matters.
fn version_mismatch(session: Option<u32>, posts: Option<u32>, links: u32) -> Option<String> {
    let expected = session?;
    let mut stale = Vec::new();
    if matches!(posts, Some(version) if version != expected) {
        stale.push(format!("posts.json says v{}", posts.unwrap_or_default()));
    }
    if links != expected {
        stale.push(format!("links.json says v{links}"));
    }
    if stale.is_empty() {
        return None;
    }
    Some(format!("Warning: session is v{expected} but {}.", stale.join(" and ")))
}

/// Records on the saved plan what happened to one item, so it is neither offered
/// again nor silently dropped from the review surface.
fn skip_item(plan: &mut SchedulePlan, index: usize, reason: String) {
    if let Some(item) = plan.items.get_mut(index) {
        item.skip = Some(reason);
    }
}

/// The folder, matching distribute. Every row in `schedule.jsonl` is keyed by
/// this, and those rows are what stop an already-queued post being sent to
/// Buffer twice — so it has to be the identity a rename cannot move.
fn project_name(session: &Session) -> String {
    session.folder()
}

#[cfg(test)]
mod tests {
    use super::schema::PlanItem;
    use super::*;

    fn item(platform: &str, title: Option<&str>) -> PlanItem {
        PlanItem {
            video_id: "chapter-01".into(),
            platform: platform.into(),
            channel_id: "6a3dbb795ab6d2f10671b945".into(),
            channel_name: "saagasocials".into(),
            url: "https://cdn.example.com/vertical/chapter-01.mp4".into(),
            text: "body copy\n\n#rust".into(),
            title: title.map(str::to_string),
            mode: "addToQueue".into(),
            scheduling_type: "notification".into(),
            needs_approval: false,
            image: false,
            metadata: meta::metadata_for(platform, title, "28"),
            reason: "vertical chapter".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "0badc0de0badc0de".into(),
            skip: Option::None,
            approved: true,
        }
    }

    fn plan_with(items: Vec<PlanItem>) -> SchedulePlan {
        SchedulePlan { project: "vd-42-demo".into(), version: Some(3), items }
    }

    #[test]
    fn skip_item_marks_only_the_named_item() {
        let mut plan = plan_with(vec![item("instagram", None), item("tiktok", None)]);
        skip_item(&mut plan, 1, "queued post-3".into());
        assert!(plan.items[0].skip.is_none());
        assert_eq!(plan.items[1].skip.as_deref(), Some("queued post-3"));
        assert_eq!(plan.queueable().count(), 1);
        // An index past the end is a no-op, not a panic.
        skip_item(&mut plan, 9, "ignored".into());
        assert_eq!(plan.queueable().count(), 1);
    }

    #[test]
    fn the_summary_names_every_non_zero_count() {
        let mut outcome = QueueOutcome { queued: 12, ..QueueOutcome::default() };
        assert_eq!(outcome.summary(), "Queued 12 post(s)");
        outcome.failed = 2;
        outcome.already = 3;
        outcome.unrecorded.push("chapter-01 → tiktok = post-9".into());
        assert_eq!(
            outcome.summary(),
            "Queued 12 post(s), 2 failed, 3 already queued, 1 NOT recorded in the ledger"
        );
    }

    #[test]
    fn a_stale_posts_or_links_version_is_called_out_by_name() {
        assert_eq!(version_mismatch(Some(3), Some(3), 3), None);
        assert_eq!(
            version_mismatch(Some(3), Some(2), 3).as_deref(),
            Some("Warning: session is v3 but posts.json says v2.")
        );
        assert_eq!(
            version_mismatch(Some(3), Some(2), 1).as_deref(),
            Some("Warning: session is v3 but posts.json says v2 and links.json says v1.")
        );
        // An unversioned session (takes flat in drafts/) has nothing to disagree with.
        assert_eq!(version_mismatch(None, Some(2), 1), None);
        // A manifest with no version predates the field; that is not a mismatch.
        assert_eq!(version_mismatch(Some(3), None, 3), None);
    }

    /// A ledger row must stay addressable after the project is renamed, so this
    /// reads the folder and never the display name.
    #[test]
    fn project_name_is_the_root_folder() {
        let mut session = Session {
            root: PathBuf::from("/tmp/rec/2026-08-15_02-19-20"),
            dir: PathBuf::from("/tmp/rec/2026-08-15_02-19-20/drafts/v1"),
            version: Some(1),
        };
        assert_eq!(project_name(&session), "2026-08-15_02-19-20");
        session.root = PathBuf::from("/");
        assert_eq!(project_name(&session), "session");
    }
}
