//! Serde types and on-disk format for the schedule stage.
//!
//! Two artifacts, in two different places on purpose.
//! `schedule/vN/schedule.json` holds the reviewable [`SchedulePlan`] (what *would* be
//! queued to Buffer) and is rewritten every plan, so it is version-scoped.
//! `{root}/schedule.jsonl` is the append-only ledger of [`ScheduleRow`]s (what *was*
//! queued); it sits at the project root, outside any version, because dedupe has to
//! span versions — v3 must not re-post what v2 already sent. The ledger writer lives
//! in `super::ledger`; this module only owns the shapes and the plan read/write helpers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEDULE_JSON: &str = "schedule.json";
pub const SCHEDULE_JSONL: &str = "schedule.jsonl";

/// One prospective Buffer post: a single video on a single channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    /// "longform" | "chapter-01" ... — matches the ids in posts.json and links.json.
    pub video_id: String,
    /// "youtube" | "instagram" | "tiktok" | "linkedin" | "twitter" | "bluesky" | "facebook" | "youtube_shorts".
    pub platform: String,
    /// Resolved Buffer channel id; empty string when no channel matched.
    pub channel_id: String,
    pub channel_name: String,
    /// Public S3 url from links.json; empty string when the asset is missing.
    pub url: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Buffer ShareMode — always "addToQueue".
    pub mode: String,
    /// Buffer SchedulingType — "automatic" | "notification".
    pub scheduling_type: String,
    pub needs_approval: bool,
    #[serde(default)]
    pub image: bool,
    /// The exact `PostInputMetaData` object Queue will send, e.g.
    /// `{"youtube": {"privacy": "public", "notifySubscribers": true, …}}`. It lives in
    /// the plan so a human can see the irreversible half of the payload — public vs
    /// private, whether every subscriber gets pinged — *before* pressing Queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Why this item exists, e.g. "hub video" / "vertical chapter".
    pub reason: String,
    /// Prompt that produced `text`, e.g. "posts.social".
    pub prompt_id: String,
    /// Which version of that prompt — carried from the posts manifest so a post's
    /// performance can be attributed to a preamble rather than to a date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<u32>,
    pub copy_hash: String,
    /// `Some(reason)` means this item is NOT queueable (no channel, no url, already queued).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<String>,
    /// Ticked in the Schedule tab. Queue sends only approved items.
    ///
    /// Approval is bound to `copy_hash`: re-planning carries the tick forward only
    /// when the copy is byte-identical, so regenerating posts or hand-editing a
    /// caption drops it. Without that you could approve caption A in the preview
    /// and ship caption B to Buffer.
    #[serde(default)]
    pub approved: bool,
}

/// The full reviewable plan for one project version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulePlan {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub items: Vec<PlanItem>,
}

impl SchedulePlan {
    /// Items nothing is blocking — everything without a `skip` reason. These are
    /// *offerable*, not sendable: an unblocked item still needs a human tick.
    pub fn queueable(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|item| item.skip.is_none())
    }

    /// Exactly what Queue sends: unblocked *and* approved.
    pub fn sendable(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|item| item.approved && item.skip.is_none())
    }
}

/// One line of the append-only ledger: proof that a plan item reached Buffer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRow {
    /// Dedupe key, "{video_id}:{platform}:{copy_hash}" — see [`row_id`].
    pub id: String,
    pub buffer_post_id: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub video_id: String,
    pub platform: String,
    pub channel_id: String,
    pub url: String,
    pub prompt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<u32>,
    pub copy_hash: String,
    /// RFC3339 UTC, e.g. "2026-08-14T23:12:04Z".
    pub queued_at: String,
    /// Set on a *second* row with the same key, appended when the post is deleted
    /// from Buffer. The ledger stays append-only, so the original row still says
    /// this went out and when — the tombstone only says it is no longer live.
    ///
    /// Dedupe reads the newest row per key, so a tombstoned item is queueable
    /// again. Without it, clearing the queue would leave every post permanently
    /// un-repostable: the plan would keep skipping them as "already queued"
    /// against posts that no longer exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

impl ScheduleRow {
    /// The same row, marked as no longer live at Buffer.
    pub fn tombstone(&self, at: String) -> ScheduleRow {
        ScheduleRow {
            deleted_at: Some(at),
            ..self.clone()
        }
    }
}

/// The dedupe key shared by plan building and the ledger: "{video_id}:{platform}:{copy_hash}".
pub fn row_id(video_id: &str, platform: &str, copy_hash: &str) -> String {
    format!("{video_id}:{platform}:{copy_hash}")
}

