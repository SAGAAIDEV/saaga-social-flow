//! Fixing a draft the CMS would refuse, by the model that wrote it.
//!
//! [`super::limits::article`] says which fields are past a limit. This asks the
//! model to rewrite exactly those — shorter, same meaning, same voice — and
//! writes back each answer that fits. It is a rewrite of the fields named and
//! nothing else, deliberately: a draft that has been read and approved is not
//! regenerated over a pull quote, and the model is not shown a way to change
//! anything but the fields it is asked about.
//!
//! Two attempts, then the cut: whatever is still over after the model has been
//! told twice is removed the way `into_article` removes a block that would
//! publish as damage, and said out loud. A pull quote is optional; a term past
//! the length was never a search term.
//!
//! Runs on a fresh draft as it is written, and on demand from the Blog tab for
//! a draft already on disk — the same tab that lists the fields for editing by
//! hand, for whoever would rather choose the words.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::limits::{self, Violation};
use super::schema::{Article, Block};

pub const PROMPT_ID: &str = "blog.repair";

pub const SYSTEM_PROMPT: &str = r#"You shorten fields of an article that are over their length limits.

You are given the article's title and description for context, then each field
that is over its limit: its name, the limit, and its current text. For each one,
return the same point in fewer characters — within the limit, in the same voice,
as finished prose. Never truncate and never end on an ellipsis. A quote stays
something the speaker could have said. A search term stays a phrase someone
would type. A long description is the standfirst under the heading: one sentence
that says what the video shows and who it is for.

Return every field you were given, under its exact `field` name, and nothing
else. Do not touch fields you were not given."#;

/// How many times the model is asked before what is still over is cut. The
/// first answer fits nearly always, and one that has been told twice is not
/// going to be told better a third time.
pub const ATTEMPTS: usize = 2;

/// What the model returns: one rewrite per field it was given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Repairs {
    repairs: Vec<Repair>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Repair {
    /// The field name exactly as given.
    field: String,
    /// The rewritten text, within the limit.
    text: String,
}

/// What a repair did: the model calls it took, and one line per change.
#[derive(Debug, Default)]
pub struct Outcome {
    pub steps: Vec<crate::agent::trace::LlmStep>,
    pub changed: Vec<String>,
}

/// Brings `article` within the CMS's limits, in place.
///
/// `status` is told before each model call, since one takes seconds. Returns
/// what changed, in words, for the log and the status line; an empty list means
/// the draft was already within limits and nothing was sent anywhere.
pub fn to_limits(
    article: &mut Article,
    model: &str,
    provider: Option<&str>,
    status: &dyn Fn(String),
) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    for attempt in 1..=ATTEMPTS {
        let fixable = repairable(&limits::article(article));
        if fixable.is_empty() {
            break;
        }
        status(format!(
            "Shortening {} field(s) the CMS would refuse ({attempt} of {ATTEMPTS})…",
            fixable.len()
        ));
        let preamble = crate::agent::prompt::builtin_version(SYSTEM_PROMPT);
        let (repairs, step) = crate::agent::extract::extract::<Repairs>(
            PROMPT_ID,
            "blog-repair",
            &preamble,
            repair_prompt(article, &fixable),
            model,
            provider,
        )?;
        outcome.steps.push(step);
        outcome
            .changed
            .extend(absorb(article, &fixable, repairs.repairs));
    }
    outcome.changed.extend(cut(article));
    Ok(outcome)
}

/// The violations a rewrite can fix: a length, on a field the model wrote.
/// An enum value or a blank required field is a different kind of wrong.
fn repairable(found: &[Violation]) -> Vec<Violation> {
    found
        .iter()
        .filter(|violation| violation.target.is_some() && violation.limit.is_some())
        .cloned()
        .collect()
}

