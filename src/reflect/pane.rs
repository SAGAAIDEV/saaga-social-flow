//! Turning a reflection into what its template needs.
//!
//! The pane is rendered in Rust, so everything the HTML shows is decided here —
//! in a pure function with tests, rather than in template logic or JavaScript.
//! A template that only interpolates cannot be wrong about the data.

use serde::Serialize;

use crate::agent::prompt;
use crate::reflect::diff;
use crate::reflect::schema::{ReflectReport, Rewrite};

/// The whole pane.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pane {
    pub report: Option<Report>,
    /// Whether anything is both ticked and validated.
    pub can_apply: bool,
    pub applicable: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub project: String,
    pub generated_at: String,
    pub inputs: String,
    pub keep: Vec<String>,
    pub drop: Vec<String>,
    pub evidence: Vec<Claim>,
    pub rewrites: Vec<Proposal>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Claim {
    pub claim: String,
    pub grounded: bool,
    pub backing: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub index: usize,
    pub prompt_id: String,
    pub approved: bool,
    /// "+2 −1 lines"
    pub scale: String,
    pub status: String,
    /// ok / bad / warn / "" — drives the pill colour.
    pub tone: &'static str,
    pub why: String,
    pub version_note: String,
    pub diff: Vec<Line>,
    pub validation: Option<String>,
}

/// One line of a rendered diff.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    /// same / add / del / skip
    pub kind: &'static str,
    pub text: String,
}

/// Builds the pane for `report`, diffing each proposal against what is live.
pub fn build(report: Option<&ReflectReport>, root: &std::path::Path) -> Pane {
    let Some(report) = report else {
        return Pane {
            report: None,
            can_apply: false,
            applicable: 0,
        };
    };
    let applicable = report.applicable().count();
    Pane {
        can_apply: applicable > 0,
        applicable,
        report: Some(Report {
            project: report.project.clone(),
            generated_at: report.generated_at.clone(),
            inputs: report.inputs.summary(),
            keep: report.reflection.keep.clone(),
            drop: report.reflection.drop.clone(),
            evidence: report
                .reflection
                .evidence
                .iter()
                .map(|item| Claim {
                    claim: item.claim.clone(),
                    grounded: item.is_grounded(),
                    backing: item.post_ids.join(", "),
                })
                .collect(),
            rewrites: report
                .reflection
                .rewrite
                .iter()
                .enumerate()
                .map(|(index, rewrite)| proposal(index, rewrite, root))
                .collect(),
        }),
    }
}

fn proposal(index: usize, rewrite: &Rewrite, root: &std::path::Path) -> Proposal {
    let live = prompt::live(&rewrite.prompt_id, Some(root));
    let (scale, version_note, lines) = match &live {
        Some(resolved) => (
            diff::summary(&resolved.text, &rewrite.preamble),
            format!(
                "{} → would become v{}",
                resolved.label(),
                prompt::next_version(&prompt::load_versions(root), &rewrite.prompt_id)
            ),
            render_diff(&resolved.text, &rewrite.preamble),
        ),
        // A prompt this program does not have cannot be diffed against anything,
        // and must not read as a small change.
        None => (
            "unknown prompt".to_string(),
            "cannot be applied".to_string(),
            vec![Line {
                kind: "add",
                text: rewrite.preamble.clone(),
            }],
        ),
    };
    Proposal {
        index,
        prompt_id: rewrite.prompt_id.clone(),
        approved: rewrite.approved,
        scale,
        status: rewrite.status().to_string(),
        tone: tone(rewrite),
        why: rewrite.why.clone(),
        version_note,
        diff: lines,
        validation: rewrite
            .validation
            .as_ref()
            .map(|validation| validation.detail.clone()),
    }
}

/// A failed validation is the loudest thing on the row; unproven is a warning,
/// not a failure.
fn tone(rewrite: &Rewrite) -> &'static str {
    match &rewrite.validation {
        Some(validation) if !validation.ok => "bad",
        None => "warn",
        Some(_) if rewrite.approved => "ok",
        Some(_) => "",
    }
}

