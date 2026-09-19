//! Rendering the sampled ledger into a readable report.
//!
//! Columns are built from whatever metrics actually came back rather than a fixed
//! set, because each network emits its own subset of `PostMetricType` — a table
//! with a hardcoded `impressions` column would show blanks for TikTok and hide
//! `saves` entirely. Preferred names lead so the common columns stay in the same
//! order run to run; anything else follows alphabetically.

use std::collections::BTreeSet;

use crate::analytics::schema::{AnalyticsRow, Window};

/// Metrics worth leading with, in reading order. Anything else is appended.
const PREFERRED: [&str; 9] = [
    "impressions",
    "reach",
    "views",
    "likes",
    "comments",
    "shares",
    "saves",
    "clicks",
    "engagementRate",
];

/// `failed` is what the pull just saw in Buffer's `error` status, one line per
/// post. It is not in `rows` — an errored post has no sample — so it rides in
/// beside them, and leads the report: it is the only thing here a reader has to
/// act on rather than read.
pub fn render(
    rows: &[AnalyticsRow],
    failed: &[String],
    project: &str,
    generated_at: &str,
) -> String {
    let mut out = format!("# Analytics — {project}\n\n");
    push_failed(&mut out, failed);
    let posts: BTreeSet<&str> = rows.iter().map(|r| r.buffer_post_id.as_str()).collect();
    let measured = rows
        .iter()
        .filter(|r| r.window != Window::Sent.as_str())
        .count();
    out.push_str(&format!(
        "_{measured} sample(s) across {} post(s) · generated {generated_at}_\n",
        posts.len()
    ));

    if measured == 0 {
        out.push_str(
            "\nNothing measured yet. Samples are taken 7 and 30 days after Buffer \
             sends a post, so a freshly queued project has nothing to show until \
             its first posts go out.\n",
        );
        push_pending(&mut out, rows);
        return out;
    }

    for window in Window::samples() {
        let mut slice: Vec<&AnalyticsRow> = rows
            .iter()
            .filter(|row| row.window == window.as_str())
            .collect();
        if slice.is_empty() {
            continue;
        }
        // Best first, on whichever headline metric this set actually has.
        let headline = headline_metric(&slice);
        slice.sort_by(|a, b| {
            b.get(&headline)
                .unwrap_or(0.0)
                .partial_cmp(&a.get(&headline).unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        push_table(&mut out, window, &slice);
    }

    push_pending(&mut out, rows);
    out
}

fn push_table(out: &mut String, window: Window, rows: &[&AnalyticsRow]) {
    let heading = match window {
        Window::Week => "7 days after posting",
        Window::Month => "30 days after posting",
        Window::Sent => "sent",
    };
    out.push_str(&format!("\n## {heading}\n\n"));

    let columns = columns_for(rows);
    out.push_str("| video | platform | sent |");
    for column in &columns {
        out.push_str(&format!(" {column} |"));
    }
    out.push_str("\n|---|---|---|");
    for _ in &columns {
        out.push_str("---|");
    }
    out.push('\n');

    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} |",
            row.video_id,
            row.platform,
            row.sent_at.as_deref().map(short_date).unwrap_or("—")
        ));
        for column in &columns {
            match row.get(column) {
                Some(value) => out.push_str(&format!(" {} |", number(value, column))),
                // A blank is honest here: this network does not report this metric.
                None => out.push_str(" — |"),
            }
        }
        out.push('\n');
    }

    if let Some(stale) = stalest(rows) {
        out.push_str(&format!("\n_Metrics as of {stale}._\n"));
    }

    if let Some(best) = rows.first() {
        out.push_str(&format!(
            "\n**Best: {} on {}** — prompt `{}`, copy `{}`.\n",
            best.video_id,
            best.platform,
            best.prompt_id.as_deref().unwrap_or("—"),
            best.copy_hash.as_deref().unwrap_or("—")
        ));
    }
}

/// Posts Buffer gave up on. First, because nothing else in the report needs a
/// hand: these have to be retried or deleted in Buffer before they do anything.
fn push_failed(out: &mut String, failed: &[String]) {
    if failed.is_empty() {
        return;
    }
    out.push_str(&format!(
        "## Failed at Buffer — {} post(s) need a retry or a delete in Buffer\n\n",
        failed.len()
    ));
    for line in failed {
        out.push_str(&format!("- {line}\n"));
    }
    out.push('\n');
}