pub fn save_plan(dir: &Path, plan: &SchedulePlan) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(SCHEDULE_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(plan).context("serializing schedule plan")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load_plan(dir: &Path) -> Result<SchedulePlan> {
    let path = dir.join(SCHEDULE_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(video_id: &str, platform: &str, skip: Option<&str>) -> PlanItem {
        PlanItem {
            video_id: video_id.into(),
            platform: platform.into(),
            channel_id: "6a3dbba45ab6d2f10671b9db".into(),
            channel_name: "SAAGA Solve".into(),
            url: "https://example.com/longform.mp4".into(),
            text: "New video is up".into(),
            title: Some("Chapter one".into()),
            mode: "addToQueue".into(),
            scheduling_type: "automatic".into(),
            needs_approval: false,
            image: false,
            metadata: Some(serde_json::json!({ "youtube": { "privacy": "public" } })),
            reason: "hub video".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "0badc0de0badc0de".into(),
            skip: skip.map(str::to_string),
            approved: false,
        }
    }

    fn plan() -> SchedulePlan {
        SchedulePlan {
            project: "vd-42-my-video".into(),
            version: Some(3),
            items: vec![
                item("longform", "youtube", None),
                item("chapter-01", "instagram", Some("no url")),
                item("chapter-01", "tiktok", None),
            ],
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-schedule-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn plan_round_trips() {
        let dir = temp_dir("round-trip");
        let plan = plan();
        let path = save_plan(&dir, &plan).expect("save");
        assert_eq!(path, dir.join(SCHEDULE_JSON));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.ends_with("}\n"), "pretty json ends with a newline");
        let back = load_plan(&dir).expect("load");
        assert_eq!(back, plan);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn optional_fields_are_omitted_when_empty() {
        let mut only = plan();
        only.version = None;
        only.items = vec![item("longform", "twitter", None)];
        only.items[0].title = None;
        only.items[0].metadata = None;
        let text = serde_json::to_string(&only).expect("serialize");
        assert!(!text.contains("\"version\""));
        assert!(!text.contains("\"title\""));
        assert!(!text.contains("\"metadata\""));
        assert!(!text.contains("\"skip\""));
        // Missing optionals still parse back to None.
        let back: SchedulePlan = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, only);
    }

    #[test]
    fn queueable_filters_skipped_items() {
        let plan = plan();
        let ids: Vec<_> = plan
            .queueable()
            .map(|item| (item.video_id.as_str(), item.platform.as_str()))
            .collect();
        assert_eq!(ids, vec![("longform", "youtube"), ("chapter-01", "tiktok")]);
        assert_eq!(plan.queueable().count(), 2);
    }

    /// The whole point of the gate: unblocked is not the same as sendable.
    #[test]
    fn sendable_needs_both_a_tick_and_no_skip() {
        let mut plan = plan();
        assert_eq!(plan.sendable().count(), 0, "nothing ships unapproved");
        // Tick every item, including the blocked one.
        for item in &mut plan.items {
            item.approved = true;
        }
        let shipped: Vec<_> = plan
            .sendable()
            .map(|item| (item.video_id.as_str(), item.platform.as_str()))
            .collect();
        assert_eq!(shipped, vec![("longform", "youtube"), ("chapter-01", "tiktok")]);
        assert_eq!(plan.queueable().count(), 2, "a tick cannot unblock a skip");
    }

    #[test]
    fn approval_survives_the_round_trip_and_defaults_off() {
        let dir = temp_dir("approval");
        let mut saved = plan();
        saved.items[0].approved = true;
        save_plan(&dir, &saved).expect("save");
        let back = load_plan(&dir).expect("load");
        assert!(back.items[0].approved);
        assert!(!back.items[2].approved);
        // A plan written before the gate existed reads back as unapproved.
        let legacy = serde_json::to_string(&saved).expect("serialize").replace(
            "\"approved\":true",
            "\"approved\":false",
        );
        let legacy: SchedulePlan = serde_json::from_str(&legacy).expect("parse");
        assert_eq!(legacy.sendable().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queueable_is_empty_when_everything_is_skipped() {
        let mut plan = plan();
        for item in &mut plan.items {
            item.skip = Some("already queued".into());
        }
        assert_eq!(plan.queueable().count(), 0);
    }

    #[test]
    fn row_id_joins_with_colons() {
        assert_eq!(
            row_id("chapter-01", "tiktok", "0badc0de0badc0de"),
            "chapter-01:tiktok:0badc0de0badc0de"
        );
    }

    /// The plan is the review surface, so the payload a human most needs to see —
    /// public, notify subscribers — has to survive the round trip verbatim.
    #[test]
    fn the_reviewable_metadata_round_trips() {
        let dir = temp_dir("metadata");
        let mut saved = plan();
        saved.items[0].metadata = Some(serde_json::json!({
            "youtube": { "privacy": "public", "notifySubscribers": true, "madeForKids": false }
        }));
        save_plan(&dir, &saved).expect("save");
        let back = load_plan(&dir).expect("load");
        let meta = back.items[0].metadata.as_ref().expect("metadata survived");
        assert_eq!(meta["youtube"]["privacy"], "public");
        assert_eq!(meta["youtube"]["notifySubscribers"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn row_round_trips() {
        let row = ScheduleRow {
            id: row_id("longform", "youtube", "deadbeef"),
            buffer_post_id: "abc123".into(),
            project: "vd-42-my-video".into(),
            version: Some(3),
            video_id: "longform".into(),
            platform: "youtube".into(),
            channel_id: "6a3dbba45ab6d2f10671b9db".into(),
            url: "https://example.com/longform.mp4".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "deadbeef".into(),
            queued_at: "2026-08-14T23:12:04Z".into(),
            deleted_at: None,
        };
        let line = serde_json::to_string(&row).expect("serialize");
        assert!(!line.contains('\n'), "a ledger row must fit on one line");
        let back: ScheduleRow = serde_json::from_str(&line).expect("parse");
        assert_eq!(back, row);
        assert_eq!(back.id, "longform:youtube:deadbeef");
    }
}
