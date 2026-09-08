//! The cross-project work queue: what is owed, everywhere, right now.
//!
//! The failure mode this exists for is not forgetting a button — it is having
//! moved on. Samples come due 7 and 30 days after a post sends, by which time the
//! project that queued it is closed and you are three projects along. A per-project
//! button can never surface that; only a scan across every project can.
//!
//! Every count here is computed from local files — `schedule.jsonl` and
//! `analytics.jsonl` in each session folder — so the queue costs no network and
//! can be recomputed on a timer without asking Buffer anything.
//!
//! `awaiting_send` is the honest exception: a post we have never seen go out
//! *might* be collectable, but confirming that needs a query. It is counted
//! separately rather than folded into the actionable total, so the badge never
//! promises work that turns out not to exist.

use crate::analytics::due::due_now;
use crate::analytics::schema::{load_rows, Window};
use crate::schedule::ledger;
use crate::sessions::SessionEntry;

/// What one project owes.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectDue {
    pub folder: String,
    pub title: String,
    pub week: usize,
    pub month: usize,
    /// Posts not yet known to have sent. Needs a network check to resolve.
    pub awaiting_send: usize,
}

impl ProjectDue {
    /// Samples collectable right now, without asking Buffer whether anything sent.
    pub fn ready(&self) -> usize {
        self.week + self.month
    }

    pub fn has_work(&self) -> bool {
        self.ready() > 0 || self.awaiting_send > 0
    }

    /// One line for the queue list.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.week > 0 {
            parts.push(format!("7d ×{}", self.week));
        }
        if self.month > 0 {
            parts.push(format!("30d ×{}", self.month));
        }
        if self.awaiting_send > 0 {
            parts.push(format!("{} awaiting send", self.awaiting_send));
        }
        format!("{} — {}", self.title, parts.join(", "))
    }
}

/// The whole queue, worst-first, projects with nothing owed omitted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Queue {
    pub projects: Vec<ProjectDue>,
}

impl Queue {
    /// Samples collectable right now across every project — the badge number.
    pub fn ready(&self) -> usize {
        self.projects.iter().map(ProjectDue::ready).sum()
    }

    pub fn awaiting_send(&self) -> usize {
        self.projects.iter().map(|p| p.awaiting_send).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }

    /// The tab badge, e.g. "Analytics (4)". No suffix when nothing is collectable,
    /// because a badge that is always lit stops being read.
    pub fn badge(&self, base: &str) -> String {
        match self.ready() {
            0 => base.to_string(),
            n => format!("{base} ({n})"),
        }
    }

    pub fn summary(&self) -> String {
        if self.projects.is_empty() {
            return "Nothing due.".to_string();
        }
        let mut text = format!(
            "{} sample(s) due across {} project(s)",
            self.ready(),
            self.projects.len()
        );
        if self.awaiting_send() > 0 {
            text.push_str(&format!(", {} awaiting send", self.awaiting_send()));
        }
        text
    }

    pub fn as_markdown(&self) -> String {
        if self.projects.is_empty() {
            return String::new();
        }
        let mut out = format!("## Work queue — {}\n\n", self.summary());
        for project in &self.projects {
            out.push_str(&format!("- {}\n", project.label()));
        }
        out.push('\n');
        out
    }
}

/// Reads one project's ledgers and counts what it owes.
///
/// An unreadable ledger yields `None` rather than a zero: "cannot tell" must not
/// render as "nothing to do", which is exactly the silence this queue exists to
/// break.
pub fn project_due(entry: &SessionEntry, now: u64) -> Option<ProjectDue> {
    let queued = ledger::load_rows(&entry.root).ok()?;
    if queued.is_empty() {
        return None;
    }
    let samples = load_rows(&entry.root).ok()?;
    let owed = due_now(&queued, &samples, now);
    let count = |window: Window| owed.iter().filter(|due| due.window == window).count();
    Some(ProjectDue {
        folder: entry.folder.clone(),
        title: entry.title().to_string(),
        week: count(Window::Week),
        month: count(Window::Month),
        awaiting_send: count(Window::Sent),
    })
}

