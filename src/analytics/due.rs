//! Working out which samples are owed right now.
//!
//! Two facts force this design. First, `addToQueue` means Buffer decides when a
//! post goes out, so a window can only be measured from `sentAt` — never from
//! when we queued it. Second, this runs in a desktop app that is closed most of
//! the time, so nothing can fire "at exactly seven days".
//!
//! So sampling is **catch-up, not scheduling**: every pull asks "what is owed and
//! not yet collected?" and takes it. Close the app for a fortnight, open it, and
//! the week-old samples land late but land. A row's timestamps say when it was
//! actually taken, so lateness is visible rather than pretended away.

use crate::analytics::schema::{already_sampled, sent_at, AnalyticsRow, Window};
use crate::schedule::schema::ScheduleRow;

/// One sample to collect on this pull.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub buffer_post_id: String,
    pub window: Window,
    pub video_id: String,
    pub platform: String,
}

/// Everything owed at `now` (unix seconds), in ledger order.
///
/// A post with no recorded send is always due for a [`Window::Sent`] check — that
/// is how a queued post eventually gets its anchor. Only once that anchor exists
/// can the measuring windows come due.
pub fn due_now(ledger: &[ScheduleRow], samples: &[AnalyticsRow], now: u64) -> Vec<Due> {
    let mut out = Vec::new();
    for row in ledger {
        let Some(sent) = sent_at(samples, &row.buffer_post_id) else {
            // Not known to have gone out yet: re-check status.
            out.push(due_for(row, Window::Sent));
            continue;
        };
        let Some(sent_unix) = parse_rfc3339(sent) else {
            eprintln!(
                "stream-recorder: unparseable sentAt {sent:?} for {}",
                row.buffer_post_id
            );
            continue;
        };
        for window in Window::samples() {
            if now >= sent_unix.saturating_add(window.after_secs())
                && !already_sampled(samples, &row.buffer_post_id, window)
            {
                out.push(due_for(row, window));
            }
        }
    }
    out
}

fn due_for(row: &ScheduleRow, window: Window) -> Due {
    Due {
        buffer_post_id: row.buffer_post_id.clone(),
        window,
        video_id: row.video_id.clone(),
        platform: row.platform.clone(),
    }
}

/// Parses the timestamps Buffer emits into unix seconds.
///
/// Tolerant on purpose: accepts `Z`, a numeric offset, and fractional seconds,
/// because the exact shape is the server's choice and a parse failure here would
/// silently stall a post's whole sampling schedule.
pub fn parse_rfc3339(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, rest) = text.split_once('T').or_else(|| text.split_once(' '))?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;

    // Split the offset off before reading the clock.
    let (clock, offset_secs) = split_offset(rest)?;
    let mut clock = clock.split(':');
    let hour: u64 = clock.next()?.parse().ok()?;
    let minute: u64 = clock.next()?.parse().ok()?;
    let second: u64 = match clock.next() {
        // Fractional seconds are dropped rather than rounded: a sample window is
        // measured in days, so sub-second precision is noise.
        Some(sec) => sec.split('.').next()?.parse().ok()?,
        None => 0,
    };

    let days = days_from_civil(year, month, day);
    let stamp = days * 86_400 + (hour * 3_600 + minute * 60 + second) as i64 - offset_secs;
    u64::try_from(stamp).ok()
}

/// Returns the clock part and the offset in seconds east of UTC.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(clock) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        return Some((clock, 0));
    }
    // Look for a sign after the clock, e.g. "20:44:00+02:00".
    let at = rest.rfind(['+', '-'])?;
    let (clock, sign_and_offset) = rest.split_at(at);
    let sign = if sign_and_offset.starts_with('-') { -1 } else { 1 };
    let offset = &sign_and_offset[1..];
    let (hours, minutes) = match offset.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None if offset.len() == 4 => (
            offset[..2].parse::<i64>().ok()?,
            offset[2..].parse::<i64>().ok()?,
        ),
        None => (offset.parse::<i64>().ok()?, 0),
    };
    Some((clock, sign * (hours * 3_600 + minutes * 60)))
}

