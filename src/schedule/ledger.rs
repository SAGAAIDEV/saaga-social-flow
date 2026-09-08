//! Append-only ledger of Buffer queue results, plus the timestamp helpers it needs.
//!
//! Each `ScheduleRow` is appended to `schedule.jsonl` immediately after its own
//! `createPost` succeeds, so a run that dies halfway still leaves a truthful record
//! of exactly what reached Buffer. Nothing is buffered in memory and flushed at the end.
//!
//! Timestamps are RFC3339 UTC produced without a date crate: `rfc3339_from_unix` is a
//! pure function over epoch seconds using Howard Hinnant's civil-from-days algorithm.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use super::schema::{ScheduleRow, SCHEDULE_JSONL};

/// Append a single queued post to `root/schedule.jsonl`, creating the file (and its
/// parent directory) when missing. Called once per successful `createPost`.
#[tracing::instrument(skip(row), fields(video_id = %row.video_id, platform = %row.platform, post = %row.buffer_post_id))]
pub fn append_row(root: &Path, row: &ScheduleRow) -> Result<()> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let path = root.join(SCHEDULE_JSONL);
    let line = serde_json::to_string(row)
        .with_context(|| format!("serializing schedule row {}", row.id))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// Read every row we have ever queued. A *missing* ledger is not an error — it just
/// means nothing has been queued for this project yet. Any other read failure is:
/// these projects live on a Google Drive mount, and an unreadable ledger that
/// degraded to "no rows" would silently disable dedupe and re-post the whole back
/// catalogue. Unparseable lines are skipped with a warning naming the line number,
/// so corruption stays visible.
pub fn load_rows(root: &Path) -> Result<Vec<ScheduleRow>> {
    let path = root.join(SCHEDULE_JSONL);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "reading {} — refusing to queue without the ledger",
                    path.display()
                )
            })
        }
    };
    Ok(parse_rows(&text, &path.display().to_string()))
}

fn parse_rows(text: &str, label: &str) -> Vec<ScheduleRow> {
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<ScheduleRow>(trimmed) {
            Ok(row) => rows.push(row),
            Err(err) => eprintln!("{label}:{}: skipping unparseable row: {err}", index + 1),
        }
    }
    rows
}

/// The most recent row carrying this exact copy for this video + platform, i.e. proof
/// that this item is already live at Buffer. The copy hash is part of the key so an
/// edited caption is a new, queueable item. Both the planner (to mark the item
/// skipped) and the queue itself (so a second press cannot double-post) ask this.
/// A deleted post is not live, so the newest row winning is what makes clearing
/// the queue restore the item rather than bury it — see [`ScheduleRow::deleted_at`].
pub fn queued_row<'a>(
    rows: &'a [ScheduleRow],
    video_id: &str,
    platform: &str,
    copy_hash: &str,
) -> Option<&'a ScheduleRow> {
    rows.iter()
        .rev()
        .find(|row| matches(row, video_id, platform) && row.copy_hash == copy_hash)
        .filter(|row| row.deleted_at.is_none())
}

/// The most recent row for this video + platform whatever the copy. Regenerating the
/// posts changes the hash, which makes the item queueable again *by design* — this is
/// how the plan can still warn that the same video is already live under other words.
pub fn prior_row<'a>(
    rows: &'a [ScheduleRow],
    video_id: &str,
    platform: &str,
) -> Option<&'a ScheduleRow> {
    rows.iter()
        .rev()
        .find(|row| matches(row, video_id, platform))
        .filter(|row| row.deleted_at.is_none())
}

/// Every post this project still has live at Buffer: the newest row per key, minus
/// the ones already tombstoned. This is exactly what a project-scoped clear deletes.
pub fn live_rows(rows: &[ScheduleRow]) -> Vec<&ScheduleRow> {
    let mut seen = std::collections::BTreeSet::new();
    let mut live = Vec::new();
    for row in rows.iter().rev() {
        if !seen.insert(row.id.clone()) {
            continue;
        }
        if row.deleted_at.is_none() {
            live.push(row);
        }
    }
    live.reverse();
    live
}

fn matches(row: &ScheduleRow, video_id: &str, platform: &str) -> bool {
    row.video_id == video_id && row.platform == platform
}