/// Scans every project, keeping only those with work. Most owed first.
pub fn scan(entries: &[SessionEntry], now: u64) -> Queue {
    let mut projects: Vec<ProjectDue> = entries
        .iter()
        .filter_map(|entry| project_due(entry, now))
        .filter(ProjectDue::has_work)
        .collect();
    projects.sort_by(|a, b| {
        b.ready()
            .cmp(&a.ready())
            .then(b.awaiting_send.cmp(&a.awaiting_send))
            .then(a.title.cmp(&b.title))
    });
    Queue { projects }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(title: &str, week: usize, month: usize, awaiting: usize) -> ProjectDue {
        ProjectDue {
            folder: "2026-08-15_02-19-20".into(),
            title: title.into(),
            week,
            month,
            awaiting_send: awaiting,
        }
    }

    #[test]
    fn the_badge_only_shows_collectable_work() {
        assert_eq!(Queue::default().badge("Analytics"), "Analytics");
        let queue = Queue {
            projects: vec![project("Rust errors", 3, 1, 0)],
        };
        assert_eq!(queue.badge("Analytics"), "Analytics (4)");
    }

    /// A post we have not seen send is not collectable work, so it must not light
    /// the badge — otherwise the badge is permanently on and stops meaning anything.
    #[test]
    fn posts_awaiting_send_do_not_light_the_badge() {
        let queue = Queue {
            projects: vec![project("Rust errors", 0, 0, 6)],
        };
        assert_eq!(queue.ready(), 0);
        assert_eq!(queue.badge("Analytics"), "Analytics");
        assert_eq!(queue.awaiting_send(), 6);
        assert!(!queue.is_empty(), "it is still worth listing");
        assert!(queue.summary().contains("6 awaiting send"));
    }

    #[test]
    fn a_project_line_names_each_window() {
        assert_eq!(
            project("Rust errors", 3, 1, 0).label(),
            "Rust errors — 7d ×3, 30d ×1"
        );
        assert_eq!(
            project("Buffer walkthrough", 0, 6, 2).label(),
            "Buffer walkthrough — 30d ×6, 2 awaiting send"
        );
    }

    #[test]
    fn the_queue_leads_with_the_project_owing_most() {
        let queue = Queue {
            projects: {
                let mut v = vec![
                    project("Quiet one", 1, 0, 0),
                    project("Busy one", 4, 2, 0),
                    project("Middle", 2, 0, 0),
                ];
                v.sort_by(|a, b| b.ready().cmp(&a.ready()));
                v
            },
        };
        let titles: Vec<&str> = queue.projects.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["Busy one", "Middle", "Quiet one"]);
        assert_eq!(queue.ready(), 9);
    }

    #[test]
    fn an_empty_queue_says_so_and_renders_nothing() {
        let queue = Queue::default();
        assert!(queue.is_empty());
        assert_eq!(queue.summary(), "Nothing due.");
        assert_eq!(queue.as_markdown(), "");
    }

    #[test]
    fn the_markdown_lists_every_project_with_work() {
        let queue = Queue {
            projects: vec![
                project("Rust errors", 3, 1, 0),
                project("Walkthrough", 0, 6, 2),
            ],
        };
        let out = queue.as_markdown();
        assert!(out.starts_with("## Work queue — 10 sample(s) due across 2 project(s)"));
        assert!(out.contains("- Rust errors — 7d ×3, 30d ×1"));
        assert!(out.contains("- Walkthrough — 30d ×6, 2 awaiting send"));
    }

    #[test]
    fn a_project_with_nothing_owed_is_not_listed() {
        assert!(!project("Done", 0, 0, 0).has_work());
    }
}