/// Days since 1970-01-01 (Howard Hinnant's civil algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = month as i64;
    let day = day as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::metrics::PostMetrics;

    const SENT: &str = "2026-08-09T20:44:00Z";
    /// 2026-08-09T20:44:00Z as unix seconds, cross-checked against Python's
    /// `datetime.fromisoformat(...).timestamp()`.
    const SENT_UNIX: u64 = 1_786_308_240;
    const DAY: u64 = 86_400;

    fn ledger(post_id: &str) -> ScheduleRow {
        ScheduleRow {
            id: format!("chapter-03:tiktok:{post_id}"),
            buffer_post_id: post_id.into(),
            project: "vd-42".into(),
            version: Some(3),
            video_id: "chapter-03".into(),
            platform: "tiktok".into(),
            channel_id: "chan".into(),
            url: "https://example.com/v.mp4".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "abcd".into(),
            queued_at: "2026-08-01T10:00:00Z".into(),
            deleted_at: None,
        }
    }

    fn sent_row(post_id: &str) -> AnalyticsRow {
        let observed = PostMetrics {
            post_id: post_id.into(),
            status: "sent".into(),
            sent_at: Some(SENT.into()),
            metrics_updated_at: None,
            metrics: Vec::new(),
        };
        AnalyticsRow::new(&ledger(post_id), Window::Sent, &observed, SENT.into())
    }

    fn sample(post_id: &str, window: Window) -> AnalyticsRow {
        let observed = PostMetrics {
            post_id: post_id.into(),
            status: "sent".into(),
            sent_at: Some(SENT.into()),
            metrics_updated_at: None,
            metrics: Vec::new(),
        };
        AnalyticsRow::new(&ledger(post_id), window, &observed, SENT.into())
    }

    #[test]
    fn the_reference_timestamp_parses_to_the_expected_epoch() {
        assert_eq!(parse_rfc3339(SENT), Some(SENT_UNIX));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-03-01T00:00:00Z"), Some(951_868_800));
        // A leap day must not shift the year.
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1_709_208_000));
    }

    #[test]
    fn offsets_and_fractional_seconds_are_understood() {
        let utc = parse_rfc3339("2026-08-09T20:44:00Z").unwrap();
        assert_eq!(parse_rfc3339("2026-08-09T20:44:00.123Z"), Some(utc));
        assert_eq!(parse_rfc3339("2026-08-09T22:44:00+02:00"), Some(utc));
        assert_eq!(parse_rfc3339("2026-08-09T14:44:00-06:00"), Some(utc));
        assert_eq!(parse_rfc3339("2026-08-09T22:44:00+0200"), Some(utc));
    }

    #[test]
    fn nonsense_timestamps_are_rejected_rather_than_guessed() {
        assert_eq!(parse_rfc3339(""), None);
        assert_eq!(parse_rfc3339("not a date"), None);
        assert_eq!(parse_rfc3339("2026-08-09"), None);
        assert_eq!(parse_rfc3339("2026-08-09Txx:44:00Z"), None);
    }

    /// A queued post has no anchor yet, so the only thing owed is the status check.
    #[test]
    fn an_unsent_post_is_due_only_for_its_send_check() {
        let due = due_now(&[ledger("post-9")], &[], SENT_UNIX + 40 * DAY);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].window, Window::Sent);
        assert_eq!(due[0].buffer_post_id, "post-9");
    }

    #[test]
    fn nothing_is_owed_before_the_first_window_matures() {
        let samples = vec![sent_row("post-9")];
        assert!(due_now(&[ledger("post-9")], &samples, SENT_UNIX + DAY).is_empty());
        assert!(due_now(&[ledger("post-9")], &samples, SENT_UNIX + 6 * DAY).is_empty());
    }

    #[test]
    fn the_week_sample_comes_due_exactly_seven_days_after_sending() {
        let samples = vec![sent_row("post-9")];
        let due = due_now(&[ledger("post-9")], &samples, SENT_UNIX + 7 * DAY);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].window, Window::Week);
    }

    /// The catch-up property: away for six weeks, both samples still land.
    #[test]
    fn a_long_absence_collects_every_owed_window_at_once() {
        let samples = vec![sent_row("post-9")];
        let due = due_now(&[ledger("post-9")], &samples, SENT_UNIX + 42 * DAY);
        let windows: Vec<_> = due.iter().map(|d| d.window).collect();
        assert_eq!(windows, vec![Window::Week, Window::Month]);
    }

    #[test]
    fn a_collected_window_is_never_collected_twice() {
        let samples = vec![sent_row("post-9"), sample("post-9", Window::Week)];
        let due = due_now(&[ledger("post-9")], &samples, SENT_UNIX + 42 * DAY);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].window, Window::Month, "only the month is still owed");

        let done = vec![
            sent_row("post-9"),
            sample("post-9", Window::Week),
            sample("post-9", Window::Month),
        ];
        assert!(due_now(&[ledger("post-9")], &done, SENT_UNIX + 400 * DAY).is_empty());
    }

    #[test]
    fn posts_are_tracked_independently() {
        let samples = vec![sent_row("post-9"), sample("post-9", Window::Week)];
        let ledger = [ledger("post-9"), ledger("post-1")];

        // At eight days post-9 owes nothing (week taken, month not due) and post-1
        // has no anchor at all, so the only thing owed is post-1's send check.
        let early = due_now(&ledger, &samples, SENT_UNIX + 8 * DAY);
        assert_eq!(early.len(), 1);
        assert_eq!(early[0].buffer_post_id, "post-1");
        assert_eq!(early[0].window, Window::Sent);

        // Past thirty days post-9's month matures, independently of post-1.
        let later = due_now(&ledger, &samples, SENT_UNIX + 31 * DAY);
        assert_eq!(later.len(), 2);
        assert_eq!(later[0].buffer_post_id, "post-9");
        assert_eq!(later[0].window, Window::Month);
        assert_eq!(later[1].buffer_post_id, "post-1");
        assert_eq!(later[1].window, Window::Sent);
    }

    /// A send time we cannot read must not silently reschedule forever.
    #[test]
    fn an_unparseable_send_time_is_skipped_loudly_not_retried() {
        let mut broken = sent_row("post-9");
        broken.sent_at = Some("whenever".into());
        assert!(due_now(&[ledger("post-9")], &[broken], SENT_UNIX + 42 * DAY).is_empty());
    }
}
