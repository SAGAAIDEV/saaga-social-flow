//! The extractor behind [`crate::critique`]: what was planned and what was
//! actually said in the take, in; a critique of every chapter and how to
//! reorganize them for the next take, out.

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::critique::{ChapterCritique, Critique};
use crate::plan::schema::PlanBody;
use crate::summary::FinalChapter;

use super::prompt;

pub const MAX_LINE_CHARS: usize = 400;

pub const SYSTEM: &str = "You are a frank, practical video coach. You get the \
transcript of a take the author just recorded, chapter by chapter, what they planned \
to say when there was a plan, and their direction for the next take. Critique it so \
the next take is better.\n\n\
Output a JSON object:\n\
- overall: 2-4 sentences on how the take works as a whole — does the hook hook, does \
the argument build, does the ask land.\n\
- chapters: one entry per chapter given, carrying its `n`, `worked` (what to keep, one \
sentence) and `fix` (the single most useful change, one sentence, concrete: what to \
cut, move, say sooner or say plainer).\n\
- reorganize: how to reorder, merge, split or cut the chapters for the next take, \
following the author's direction — a short paragraph that names the chapters.\n\n\
Judge what was said, not the delivery you cannot hear. Keep the author's ideas and \
examples; do not invent facts.";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
struct CritiqueExtraction {
    #[serde(default)]
    overall: String,
    #[serde(default)]
    chapters: Vec<ExtractedChapter>,
    #[serde(default)]
    reorganize: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedChapter {
    n: u32,
    #[serde(default)]
    worked: String,
    #[serde(default)]
    fix: String,
}

pub fn user_prompt(
    project: &str,
    chapters: &[FinalChapter],
    planned: Option<&PlanBody>,
    direction: &str,
) -> String {
    let mut user = format!("Video: {project}\n");
    let direction = direction.trim();
    user.push_str("\nAuthor's direction for the next take:\n");
    user.push_str(if direction.is_empty() {
        "(none — critique it on its own terms)"
    } else {
        direction
    });
    user.push('\n');
    for chapter in chapters {
        user.push_str(&format!("\n<Chapter {}>\n", chapter.n));
        if !chapter.title.trim().is_empty() {
            user.push_str(&format!("Title: {}\n", chapter.title.trim()));
        }
        if let Some(plan) = planned.and_then(|plan| plan.chapter(chapter.n)) {
            user.push_str(&format!("Planned: {}", plan.title.trim()));
            if !plan.points.is_empty() {
                user.push_str(&format!(" — {}", plan.points.join("; ")));
            }
            user.push('\n');
        }
        user.push_str("Said:\n");
        user.push_str(chapter.text.trim());
        user.push('\n');
    }
    user
}

/// Trimmed and clipped; one entry per chapter given, in order — a chapter the
/// model skipped is left out, one it made up is dropped.
fn clean(raw: CritiqueExtraction, chapters: &[FinalChapter]) -> Critique {
    let line = |text: &str| clip(text.trim(), MAX_LINE_CHARS);
    Critique {
        overall: clip(raw.overall.trim(), 1200),
        chapters: chapters
            .iter()
            .filter_map(|chapter| {
                let said = raw.chapters.iter().find(|c| c.n == chapter.n)?;
                let (worked, fix) = (line(&said.worked), line(&said.fix));
                (!worked.is_empty() || !fix.is_empty()).then(|| ChapterCritique {
                    n: chapter.n,
                    title: chapter.title.trim().to_string(),
                    worked,
                    fix,
                })
            })
            .collect(),
        reorganize: clip(raw.reorganize.trim(), 1500),
        ..Critique::default()
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(space) if space > max / 2 => format!("{}…", cut[..space].trim_end()),
        _ => format!("{}…", cut.trim_end()),
    }
}

#[tracing::instrument(skip_all, fields(chapters = chapters.len(), model, provider))]
pub fn critique(
    project: &str,
    chapters: &[FinalChapter],
    planned: Option<&PlanBody>,
    direction: &str,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(Critique, super::trace::LlmStep)> {
    let preamble = prompt::resolve(prompt::CRITIQUE, SYSTEM, prompt_root);
    let (extracted, step) = super::extract::extract::<CritiqueExtraction>(
        prompt::CRITIQUE,
        "critique",
        &preamble,
        user_prompt(project, chapters, planned, direction),
        model,
        provider,
    )?;
    let critique = clean(extracted, chapters);
    if critique.overall.is_empty() && critique.chapters.is_empty() {
        bail!("the model returned no critique — try again");
    }
    Ok((critique, step))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::schema::{ChapterKind, PlanChapter};

    fn chapters() -> Vec<FinalChapter> {
        vec![
            FinalChapter {
                n: 1,
                title: "The hour".into(),
                text: "Deploys took an hour.".into(),
            },
            FinalChapter {
                n: 2,
                title: String::new(),
                text: "The cache fixed it.".into(),
            },
        ]
    }

    #[test]
    fn the_prompt_carries_the_direction_the_plan_and_what_was_said() {
        let planned = PlanBody {
            chapters: vec![PlanChapter {
                kind: ChapterKind::Hook,
                title: "Open cold".into(),
                goal: String::new(),
                points: vec!["An hour".into(), "Every deploy".into()],
                verbatim: None,
                cues: Vec::new(),
                show: String::new(),
                layout: None,
                est_seconds: None,
            }],
            ..PlanBody::default()
        };
        let prompt = user_prompt("Deploys", &chapters(), Some(&planned), "Lead with the demo");
        assert!(prompt.contains("direction for the next take:\nLead with the demo"));
        assert!(prompt.contains(
            "<Chapter 1>\nTitle: The hour\nPlanned: Open cold — An hour; Every deploy\nSaid:\nDeploys took an hour."
        ));
        assert!(
            prompt.contains("<Chapter 2>\nSaid:\nThe cache fixed it."),
            "no plan chapter 2"
        );
        let bare = user_prompt("Deploys", &chapters(), None, " ");
        assert!(bare.contains("(none — critique it on its own terms)"));
        assert!(!bare.contains("Planned:"));
    }

    #[test]
    fn chapters_follow_the_take_and_invented_ones_are_dropped() {
        let raw = CritiqueExtraction {
            overall: "  Strong hook, slow middle. ".into(),
            chapters: vec![
                ExtractedChapter {
                    n: 7,
                    worked: "Made up".into(),
                    fix: "Made up".into(),
                },
                ExtractedChapter {
                    n: 1,
                    worked: "The number lands.".into(),
                    fix: "Say it in the first line.".into(),
                },
            ],
            reorganize: "Merge 2 into 1.".into(),
        };
        let critique = clean(raw, &chapters());
        assert_eq!(critique.overall, "Strong hook, slow middle.");
        assert_eq!(critique.chapters.len(), 1);
        assert_eq!(critique.chapters[0].n, 1);
        assert_eq!(critique.chapters[0].title, "The hour");
        assert_eq!(critique.reorganize, "Merge 2 into 1.");
    }
}
