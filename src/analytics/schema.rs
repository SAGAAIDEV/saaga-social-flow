//! The analytics ledger: one row per (post, window) sample.
//!
//! Lives at `{root}/analytics.jsonl`, beside `schedule.jsonl` and outside any
//! version, because a post queued from v2 is still being measured while v3 is
//! being cut. Append-only and deduped on `(buffer_post_id, window)`, so a pull
//! that runs twice — or runs after the app was closed for a fortnight — collects
//! each sample exactly once.
//!
//! Every row carries the ledger's identity fields (`video_id`, `platform`,
//! `prompt_id`, `copy_hash`) copied in at sample time. That denormalisation is
//! deliberate: the report must stay readable even if the plan is rebuilt or the
//! copy is regenerated underneath it.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::schedule::metrics::{MetricValue, PostMetrics};
use crate::schedule::schema::ScheduleRow;

pub const ANALYTICS_JSONL: &str = "analytics.jsonl";

/// When a sample was taken, relative to the post actually going out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// Not a measurement — the observation that Buffer sent it, and when.
    Sent,
    Week,
    Month,
}

impl Window {
    pub fn as_str(self) -> &'static str {
        match self {
            Window::Sent => "sent",
            Window::Week => "7d",
            Window::Month => "30d",
        }
    }

    /// How long after `sentAt` this sample is due.
    pub fn after_secs(self) -> u64 {
        match self {
            Window::Sent => 0,
            Window::Week => 7 * 86_400,
            Window::Month => 30 * 86_400,
        }
    }

    /// The measuring windows, in order. Excludes [`Window::Sent`].
    pub fn samples() -> [Window; 2] {
        [Window::Week, Window::Month]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsRow {
    /// Join key back to `schedule.jsonl`.
    pub buffer_post_id: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub video_id: String,
    pub platform: String,
    /// "sent" | "7d" | "30d"
    pub window: String,
    /// When Buffer actually published it — the anchor every window is measured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    pub pulled_at: String,
    /// Buffer refreshes metrics on its own cadence; without this a stale zero is
    /// indistinguishable from a real one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_updated_at: Option<String>,
    #[serde(default)]
    pub metrics: Vec<MetricValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_hash: Option<String>,
}

impl AnalyticsRow {
    /// Builds a row from what the ledger knows plus what Buffer just returned.
    pub fn new(
        ledger: &ScheduleRow,
        window: Window,
        observed: &PostMetrics,
        pulled_at: String,
    ) -> AnalyticsRow {
        AnalyticsRow {
            buffer_post_id: ledger.buffer_post_id.clone(),
            project: ledger.project.clone(),
            version: ledger.version,
            video_id: ledger.video_id.clone(),
            platform: ledger.platform.clone(),
            window: window.as_str().to_string(),
            sent_at: observed.sent_at.clone(),
            pulled_at,
            metrics_updated_at: observed.metrics_updated_at.clone(),
            metrics: observed.metrics.clone(),
            prompt_id: Some(ledger.prompt_id.clone()),
            copy_hash: Some(ledger.copy_hash.clone()),
        }
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.metrics
            .iter()
            .find(|metric| metric.name.eq_ignore_ascii_case(name))
            .map(|metric| metric.value)
    }
}

pub fn append_row(root: &Path, row: &AnalyticsRow) -> Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let path = root.join(ANALYTICS_JSONL);
    let line = serde_json::to_string(row).context("serializing analytics row")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

/// Reads every sample. A missing file is an empty history, not an error — but an
/// unreadable one is, because silently reporting "no data" would read as "nothing
/// performed" and would also re-pull every sample already taken.
pub fn load_rows(root: &Path) -> Result<Vec<AnalyticsRow>> {
    let path = root.join(ANALYTICS_JSONL);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("reading {}", path.display()));
        }
    };
    Ok(parse_rows(&text))
}

fn parse_rows(text: &str) -> Vec<AnalyticsRow> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(n, line)| match serde_json::from_str(line) {
            Ok(row) => Some(row),
            Err(err) => {
                eprintln!("stream-recorder: skipping analytics.jsonl line {}: {err}", n + 1);
                None
            }
        })
        .collect()
}

/// True when this exact sample has already been taken.
pub fn already_sampled(rows: &[AnalyticsRow], post_id: &str, window: Window) -> bool {
    rows.iter()
        .any(|row| row.buffer_post_id == post_id && row.window == window.as_str())
}