/// Current UTC time as RFC3339. A clock set before the epoch degrades to the epoch
/// rather than failing the queue write.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// Epoch seconds -> `"YYYY-MM-DDTHH:MM:SSZ"`. Pure, no dependencies, correct for any
/// date the proleptic Gregorian calendar covers (well past 2030).
pub fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month, day)
/// in the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap day lands at the end of the era.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era, [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365], March-based
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + i64::from(m <= 2), m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-schedule-ledger-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn row(video_id: &str, platform: &str, copy_hash: &str) -> ScheduleRow {
        ScheduleRow {
            id: format!("{video_id}:{platform}:{copy_hash}"),
            buffer_post_id: "post-1".into(),
            project: "vd-42-demo".into(),
            version: Some(2),
            video_id: video_id.into(),
            platform: platform.into(),
            channel_id: "6a3dbb795ab6d2f10671b945".into(),
            url: "https://example.com/vertical/chapter-01.mp4".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: copy_hash.into(),
            queued_at: "2026-08-14T23:12:04Z".into(),
            deleted_at: None,
        }
    }

    #[test]
    fn epoch_formats_as_unix_zero() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn modern_epoch_converts_to_utc() {
        // 1_755_000_000 = 20312 days (2025-08-12) + 43_200 seconds (12:00:00).
        assert_eq!(rfc3339_from_unix(1_755_000_000), "2025-08-12T12:00:00Z");
    }

    #[test]
    fn leap_day_round_trips() {
        assert_eq!(rfc3339_from_unix(1_709_164_800), "2024-02-29T00:00:00Z");
        // One second before, and the last second of that leap day.
        assert_eq!(rfc3339_from_unix(1_709_164_799), "2024-02-28T23:59:59Z");
        assert_eq!(rfc3339_from_unix(1_709_251_199), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn dates_past_2030_are_correct() {
        assert_eq!(rfc3339_from_unix(2_082_758_400), "2036-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(4_102_444_800), "2100-01-01T00:00:00Z");
    }

    #[test]
    fn now_is_a_plausible_rfc3339_stamp() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "unexpected stamp {now}");
        assert!(now.ends_with('Z'), "unexpected stamp {now}");
        assert!(now.as_str() > "2020-01-01T00:00:00Z", "stale clock {now}");
    }

    #[test]
    fn append_then_load_returns_the_row() {
        let dir = temp_dir("append");
        let first = row("longform", "youtube", "abc123");
        let second = row("chapter-01", "tiktok", "def456");
        append_row(&dir, &first).expect("append first");
        append_row(&dir, &second).expect("append second");
        let rows = load_rows(&dir).expect("load");
        assert_eq!(rows, vec![first, second]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_ledger_loads_empty() {
        let dir = temp_dir("missing");
        assert!(load_rows(&dir)
            .expect("a missing ledger is not an error")
            .is_empty());
    }

    /// A ledger we cannot read must stop the queue, not degrade to "nothing was
    /// ever queued" — that silence is what re-posts a whole project.
    #[test]
    fn an_unreadable_ledger_is_an_error_not_an_empty_list() {
        let dir = temp_dir("unreadable");
        std::fs::create_dir_all(dir.join(SCHEDULE_JSONL)).expect("ledger path is a directory");
        let err = load_rows(&dir).expect_err("a directory is not a readable ledger");
        assert!(format!("{err:#}").contains("refusing to queue"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_line_is_skipped_and_good_lines_survive() {
        let dir = temp_dir("corrupt");
        let good = row("longform", "linkedin", "aaa111");
        append_row(&dir, &good).expect("append good");
        {
            let path = dir.join(SCHEDULE_JSONL);
            let mut file = OpenOptions::new().append(true).open(&path).expect("open");
            file.write_all(b"{not json at all\n").expect("write junk");
        }
        let tail = row("chapter-02", "instagram", "bbb222");
        append_row(&dir, &tail).expect("append tail");

        let rows = load_rows(&dir).expect("load");
        assert_eq!(rows, vec![good, tail]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blank_lines_are_ignored() {
        let good = row("longform", "twitter", "ccc333");
        let text = format!("\n{}\n\n", serde_json::to_string(&good).unwrap());
        assert_eq!(parse_rows(&text, "test.jsonl"), vec![good]);
    }

    #[test]
    fn queued_row_matches_the_triple() {
        let rows = vec![
            row("longform", "youtube", "abc123"),
            row("chapter-01", "tiktok", "def456"),
        ];
        assert!(queued_row(&rows, "longform", "youtube", "abc123").is_some());
        assert!(queued_row(&rows, "chapter-01", "tiktok", "def456").is_some());
        // A rewritten caption changes the hash, so it is queueable again.
        assert!(queued_row(&rows, "longform", "youtube", "zzz999").is_none());
        // Same copy, different platform or video is a different item.
        assert!(queued_row(&rows, "longform", "tiktok", "abc123").is_none());
        assert!(queued_row(&rows, "chapter-02", "youtube", "abc123").is_none());
        assert!(queued_row(&[], "longform", "youtube", "abc123").is_none());
    }

    #[test]
    fn queued_row_returns_the_most_recent_match() {
        let mut newer = row("longform", "youtube", "abc123");
        newer.buffer_post_id = "post-2".into();
        newer.queued_at = "2026-09-01T08:00:00Z".into();
        let rows = vec![row("longform", "youtube", "abc123"), newer];
        let found = queued_row(&rows, "longform", "youtube", "abc123").expect("a match");
        assert_eq!(found.buffer_post_id, "post-2");
    }

    /// Regenerated copy is queueable again, but the earlier post is still live —
    /// the planner needs to find it so it can warn instead of silently duplicating.
    #[test]
    fn prior_row_finds_the_same_video_under_different_copy() {
        let rows = vec![row("longform", "youtube", "abc123")];
        assert!(queued_row(&rows, "longform", "youtube", "rewritten").is_none());
        let prior = prior_row(&rows, "longform", "youtube").expect("the earlier post");
        assert_eq!(prior.copy_hash, "abc123");
        assert_eq!(prior.buffer_post_id, "post-1");
        assert!(prior_row(&rows, "longform", "tiktok").is_none());
        assert!(prior_row(&rows, "chapter-01", "youtube").is_none());
    }
}
