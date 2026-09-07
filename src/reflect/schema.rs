//! What a reflection proposes, and how it is stored.
//!
//! Written to `{root}/reflect/vN/reflect.json` and reviewed before anything is
//! applied. A [`Rewrite`] carries `approved` and `validated` separately on
//! purpose: ticking one says a human wants it, and validating says the model's
//! new preamble actually still produces usable copy. Apply needs both.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const REFLECT_JSON: &str = "reflect.json";

/// A claim tied to the posts that support it.
///
/// Without `post_ids` a recommendation is the model's prior rather than a finding,
/// and the review surface says so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub claim: String,
    #[serde(default)]
    pub post_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

impl Evidence {
    /// True when something in the ledgers backs this up.
    pub fn is_grounded(&self) -> bool {
        !self.post_ids.is_empty()
    }
}

/// A proposed replacement preamble for one prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rewrite {
    /// One of the ids in `agent::prompt` — "posts.social", "titles.chapter_cards".
    pub prompt_id: String,
    /// The full replacement preamble. The output schema is enforced structurally,
    /// so this is guidance only and cannot break parsing.
    pub preamble: String,
    pub why: String,
    /// Ticked in the Reflect tab.
    #[serde(default)]
    #[schemars(skip)]
    pub approved: bool,
    /// Set once this preamble has been run against real project content and the
    /// result checked. `None` means never tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub validation: Option<Validation>,
}

/// The outcome of running a proposed preamble for real.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Validation {
    pub ok: bool,
    pub checked_at: String,
    /// What was produced or what went wrong, for the review pane.
    pub detail: String,
}

impl Rewrite {
    /// Apply needs a human tick *and* a passing validation. A rewrite that parses
    /// can still produce unusable copy, and one a human likes can still be broken.
    pub fn is_applicable(&self) -> bool {
        self.approved && matches!(&self.validation, Some(v) if v.ok)
    }

    pub fn status(&self) -> &'static str {
        match (&self.validation, self.approved) {
            (Some(v), _) if !v.ok => "FAILED validation",
            (None, _) => "not validated",
            (Some(_), false) => "validated — not approved",
            (Some(_), true) => "ready to apply",
        }
    }
}

/// What the model returns. Kept free of bookkeeping so the schema stays small.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reflection {
    /// What is working and should be preserved in any rewrite.
    #[serde(default)]
    pub keep: Vec<String>,
    /// What is not working and should stop.
    #[serde(default)]
    pub drop: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub rewrite: Vec<Rewrite>,
}

/// A reflection plus the bookkeeping the model never sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflectReport {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub generated_at: String,
    /// What the corpus actually contained, so a thin report is readable as thin
    /// rather than as "nothing to say".
    pub inputs: Inputs,
    #[serde(flatten)]
    pub reflection: Reflection,
}

/// How much evidence the reflection had to work with.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Inputs {
    pub llm_steps: usize,
    pub titles_edited: usize,
    pub captions_edited: usize,
    pub declined: usize,
    pub measured_posts: usize,
}

impl Inputs {
    pub fn summary(&self) -> String {
        format!(
            "{} llm step(s), {} edited title(s), {} edited caption(s), {} declined, {} measured",
            self.llm_steps,
            self.titles_edited,
            self.captions_edited,
            self.declined,
            self.measured_posts
        )
    }

    /// True when there is nothing a reflection could reason from.
    pub fn is_empty(&self) -> bool {
        self.llm_steps == 0
            && self.titles_edited == 0
            && self.captions_edited == 0
            && self.declined == 0
            && self.measured_posts == 0
    }
}

impl ReflectReport {
    pub fn applicable(&self) -> impl Iterator<Item = &Rewrite> {
        self.reflection
            .rewrite
            .iter()
            .filter(|rewrite| rewrite.is_applicable())
    }
}