/// Posts that have sent but whose windows have not come due yet — so an empty
/// table reads as "too early" rather than "it flopped".
fn push_pending(out: &mut String, rows: &[AnalyticsRow]) {
    let mut pending: Vec<&AnalyticsRow> = rows
        .iter()
        .filter(|row| row.window == Window::Sent.as_str())
        .filter(|row| {
            let id = row.buffer_post_id.as_str();
            Window::samples().iter().any(|w| {
                !rows
                    .iter()
                    .any(|r| r.buffer_post_id == id && r.window == w.as_str())
            })
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    pending.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    out.push_str("\n## Still maturing\n\n");
    for row in pending {
        out.push_str(&format!(
            "- {} · {} — sent {}\n",
            row.video_id,
            row.platform,
            row.sent_at.as_deref().map(short_date).unwrap_or("—")
        ));
    }
}

/// The metric to rank on: the first preferred one this set actually reports.
fn headline_metric(rows: &[&AnalyticsRow]) -> String {
    let present = columns_for(rows);
    PREFERRED
        .iter()
        .find(|name| present.iter().any(|column| column == *name))
        .map(|name| name.to_string())
        .or_else(|| present.first().cloned())
        .unwrap_or_default()
}

/// Every metric name present, preferred ones first.
fn columns_for(rows: &[&AnalyticsRow]) -> Vec<String> {
    let present: BTreeSet<&str> = rows
        .iter()
        .flat_map(|row| row.metrics.iter())
        .map(|metric| metric.name.as_str())
        .collect();
    let mut out: Vec<String> = PREFERRED
        .iter()
        .filter(|name| present.contains(*name))
        .map(|name| name.to_string())
        .collect();
    out.extend(
        present
            .iter()
            .filter(|name| !PREFERRED.contains(name))
            .map(|name| name.to_string()),
    );
    out
}

fn stalest<'a>(rows: &[&'a AnalyticsRow]) -> Option<&'a str> {
    rows.iter()
        .filter_map(|row| row.metrics_updated_at.as_deref())
        .min()
}

fn number(value: f64, column: &str) -> String {
    if column.ends_with("Rate") {
        return format!("{value:.1}%");
    }
    if value.fract() == 0.0 {
        return (value as i64).to_string();
    }
    format!("{value:.1}")
}