/// The recorded send time for a post, if we have observed it going out.
pub fn sent_at<'a>(rows: &'a [AnalyticsRow], post_id: &str) -> Option<&'a str> {
    rows.iter()
        .find(|row| row.buffer_post_id == post_id && row.window == Window::Sent.as_str())
        .and_then(|row| row.sent_at.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_row(post_id: &str, video: &str, platform: &str) -> ScheduleRow {
        ScheduleRow {
            id: format!("{video}:{platform}:abcd"),
            buffer_post_id: post_id.into(),
            project: "vd-42".into(),
            version: Some(3),
            video_id: video.into(),
            platform: platform.into(),
            channel_id: "chan".into(),
            url: "https://example.com/v.mp4".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "abcd".into(),
            queued_at: "2026-08-01T10:00:00Z".into(),
            deleted_at: None,
        }
    }

    fn observed() -> PostMetrics {
        PostMetrics {
            post_id: "post-9".into(),
            status: "sent".into(),
            sent_at: Some("2026-08-09T20:44:00Z".into()),
            metrics_updated_at: Some("2026-08-16T06:00:00Z".into()),
            metrics: vec![MetricValue {
                name: "impressions".into(),
                kind: "impressions".into(),
                unit: "count".into(),
                value: 8120.0,
            }],
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-analytics-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// These strings are persisted in analytics.jsonl, so changing one silently
    /// orphans every sample already taken under the old name.
    #[test]
    fn window_names_and_maturities_are_fixed() {
        assert_eq!(Window::Sent.as_str(), "sent");
        assert_eq!(Window::Week.as_str(), "7d");
        assert_eq!(Window::Month.as_str(), "30d");
        assert_eq!(Window::samples(), [Window::Week, Window::Month]);
        assert_eq!(Window::Week.after_secs(), 604_800);
        assert_eq!(Window::Month.after_secs(), 2_592_000);
        assert_eq!(Window::Sent.after_secs(), 0);
    }

    /// The report has to survive a re-plan, so identity is copied in at sample time.
    #[test]
    fn a_row_carries_the_ledgers_identity_fields() {
        let row = AnalyticsRow::new(
            &ledger_row("post-9", "chapter-03", "tiktok"),
            Window::Week,
            &observed(),
            "2026-08-16T09:00:00Z".into(),
        );
        assert_eq!(row.buffer_post_id, "post-9");
        assert_eq!(row.video_id, "chapter-03");
        assert_eq!(row.platform, "tiktok");
        assert_eq!(row.window, "7d");
        assert_eq!(row.prompt_id.as_deref(), Some("posts.social"));
        assert_eq!(row.copy_hash.as_deref(), Some("abcd"));
        assert_eq!(row.sent_at.as_deref(), Some("2026-08-09T20:44:00Z"));
        assert_eq!(row.get("impressions"), Some(8120.0));
    }

    #[test]
    fn rows_round_trip_one_per_line() {
        let dir = temp("append");
        let row = AnalyticsRow::new(
            &ledger_row("post-9", "chapter-03", "tiktok"),
            Window::Week,
            &observed(),
            "2026-08-16T09:00:00Z".into(),
        );
        append_row(&dir, &row).unwrap();
        append_row(&dir, &row).unwrap();
        let text = std::fs::read_to_string(dir.join(ANALYTICS_JSONL)).unwrap();
        assert_eq!(text.lines().count(), 2);
        let back = load_rows(&dir).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], row);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_ledger_is_empty_but_an_unreadable_one_is_an_error() {
        let dir = temp("missing");
        assert!(load_rows(&dir).unwrap().is_empty());
        // A directory where the file should be cannot be read as history.
        std::fs::create_dir_all(dir.join(ANALYTICS_JSONL)).unwrap();
        assert!(load_rows(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_line_is_skipped_and_the_rest_survive() {
        let good = serde_json::to_string(&AnalyticsRow::new(
            &ledger_row("post-9", "chapter-03", "tiktok"),
            Window::Week,
            &observed(),
            "2026-08-16T09:00:00Z".into(),
        ))
        .unwrap();
        let rows = parse_rows(&format!("{good}\n{{ truncated\n{good}\n"));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn sampling_is_tracked_per_window_not_per_post() {
        let rows = vec![AnalyticsRow::new(
            &ledger_row("post-9", "chapter-03", "tiktok"),
            Window::Week,
            &observed(),
            "2026-08-16T09:00:00Z".into(),
        )];
        assert!(already_sampled(&rows, "post-9", Window::Week));
        assert!(!already_sampled(&rows, "post-9", Window::Month));
        assert!(!already_sampled(&rows, "post-1", Window::Week));
    }

    #[test]
    fn the_send_time_comes_from_the_sent_observation() {
        let mut sent = AnalyticsRow::new(
            &ledger_row("post-9", "chapter-03", "tiktok"),
            Window::Sent,
            &observed(),
            "2026-08-09T21:00:00Z".into(),
        );
        sent.metrics.clear();
        let rows = vec![sent];
        assert_eq!(sent_at(&rows, "post-9"), Some("2026-08-09T20:44:00Z"));
        assert_eq!(sent_at(&rows, "post-1"), None);
    }
}
