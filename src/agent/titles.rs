use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::titles::schema::{ChapterTitle, TitlesManifest};

use super::prompt;

pub const SYSTEM: &str = "You title a recorded video and its chapters.\n\
\n\
The chapter titles appear on a title card and a vertical opener, so each must be \
2-6 words, specific, and readable at a glance.\n\
\n\
The longform title is the whole video's name. It opens the video on its own card \
and becomes the headline everywhere it is published, so it has to stand alone \
without the chapters under it: say what the video shows or argues, front-load the \
words someone would search for, and keep it under about 70 characters. It is not \
a chapter title and must not read like one.\n\
\n\
For all of them: no quotes, no trailing punctuation, no numbering.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct TitlesExtraction {
    /// The whole video's title.
    longform: String,
    chapters: Vec<ExtractedTitle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedTitle {
    n: u32,
    title: String,
}

impl TitlesExtraction {
    fn into_manifest(
        self,
        version: Option<u32>,
        fallback: &[(u32, String, Option<String>)],
        project: &str,
    ) -> TitlesManifest {
        let chapters = fallback
            .iter()
            .map(|(n, _, hint)| {
                let from_model = self.chapters.iter().find_map(|item| {
                    (item.n == *n)
                        .then(|| item.title.trim())
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                });
                ChapterTitle {
                    n: *n,
                    title: from_model
                        .or_else(|| hint.clone().filter(|s| !s.trim().is_empty()))
                        .unwrap_or_else(|| format!("Chapter {n}")),
                    approved: false,
                }
            })
            .collect();
        // The project folder's name is the fallback, the same one the opening
        // card used before there was anything better to put on it.
        let longform = self.longform.trim().to_string();
        let longform = if longform.is_empty() {
            project.trim().to_string()
        } else {
            longform
        };
        TitlesManifest {
            version,
            longform,
            chapters,
        }
    }
}

pub fn user_prompt(chapters: &[(u32, String, Option<String>)], project: &str) -> String {
    let mut user = format!("Video: {project}\n");
    for (n, transcript, hint) in chapters {
        user.push_str(&format!("\n<Chapter {n}>\n"));
        if let Some(hint) = hint.as_deref().filter(|s| !s.trim().is_empty()) {
            user.push_str(&format!("Notes title: {hint}\n"));
        }
        user.push_str(transcript.trim());
        user.push('\n');
    }
    user
}

#[tracing::instrument(skip(chapters, prompt_root), fields(chapters = chapters.len(), model, provider))]
pub fn extract_titles(
    chapters: &[(u32, String, Option<String>)],
    project: &str,
    version: Option<u32>,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(TitlesManifest, super::trace::LlmStep)> {
    if chapters.is_empty() {
        bail!("no chapters to title");
    }
    let preamble = prompt::resolve(prompt::TITLES, SYSTEM, prompt_root);
    let prompt = user_prompt(chapters, project);
    let (extracted, step) = super::extract::extract::<TitlesExtraction>(
        prompt::TITLES,
        "titles",
        &preamble,
        prompt,
        model,
        provider,
    )?;
    let manifest = extracted.into_manifest(version, chapters, project);
    if manifest.chapters.is_empty() {
        bail!("no chapter titles produced");
    }
    Ok((manifest, step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_prompt_includes_notes_hints() {
        let prompt = user_prompt(
            &[(1, "hello there".into(), Some("The Hook".into()))],
            "Demo",
        );
        assert!(prompt.contains("Video: Demo"));
        assert!(prompt.contains("<Chapter 1>"));
        assert!(prompt.contains("Notes title: The Hook"));
        assert!(prompt.contains("hello there"));
    }

    fn extraction(longform: &str) -> TitlesExtraction {
        TitlesExtraction {
            longform: longform.into(),
            chapters: vec![ExtractedTitle {
                n: 2,
                title: "  The Fix  ".into(),
            }],
        }
    }

    fn fallback() -> Vec<(u32, String, Option<String>)> {
        vec![
            (1, "aaa".into(), Some("The Hook".into())),
            (2, "bbb".into(), None),
        ]
    }

    #[test]
    fn into_manifest_keeps_fallback_order_and_fills_gaps() {
        let got = extraction("Why watermarking fails").into_manifest(
            Some(1),
            &fallback(),
            "vd-42-watermarking",
        );
        assert_eq!(got.chapters[0].title, "The Hook");
        assert_eq!(got.chapters[1].title, "The Fix");
        assert!(!got.chapters[0].approved);
        assert_eq!(got.longform, "Why watermarking fails");
    }

    /// The opening card has to say something, so a model that skipped the
    /// longform title falls back to the project's own name rather than opening
    /// the video on a blank card.
    #[test]
    fn a_missing_longform_title_falls_back_to_the_project_name() {
        let got = extraction("   ").into_manifest(None, &fallback(), "vd-42-watermarking");
        assert_eq!(got.longform, "vd-42-watermarking");
    }

    /// It is the video's name, not a chapter's — the prompt has to say so, or
    /// the model writes a seventh chapter title and the card reads like one.
    #[test]
    fn the_prompt_separates_the_videos_title_from_a_chapters() {
        assert!(SYSTEM.contains("stand alone"));
        assert!(SYSTEM.contains("must not read like one"));
        assert!(SYSTEM.contains("2-6 words"), "the chapter rule survives");
    }
}
