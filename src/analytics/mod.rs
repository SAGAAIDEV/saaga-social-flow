//! Analytics stage: sample what was queued, then report on it.
//!
//! One press collects every sample that has come due and rewrites the report.
//! Nothing is scheduled — see [`due`] for why a desktop app cannot fire a timer
//! seven days after a post it did not choose the send time for.
//!
//! The join is `buffer_post_id`: `schedule.jsonl` says what we queued and which
//! prompt wrote it, Buffer says how it did, and `analytics.jsonl` records the
//! pair. Posts queued outside this app are not in the ledger, so they are not in
//! the report — that is the cost of correlating to our own copy.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

use crate::schedule::buffer::BufferClient;
use crate::schedule::{ledger, metrics};
use crate::session::Session;

pub mod due;
pub mod queue;
pub mod report;
pub mod schema;

use due::{due_now, now_unix};
use schema::{append_row, load_rows, AnalyticsRow, Window};

pub const REPORT_MD: &str = "analytics-report.md";

/// What one pull did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PullOutcome {
    pub report: PathBuf,
    /// Posts observed going out for the first time.
    pub newly_sent: usize,
    pub sampled: usize,
    pub failed: usize,
    /// Ledger rows Buffer has not sent yet.
    pub waiting: usize,
    /// Posts Buffer tried to publish and gave up on, each with its message.
    /// They will never become "sent" on their own, so they are named rather
    /// than folded into `waiting` — where one sat unseen for weeks.
    pub errored: Vec<String>,
}

impl PullOutcome {
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} sample(s)", self.sampled)];
        if self.newly_sent > 0 {
            parts.push(format!("{} newly sent", self.newly_sent));
        }
        if self.waiting > 0 {
            parts.push(format!("{} still queued", self.waiting));
        }
        if !self.errored.is_empty() {
            parts.push(format!("{} failed at Buffer", self.errored.len()));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(", ")
    }
}

pub enum AnalyticsEvent {
    Status(String),
    Ready(PullOutcome, String),
    /// A cross-project sweep finished: the totals and how many projects it touched.
    Collected(PullOutcome, usize),
    Failed(String),
}