/// The context and the fields, one block each, ending with what to return.
fn repair_prompt(article: &Article, fixable: &[Violation]) -> String {
    let mut out = format!(
        "Title: {}\nDescription: {}\n\nFields over their limit:\n",
        article.title, article.description
    );
    for violation in fixable {
        let (Some(target), Some(limit)) = (violation.target, violation.limit) else {
            continue;
        };
        let current = limits::value_of(article, target).unwrap_or_default();
        out.push_str(&format!(
            "\nfield: {}\nlimit: {limit} characters\ncurrent ({} characters): {current}\n",
            violation.field,
            current.chars().count()
        ));
    }
    out.push_str(
        "\nReturn every field above, under its exact `field` name, rewritten to fit its limit.",
    );
    out
}

/// Writes back each repair that names a field it was asked about and fits.
///
/// Anything else — a field that was not asked about, an empty answer, one still
/// over — is left alone, for the next attempt or the cut. Says what changed.
fn absorb(article: &mut Article, fixable: &[Violation], repairs: Vec<Repair>) -> Vec<String> {
    let mut said = Vec::new();
    for repair in repairs {
        let Some(violation) = fixable
            .iter()
            .find(|violation| violation.field == repair.field.trim())
        else {
            continue;
        };
        let (Some(target), Some(limit)) = (violation.target, violation.limit) else {
            continue;
        };
        let text = repair.text.trim();
        if text.is_empty() || !limits::fits(text, limit) {
            continue;
        }
        let before = limits::value_of(article, target)
            .map(|current| current.chars().count())
            .unwrap_or_default();
        if limits::set(article, target, text) {
            said.push(format!(
                "shortened {} from {before} to {} characters",
                target.label(),
                text.chars().count()
            ));
        }
    }
    said
}

/// What is still over after the model has been asked: cut, and said out loud.
///
/// The same policy as `into_article`'s drops — a block that would publish as
/// damage goes, and the article stands without it. A quote is the one block the
/// CMS bounds that the model writes freely; a keyword target past the term
/// length was never a search term; notes and a heading are clipped at a word.
pub fn cut(article: &mut Article) -> Vec<String> {
    use limits::{fits, COLUMN, NOTES, TERM, TITLE};

    let mut said = Vec::new();
    article.blocks.retain(|block| match block {
        Block::Quote { text, .. } if !fits(text, COLUMN) => {
            said.push(format!(
                "dropped a quote the CMS would refuse ({} characters, {COLUMN} allowed): {:?}…",
                text.chars().count(),
                clipped(text, 60)
            ));
            false
        }
        _ => true,
    });
    article.keyword_targets.retain(|target| {
        let kept = fits(&target.term, TERM);
        if !kept {
            said.push(format!(
                "dropped keyword target {:?}… ({} characters, {TERM} allowed)",
                clipped(&target.term, 40),
                target.term.chars().count()
            ));
        }
        kept
    });
    for target in &mut article.keyword_targets {
        if !fits(&target.notes, NOTES) {
            target.notes = clipped(&target.notes, NOTES);
        }
    }
    if !fits(&article.h1, TITLE) {
        article.h1 = clipped(&article.h1, TITLE);
        said.push(format!("clipped the heading to {TITLE} characters"));
    }
    // Dropping the primary must not leave a brief with none — the rule the
    // generator enforces, kept here.
    if !article.keyword_targets.is_empty()
        && !article
            .keyword_targets
            .iter()
            .any(|target| target.priority == "primary")
    {
        article.keyword_targets[0].priority = "primary".to_string();
    }
    said
}

