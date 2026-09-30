//! The extractor behind [`crate::summary`]: the final video's transcript in, a
//! summary of what it says out.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::summary::{ChapterSummary, FinalChapter, Summary};

use super::prompt;

pub const MAX_TAKEAWAYS: usize = 6;
pub const MAX_SUMMARY_CHARS: usize = 900;
pub const MAX_LINE_CHARS: usize = 240;

pub const SYSTEM: &str = "You summarize a finished video from its transcript, for the \
person who made it to read before it goes out and to paste where it is described.\n\n\
Output a JSON object:\n\
- summary: 2-4 sentences on what the video shows or argues and who it helps. Plain, \
specific, in the speaker's terms. No \"In this video\".\n\
- takeaways: 3-5 things a viewer leaves with, one short sentence each.\n\
- chapters: one entry per chapter given, carrying its `n` and a one-sentence `summary` \
of what that chapter says.\n\n\
Only what the transcript says. Do not add facts, numbers or links that are not in it.";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
struct SummaryExtraction {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    takeaways: Vec<String>,
    #[serde(default)]
    chapters: Vec<ExtractedChapter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedChapter {
    n: u32,
    #[serde(default)]
    summary: String,
}

pub fn user_prompt(project: &str, chapters: &[FinalChapter]) -> String {
    let mut user = format!("Video: {project}\n");
    for chapter in chapters {
        user.push_str(&format!("\n<Chapter {}>\n", chapter.n));
        if !chapter.title.trim().is_empty() {
            user.push_str(&format!("Title: {}\n", chapter.title.trim()));
        }
        user.push_str(chapter.text.trim());
        user.push('\n');
    }
    user
}

/// Trimmed and capped; one chapter line per chapter given, in order, titled
/// as the render titled it — a chapter the model skipped is left out rather
/// than invented, and one it made up is dropped.
fn clean(raw: SummaryExtraction, chapters: &[FinalChapter]) -> Summary {
    let line = |text: &str| clip(text.trim(), MAX_LINE_CHARS);
    Summary {
        summary: clip(raw.summary.trim(), MAX_SUMMARY_CHARS),
        takeaways: raw
            .takeaways
            .iter()
            .map(|t| line(t))
            .filter(|t| !t.is_empty())
            .take(MAX_TAKEAWAYS)
            .collect(),
        chapters: chapters
            .iter()
            .filter_map(|chapter| {
                let said = raw.chapters.iter().find(|c| c.n == chapter.n)?;
                let summary = line(&said.summary);
                (!summary.is_empty()).then(|| ChapterSummary {
                    n: chapter.n,
                    title: chapter.title.trim().to_string(),
                    summary,
                })
            })
            .collect(),
        transcript_hash: String::new(),
        model: String::new(),
        created_at: String::new(),
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

#[tracing::instrument(skip(chapters, prompt_root), fields(chapters = chapters.len(), model, provider))]
pub fn summarize(
    project: &str,
    chapters: &[FinalChapter],
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(Summary, super::trace::LlmStep)> {
    let preamble = prompt::resolve(prompt::SUMMARY, SYSTEM, prompt_root);
    let (extracted, step) = super::extract::extract::<SummaryExtraction>(
        prompt::SUMMARY,
        "summary",
        &preamble,
        user_prompt(project, chapters),
        model,
        provider,
    )?;
    let summary = clean(extracted, chapters);
    if summary.summary.is_empty() {
        anyhow::bail!("the model returned no summary — press Summarize again");
    }
    Ok((summary, step))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_prompt_carries_every_chapter_and_its_title() {
        let prompt = user_prompt("Deploys", &chapters());
        assert!(prompt.contains("Video: Deploys"));
        assert!(prompt.contains("<Chapter 1>\nTitle: The hour\nDeploys took an hour."));
        assert!(prompt.contains("<Chapter 2>\nThe cache fixed it."));
    }

    #[test]
    fn chapters_follow_the_video_and_invented_ones_are_dropped() {
        let raw = SummaryExtraction {
            summary: "  How deploys got fast.  ".into(),
            takeaways: (0..9)
                .map(|i| format!("Takeaway {i}"))
                .chain(["  ".into()])
                .collect(),
            chapters: vec![
                ExtractedChapter {
                    n: 9,
                    summary: "Made up.".into(),
                },
                ExtractedChapter {
                    n: 2,
                    summary: "The fix.".into(),
                },
            ],
        };
        let summary = clean(raw, &chapters());
        assert_eq!(summary.summary, "How deploys got fast.");
        assert_eq!(summary.takeaways.len(), MAX_TAKEAWAYS);
        assert_eq!(summary.chapters.len(), 1, "chapter 1 skipped, 9 invented");
        assert_eq!(summary.chapters[0].n, 2);
        assert_eq!(summary.chapters[0].summary, "The fix.");
    }

    #[test]
    fn a_long_line_is_cut_on_a_word_and_says_so() {
        let long = "word ".repeat(100);
        let cut = clip(long.trim(), MAX_LINE_CHARS);
        assert!(cut.chars().count() <= MAX_LINE_CHARS + 1);
        assert!(cut.ends_with("word…"));
    }
}