fn render_diff(before: &str, after: &str) -> Vec<Line> {
    diff::render(before, after)
        .lines()
        .map(|line| {
            let (kind, text) = match line.chars().next() {
                Some('+') => ("add", &line[1..]),
                Some('-') => ("del", &line[1..]),
                _ if line.trim_start().starts_with('…') => ("skip", line),
                _ => ("same", line),
            };
            Line {
                kind,
                text: text.trim_end().to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflect::schema::{Inputs, Reflection, Validation};

    fn rewrite(prompt_id: &str, approved: bool, validation: Option<bool>) -> Rewrite {
        Rewrite {
            prompt_id: prompt_id.into(),
            preamble: "Write shorter hooks.".into(),
            why: "captions were shortened by hand".into(),
            approved,
            validation: validation.map(|ok| Validation {
                ok,
                checked_at: "now".into(),
                detail: if ok {
                    "12 post(s)".into()
                } else {
                    "over limit".into()
                },
            }),
        }
    }

    fn report(rewrites: Vec<Rewrite>) -> ReflectReport {
        ReflectReport {
            project: "vd-42".into(),
            version: Some(3),
            generated_at: "2026-08-16T09:00:00Z".into(),
            inputs: Inputs {
                llm_steps: 3,
                ..Inputs::default()
            },
            reflection: Reflection {
                keep: vec!["short hooks".into()],
                drop: Vec::new(),
                evidence: Vec::new(),
                rewrite: rewrites,
            },
        }
    }

    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("stream-recorder-pane-{}", std::process::id()))
    }

    #[test]
    fn no_report_yields_an_empty_pane() {
        let pane = build(None, &root());
        assert!(pane.report.is_none());
        assert!(!pane.can_apply);
        assert_eq!(pane.applicable, 0);
    }

    #[test]
    fn apply_is_only_offered_when_something_is_ticked_and_validated() {
        let ready = build(
            Some(&report(vec![rewrite("posts.social", true, Some(true))])),
            &root(),
        );
        assert!(ready.can_apply);
        assert_eq!(ready.applicable, 1);

        let unproven = build(
            Some(&report(vec![rewrite("posts.social", true, None)])),
            &root(),
        );
        assert!(!unproven.can_apply);
    }

    #[test]
    fn a_proposal_carries_its_diff_against_the_live_prompt() {
        let pane = build(
            Some(&report(vec![rewrite("posts.social", false, None)])),
            &root(),
        );
        let proposal = &pane.report.unwrap().rewrites[0];
        assert_eq!(proposal.index, 0);
        assert!(
            proposal.scale.contains("lines"),
            "sized: {}",
            proposal.scale
        );
        assert!(proposal.version_note.contains("would become v1"));
        assert!(!proposal.diff.is_empty());
        // The builtin is long, so replacing it with one line is mostly deletions.
        assert!(proposal.diff.iter().any(|line| line.kind == "del"));
        assert!(proposal.diff.iter().any(|line| line.kind == "add"));
    }

    /// Colour carries the state, so a failed validation must not look neutral.
    #[test]
    fn tone_makes_a_failed_validation_loud() {
        assert_eq!(tone(&rewrite("posts.social", true, Some(false))), "bad");
        assert_eq!(tone(&rewrite("posts.social", false, Some(false))), "bad");
        assert_eq!(tone(&rewrite("posts.social", true, None)), "warn");
        assert_eq!(tone(&rewrite("posts.social", true, Some(true))), "ok");
        assert_eq!(tone(&rewrite("posts.social", false, Some(true))), "");
    }

    #[test]
    fn an_unknown_prompt_cannot_be_applied_and_says_so() {
        let pane = build(
            Some(&report(vec![rewrite("schedule.plan", true, Some(true))])),
            &root(),
        );
        let proposal = &pane.report.unwrap().rewrites[0];
        assert_eq!(proposal.scale, "unknown prompt");
        assert_eq!(proposal.version_note, "cannot be applied");
    }

    #[test]
    fn diff_lines_are_classified_for_the_template() {
        let lines = render_diff("keep\nold line\ntail", "keep\nnew line\ntail");
        let kinds: Vec<&str> = lines.iter().map(|line| line.kind).collect();
        assert!(kinds.contains(&"del"));
        assert!(kinds.contains(&"add"));
        assert!(kinds.contains(&"same"));
        let removed = lines.iter().find(|line| line.kind == "del").unwrap();
        assert_eq!(removed.text.trim(), "old line", "the marker is stripped");
    }

    #[test]
    fn elided_runs_are_marked_as_skips() {
        let before: String = (0..40).map(|n| format!("rule {n}\n")).collect();
        let after = before.replace("rule 20", "rule twenty");
        let lines = render_diff(&before, &after);
        assert!(lines.iter().any(|line| line.kind == "skip"));
    }

    #[test]
    fn evidence_records_whether_anything_backs_it() {
        let mut source = report(Vec::new());
        source.reflection.evidence = vec![
            crate::reflect::schema::Evidence {
                claim: "grounded".into(),
                post_ids: vec!["post-9".into()],
                metric: None,
                delta: None,
            },
            crate::reflect::schema::Evidence {
                claim: "a hunch".into(),
                post_ids: Vec::new(),
                metric: None,
                delta: None,
            },
        ];
        let pane = build(Some(&source), &root());
        let evidence = pane.report.unwrap().evidence;
        assert!(evidence[0].grounded);
        assert_eq!(evidence[0].backing, "post-9");
        assert!(!evidence[1].grounded);
    }
}