/// Character-wise so a multi-byte string is cut at a boundary, and on a word
/// boundary where there is one.
fn clipped(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    match cut.rsplit_once(' ') {
        Some((head, _)) if head.chars().count() >= max / 2 => head.trim_end().to_string(),
        _ => cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blog::limits::{Target, COLUMN};
    use crate::blog::schema::KeywordTarget;

    fn draft() -> Article {
        Article {
            title: "Why watermarking fails".into(),
            slug: "why-watermarking-fails".into(),
            description: "A complete promise.".into(),
            blocks: vec![
                Block::Text {
                    html: "<p>Body.</p>".into(),
                },
                Block::Quote {
                    text: "q".repeat(326),
                    highlight: String::new(),
                },
            ],
            keyword_targets: vec![KeywordTarget {
                term: "t".repeat(130),
                priority: "primary".into(),
                intent: String::new(),
                notes: String::new(),
            }],
            ..Article::default()
        }
    }

    /// The prompt names each field the way the violation does, with its limit
    /// and its current text, so the answer can be matched back by name.
    #[test]
    fn the_prompt_names_each_field_with_its_limit_and_text() {
        let article = draft();
        let fixable = repairable(&limits::article(&article));
        assert_eq!(fixable.len(), 2, "{fixable:?}");
        let prompt = repair_prompt(&article, &fixable);
        assert!(
            prompt.starts_with("Title: Why watermarking fails"),
            "{prompt}"
        );
        assert!(prompt.contains("field: blocks[1] (quote).text\nlimit: 255 characters\ncurrent (326 characters): qqq"), "{prompt}");
        assert!(
            prompt.contains("field: keyword_targets[0].term\nlimit: 120 characters"),
            "{prompt}"
        );
    }

    /// Only an answer that names an asked field and fits is written. The rest
    /// is left for the cut, which then says what it removed.
    #[test]
    fn fitting_repairs_are_written_and_the_rest_are_left_for_the_cut() {
        let mut article = draft();
        let fixable = repairable(&limits::article(&article));
        let said = absorb(
            &mut article,
            &fixable,
            vec![
                Repair {
                    field: "blocks[1] (quote).text".into(),
                    text: "  Shorter, and still the point.  ".into(),
                },
                Repair {
                    field: "keyword_targets[0].term".into(),
                    text: "x".repeat(121),
                },
                Repair {
                    field: "title".into(),
                    text: "Not asked about".into(),
                },
            ],
        );
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with("shortened Block 2 · quote from 326 to 29"),
            "{}",
            said[0]
        );
        assert_eq!(article.title, "Why watermarking fails");
        assert_eq!(
            limits::value_of(&article, Target::QuoteText(1)),
            Some("Shorter, and still the point.")
        );

        let cuts = cut(&mut article);
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert!(cuts[0].starts_with("dropped keyword target"), "{}", cuts[0]);
        assert!(article.keyword_targets.is_empty());
        assert!(limits::article(&article).is_empty());
    }

    /// After the model has been asked twice, a quote still over the column is
    /// the block that would have been the 500. It goes, and the log says so.
    #[test]
    fn a_quote_still_over_the_column_is_dropped_and_said() {
        let mut article = Article {
            title: "t".into(),
            blocks: vec![
                Block::Text {
                    html: "<p>a</p>".into(),
                },
                Block::Quote {
                    text: "x".repeat(COLUMN + 1),
                    highlight: String::new(),
                },
                Block::Quote {
                    text: "Kept.".into(),
                    highlight: "Kept".into(),
                },
            ],
            ..Article::default()
        };
        let said = cut(&mut article);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("256 characters"), "{}", said[0]);
        assert_eq!(article.blocks.len(), 2);
        assert!(matches!(&article.blocks[1], Block::Quote { text, .. } if text == "Kept."));
    }

    /// Dropping the primary target must not leave the brief with no primary,
    /// and notes are clipped rather than dropped.
    #[test]
    fn the_cut_keeps_a_primary_and_clips_notes() {
        let target = |term: &str, priority: &str| KeywordTarget {
            term: term.into(),
            priority: priority.into(),
            intent: String::new(),
            notes: "n".repeat(1001),
        };
        let mut article = Article {
            keyword_targets: vec![
                target(&"t".repeat(121), "primary"),
                target("short", "secondary"),
            ],
            ..Article::default()
        };
        let said = cut(&mut article);
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(article.keyword_targets.len(), 1);
        assert_eq!(article.keyword_targets[0].priority, "primary");
        assert!(article.keyword_targets[0].notes.chars().count() <= 1000);
    }

    #[test]
    fn a_draft_within_limits_is_left_alone() {
        let mut article = Article {
            blocks: vec![Block::Quote {
                text: "Fine.".into(),
                highlight: String::new(),
            }],
            ..Article::default()
        };
        assert!(cut(&mut article).is_empty());
        assert!(repairable(&limits::article(&article)).is_empty());
        assert_eq!(article.blocks.len(), 1);
    }
}