pub fn save(dir: &Path, report: &ReflectReport) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(REFLECT_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(report).context("serializing reflection")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load(dir: &Path) -> Result<ReflectReport> {
    let path = dir.join(REFLECT_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(approved: bool, validation: Option<bool>) -> Rewrite {
        Rewrite {
            prompt_id: "posts.social".into(),
            preamble: "Write shorter hooks.".into(),
            why: "short hooks outperformed".into(),
            approved,
            validation: validation.map(|ok| Validation {
                ok,
                checked_at: "2026-08-16T09:00:00Z".into(),
                detail: if ok { "12 posts".into() } else { "no output".into() },
            }),
        }
    }

    fn report(rewrites: Vec<Rewrite>) -> ReflectReport {
        ReflectReport {
            project: "vd-42".into(),
            version: Some(3),
            generated_at: "2026-08-16T09:00:00Z".into(),
            inputs: Inputs {
                llm_steps: 4,
                titles_edited: 2,
                ..Inputs::default()
            },
            reflection: Reflection {
                keep: vec!["short hooks".into()],
                drop: vec!["generic CTAs".into()],
                evidence: Vec::new(),
                rewrite: rewrites,
            },
        }
    }

    /// The whole gate: a tick alone is not enough, and neither is a pass alone.
    #[test]
    fn applying_needs_both_a_tick_and_a_passing_validation() {
        assert!(!rewrite(true, None).is_applicable(), "unvalidated");
        assert!(!rewrite(true, Some(false)).is_applicable(), "failed");
        assert!(!rewrite(false, Some(true)).is_applicable(), "unapproved");
        assert!(rewrite(true, Some(true)).is_applicable());
    }

    #[test]
    fn status_names_the_blocking_condition() {
        assert_eq!(rewrite(true, None).status(), "not validated");
        assert_eq!(rewrite(true, Some(false)).status(), "FAILED validation");
        assert_eq!(rewrite(false, Some(true)).status(), "validated — not approved");
        assert_eq!(rewrite(true, Some(true)).status(), "ready to apply");
        // A failed validation outranks the tick: it is the thing to fix.
        assert_eq!(rewrite(false, Some(false)).status(), "FAILED validation");
    }

    #[test]
    fn only_applicable_rewrites_are_offered() {
        let report = report(vec![
            rewrite(true, Some(true)),
            rewrite(true, None),
            rewrite(false, Some(true)),
        ]);
        assert_eq!(report.applicable().count(), 1);
    }

    #[test]
    fn evidence_without_post_ids_is_not_grounded() {
        let bare = Evidence {
            claim: "shorter is better".into(),
            post_ids: Vec::new(),
            metric: None,
            delta: None,
        };
        assert!(!bare.is_grounded());
        let backed = Evidence {
            post_ids: vec!["post-9".into()],
            ..bare
        };
        assert!(backed.is_grounded());
    }

    #[test]
    fn inputs_read_as_thin_rather_than_silent() {
        assert!(Inputs::default().is_empty());
        assert!(Inputs::default().summary().contains("0 llm step(s)"));
        let some = Inputs {
            captions_edited: 1,
            ..Inputs::default()
        };
        assert!(!some.is_empty());
    }

    #[test]
    fn a_report_round_trips_with_its_review_state() {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-reflect-{}-rt",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let saved = report(vec![rewrite(true, Some(true))]);
        save(&dir, &saved).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back, saved);
        assert!(back.reflection.rewrite[0].approved);
        assert!(back.reflection.rewrite[0].validation.as_ref().unwrap().ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review state is ours, not the model's — it must not appear in the schema
    /// the extractor asks the model to fill.
    #[test]
    fn the_model_schema_omits_review_state() {
        let schema = serde_json::to_string(&schemars::schema_for!(Reflection)).unwrap();
        assert!(schema.contains("preamble"));
        assert!(schema.contains("post_ids"));
        assert!(!schema.contains("approved"));
        assert!(!schema.contains("validation"));
    }
}