/// "2026-08-09T20:44:00Z" → "2026-08-09".
fn short_date(stamp: &str) -> &str {
    stamp.split('T').next().unwrap_or(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::metrics::MetricValue;

    fn metric(name: &str, value: f64) -> MetricValue {
        MetricValue {
            name: name.into(),
            kind: name.into(),
            unit: if name.ends_with("Rate") {
                "percentage"
            } else {
                "count"
            }
            .into(),
            value,
        }
    }

    fn row(video: &str, platform: &str, window: Window, metrics: Vec<MetricValue>) -> AnalyticsRow {
        AnalyticsRow {
            buffer_post_id: format!("{video}-{platform}"),
            project: "vd-42".into(),
            version: Some(3),
            video_id: video.into(),
            platform: platform.into(),
            window: window.as_str().to_string(),
            sent_at: Some("2026-08-09T20:44:00Z".into()),
            pulled_at: "2026-08-16T09:00:00Z".into(),
            metrics_updated_at: Some("2026-08-16T06:00:00Z".into()),
            metrics,
            prompt_id: Some("posts.social".into()),
            copy_hash: Some("abcd".into()),
        }
    }

    /// The one section a reader must act on leads, and survives an otherwise
    /// empty history — a project whose only post failed has nothing else to say.
    #[test]
    fn posts_buffer_failed_lead_the_report_even_with_nothing_measured() {
        let failed = vec!["chapter-02 → bluesky: stuck processing".to_string()];
        let out = render(&[], &failed, "vd-42", "now");
        let section = out.find("## Failed at Buffer").expect("the failed section");
        let nothing = out.find("Nothing measured yet").expect("the empty note");
        assert!(section < nothing, "failures come first");
        assert!(out.contains("1 post(s) need a retry or a delete in Buffer"));
        assert!(out.contains("- chapter-02 → bluesky: stuck processing"));
        assert!(!render(&[], &[], "vd-42", "now").contains("Failed at Buffer"));
    }

    #[test]
    fn an_empty_history_says_why_rather_than_showing_nothing() {
        let out = render(&[], &[], "vd-42", "2026-08-16T09:00:00Z");
        assert!(out.contains("# Analytics — vd-42"));
        assert!(out.contains("Nothing measured yet"));
        assert!(out.contains("7 and 30 days"));
    }

    #[test]
    fn a_week_table_ranks_best_first_and_names_the_winning_prompt() {
        let rows = vec![
            row(
                "chapter-01",
                "tiktok",
                Window::Week,
                vec![metric("impressions", 3400.0)],
            ),
            row(
                "chapter-03",
                "tiktok",
                Window::Week,
                vec![metric("impressions", 8120.0)],
            ),
        ];
        let out = render(&rows, &[], "vd-42", "2026-08-16T09:00:00Z");
        let first = out.find("chapter-03").unwrap();
        let second = out.find("chapter-01").unwrap();
        assert!(first < second, "the better post leads");
        assert!(out.contains("## 7 days after posting"));
        assert!(out.contains("**Best: chapter-03 on tiktok**"));
        assert!(out.contains("prompt `posts.social`"));
    }

    /// The core rule: columns follow the data, and a network that does not report
    /// a metric gets a dash rather than a zero.
    #[test]
    fn columns_come_from_the_metrics_that_actually_arrived() {
        let rows = vec![
            row(
                "chapter-03",
                "tiktok",
                Window::Week,
                vec![metric("views", 8120.0), metric("saves", 210.0)],
            ),
            row(
                "longform",
                "youtube",
                Window::Week,
                vec![metric("impressions", 1032.0)],
            ),
        ];
        let out = render(&rows, &[], "vd-42", "now");
        assert!(out.contains("| impressions |"));
        assert!(out.contains(" views |"));
        assert!(out.contains(" saves |"));
        assert!(
            out.contains(" — |"),
            "a missing metric is a dash, not a zero"
        );
        assert!(!out.contains("totalTimeWatched"));
    }

    #[test]
    fn rates_render_as_percentages_and_counts_as_whole_numbers() {
        let rows = vec![row(
            "chapter-03",
            "tiktok",
            Window::Week,
            vec![
                metric("impressions", 8120.0),
                metric("engagementRate", 6.83),
            ],
        )];
        let out = render(&rows, &[], "vd-42", "now");
        assert!(out.contains("| 8120 |"));
        assert!(out.contains("6.8%"));
    }

    #[test]
    fn both_windows_get_their_own_table() {
        let rows = vec![
            row(
                "chapter-03",
                "tiktok",
                Window::Week,
                vec![metric("views", 8120.0)],
            ),
            row(
                "chapter-03",
                "tiktok",
                Window::Month,
                vec![metric("views", 19400.0)],
            ),
        ];
        let out = render(&rows, &[], "vd-42", "now");
        assert!(out.contains("## 7 days after posting"));
        assert!(out.contains("## 30 days after posting"));
        assert!(out.contains("19400"));
    }

    /// A sent post with no sample yet must read as "too early", not as a failure.
    #[test]
    fn posts_awaiting_their_first_window_are_listed_as_maturing() {
        let mut sent = row("chapter-05", "instagram", Window::Sent, Vec::new());
        sent.sent_at = Some("2026-08-14T08:00:00Z".into());
        let rows = vec![
            row(
                "chapter-03",
                "tiktok",
                Window::Week,
                vec![metric("views", 10.0)],
            ),
            sent,
        ];
        let out = render(&rows, &[], "vd-42", "now");
        assert!(out.contains("## Still maturing"));
        assert!(out.contains("chapter-05 · instagram — sent 2026-08-14"));
    }

    #[test]
    fn a_fully_sampled_post_is_not_listed_as_maturing() {
        let rows = vec![
            row("chapter-03", "tiktok", Window::Sent, Vec::new()),
            row(
                "chapter-03",
                "tiktok",
                Window::Week,
                vec![metric("views", 10.0)],
            ),
            row(
                "chapter-03",
                "tiktok",
                Window::Month,
                vec![metric("views", 20.0)],
            ),
        ];
        let out = render(&rows, &[], "vd-42", "now");
        assert!(!out.contains("Still maturing"));
    }

    /// Buffer refreshes on its own cadence, so the report dates itself by the
    /// oldest refresh in the table rather than implying everything is current.
    #[test]
    fn the_table_reports_its_stalest_refresh() {
        let mut old = row(
            "chapter-01",
            "tiktok",
            Window::Week,
            vec![metric("views", 5.0)],
        );
        old.metrics_updated_at = Some("2026-08-12T06:00:00Z".into());
        let rows = vec![
            row(
                "chapter-03",
                "tiktok",
                Window::Week,
                vec![metric("views", 10.0)],
            ),
            old,
        ];
        let out = render(&rows, &[], "vd-42", "now");
        assert!(out.contains("_Metrics as of 2026-08-12T06:00:00Z._"));
    }
}