#[tracing::instrument(skip_all)]
pub fn spawn_pull(session: Session, tx: Sender<AnalyticsEvent>) {
    if let Err(err) = thread::Builder::new()
        .name("analytics-pull".into())
        .spawn(move || match run_pull(&session, &tx) {
            Ok((outcome, markdown)) => {
                eprintln!("stream-recorder: analytics {}", outcome.summary());
                let _ = tx.send(AnalyticsEvent::Ready(outcome, markdown));
            }
            Err(err) => {
                eprintln!("stream-recorder: analytics pull failed: {err:#}");
                let _ = tx.send(AnalyticsEvent::Failed(format!("Analytics failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start analytics job: {err}");
    }
}

/// Pulls for every project that owes something, not just the open one.
///
/// The queue exists because samples come due long after you have moved on, so a
/// collector that only ever looks at the current project would miss exactly the
/// work the queue is there to surface.
#[tracing::instrument(skip_all)]
pub fn spawn_collect_all(tx: Sender<AnalyticsEvent>) {
    if let Err(err) = thread::Builder::new()
        .name("analytics-collect-all".into())
        .spawn(move || {
            let entries = crate::sessions::list();
            let owed = queue::scan(&entries, due::now_unix());
            if owed.is_empty() {
                let _ = tx.send(AnalyticsEvent::Status("Nothing due anywhere.".to_string()));
                return;
            }
            let mut total = PullOutcome::default();
            for project in &owed.projects {
                let Some(entry) = entries.iter().find(|e| e.folder == project.folder) else {
                    continue;
                };
                let _ = tx.send(AnalyticsEvent::Status(format!(
                    "Collecting {}…",
                    project.title
                )));
                let session = match Session::open_root(entry.root.clone()) {
                    Ok(session) => session,
                    Err(err) => {
                        eprintln!("stream-recorder: cannot open {}: {err:#}", project.title);
                        continue;
                    }
                };
                // One project's failure must not abandon the rest of the queue.
                match run_pull(&session, &tx) {
                    Ok((outcome, _)) => {
                        total.sampled += outcome.sampled;
                        total.newly_sent += outcome.newly_sent;
                        total.waiting += outcome.waiting;
                        total.failed += outcome.failed;
                        total.errored.extend(outcome.errored);
                    }
                    Err(err) => {
                        total.failed += 1;
                        let _ = tx.send(AnalyticsEvent::Status(format!(
                            "{} failed: {err:#}",
                            project.title
                        )));
                    }
                }
            }
            let _ = tx.send(AnalyticsEvent::Collected(total, owed.projects.len()));
        })
    {
        eprintln!("stream-recorder: could not start collect-all job: {err}");
    }
}

fn run_pull(session: &Session, tx: &Sender<AnalyticsEvent>) -> Result<(PullOutcome, String)> {
    let status = |msg: String| {
        let _ = tx.send(AnalyticsEvent::Status(msg));
    };

    let queued = ledger::load_rows(&session.root)?;
    if queued.is_empty() {
        let markdown = write_report(session, &[], &[])?;
        return Ok((
            PullOutcome {
                report: session.root.join(REPORT_MD),
                ..PullOutcome::default()
            },
            markdown,
        ));
    }

    let mut samples = load_rows(&session.root)?;
    let owed = due_now(&queued, &samples, now_unix());
    let mut outcome = PullOutcome {
        report: session.root.join(REPORT_MD),
        ..PullOutcome::default()
    };
    if owed.is_empty() {
        status("Nothing due — every sample is already collected.".to_string());
    } else {
        status(format!("Pulling {} sample(s) from Buffer…", owed.len()));
        let client = BufferClient::from_env()?;
        for (step, item) in owed.iter().enumerate() {
            let label = format!("{} → {}", item.video_id, item.platform);
            status(format!("[{}/{}] {label}…", step + 1, owed.len()));
            let Some(row) = queued
                .iter()
                .find(|row| row.buffer_post_id == item.buffer_post_id)
            else {
                continue;
            };
            match metrics::fetch(&client, &item.buffer_post_id) {
                Ok(observed) => {
                    if item.window == Window::Sent {
                        // Buffer gave up on it. Not recorded, so a retry in Buffer's
                        // UI is picked up by the next pull — but named, because a
                        // post in this state is waiting on a human, not on time.
                        if let Some(why) = observed.failure() {
                            eprintln!("stream-recorder: {label} failed at Buffer: {why}");
                            status(format!("{label} failed at Buffer: {why}"));
                            outcome.errored.push(format!("{label}: {why}"));
                            continue;
                        }
                        // Only record a send once Buffer really sent it: the absence
                        // of a "sent" row is what keeps the post being re-checked.
                        if !observed.is_sent() {
                            outcome.waiting += 1;
                            continue;
                        }
                    }
                    let sample = AnalyticsRow::new(row, item.window, &observed, iso_now());
                    append_row(&session.root, &sample)?;
                    if item.window == Window::Sent {
                        outcome.newly_sent += 1;
                    } else {
                        outcome.sampled += 1;
                    }
                    samples.push(sample);
                }
                Err(err) => {
                    outcome.failed += 1;
                    status(format!("{label} failed: {err:#}"));
                }
            }
        }
    }

    let markdown = write_report(session, &samples, &outcome.errored)?;
    Ok((outcome, markdown))
}

fn write_report(session: &Session, samples: &[AnalyticsRow], failed: &[String]) -> Result<String> {
    let markdown = report::render(samples, failed, &session.title(), &iso_now());
    let path = session.root.join(REPORT_MD);
    std::fs::write(&path, &markdown).with_context(|| format!("writing {}", path.display()))?;
    Ok(markdown)
}

fn iso_now() -> String {
    ledger::now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A post Buffer failed is not "still queued": the two need opposite
    /// actions, and the summary is where the difference is first seen.
    #[test]
    fn a_post_buffer_failed_is_named_apart_from_the_queued_ones() {
        let outcome = PullOutcome {
            waiting: 3,
            errored: vec!["chapter-02 → bluesky: stuck processing".into()],
            ..PullOutcome::default()
        };
        assert_eq!(
            outcome.summary(),
            "0 sample(s), 3 still queued, 1 failed at Buffer"
        );
    }

    #[test]
    fn the_summary_names_every_non_zero_count() {
        let mut outcome = PullOutcome {
            sampled: 4,
            ..PullOutcome::default()
        };
        assert_eq!(outcome.summary(), "4 sample(s)");
        outcome.newly_sent = 2;
        outcome.waiting = 7;
        outcome.failed = 1;
        assert_eq!(
            outcome.summary(),
            "4 sample(s), 2 newly sent, 7 still queued, 1 failed"
        );
    }
}
