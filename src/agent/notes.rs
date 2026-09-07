use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::notes::{Chapter, NotesData};

use super::prompt;

pub const SYSTEM: &str = "You turn a rehearsal transcript into a speaking-notes slide deck \
the presenter will read while recording the take that ships.\n\n\
Output a JSON object with a `chapters` array. One chapter per feature or beat — \
not a dump of the transcript, and not one blob for the whole video.\n\n\
Each chapter:\n\
- title: 2-5 words naming the feature.\n\
- points: the beats to hit, as short fragments. A glance has to be enough. Aim for 3-5; never more than 6.\n\
- verbatim: only where the exact words matter. Omit otherwise.\n\
- cues: delivery notes earned by what the rehearsal did wrong. Omit if there is nothing to say.\n\n\
This is a teleprompter. Tighten the rambling, keep what landed, drop whatever was said twice.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct NotesExtraction {
    #[serde(alias = "slides")]
    chapters: Vec<ExtractedChapter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedChapter {
    title: String,
    #[serde(default)]
    points: Vec<String>,
    #[serde(default)]
    verbatim: Option<String>,
    #[serde(default)]
    cues: Vec<String>,
}

impl NotesExtraction {
    fn into_notes(self, title: &str, version: Option<u32>) -> NotesData {
        NotesData {
            title: title.to_string(),
            version,
            chapters: self
                .chapters
                .into_iter()
                .filter_map(|chapter| {
                    let title = chapter.title.trim().to_string();
                    if title.is_empty() {
                        return None;
                    }
                    Some(Chapter {
                        title,
                        points: clean_list(chapter.points),
                        verbatim: chapter
                            .verbatim
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                        cues: clean_list(chapter.cues),
                    })
                })
                .collect(),
        }
    }
}

fn clean_list(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn user_prompt(chapters: &[(u32, String)], title: &str, extra: Option<&str>) -> String {
    let mut lines = vec![format!("Video: {title}")];
    if let Some(extra) = extra.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("\nFocus:\n{extra}"));
    }
    for (n, text) in chapters {
        lines.push(format!("\n<Chapter {n}>\n{text}"));
    }
    lines.join("\n")
}

#[tracing::instrument(skip(chapters, extra_prompt, prompt_root), fields(chapters = chapters.len(), model, provider))]
pub fn extract_notes(
    chapters: &[(u32, String)],
    title: &str,
    version: Option<u32>,
    model: &str,
    provider: Option<&str>,
    extra_prompt: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(NotesData, super::trace::LlmStep)> {
    let preamble = prompt::resolve(prompt::NOTES, SYSTEM, prompt_root);
    let prompt = user_prompt(chapters, title, extra_prompt);
    let (extracted, step) = super::extract::extract::<NotesExtraction>(
        prompt::NOTES,
        "notes",
        &preamble,
        prompt,
        model,
        provider,
    )?;
    let data = extracted.into_notes(title, version);
    if data.chapters.is_empty() {
        bail!("OpenRouter returned no chapters");
    }
    Ok((data, step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_prompt_concats_every_chapter() {
        let prompt = user_prompt(
            &[(1, "hello there".into()), (2, "the fix".into())],
            "Demo",
            Some("keep the intro tight"),
        );
        assert!(prompt.contains("Video: Demo"));
        assert!(prompt.contains("Focus:"));
        assert!(prompt.contains("keep the intro tight"));
        assert!(prompt.contains("<Chapter 1>"));
        assert!(prompt.contains("hello there"));
        assert!(prompt.contains("<Chapter 2>"));
    }

    #[test]
    fn into_notes_keeps_optional_fields_and_drops_empty_titles() {
        let extracted = NotesExtraction {
            chapters: vec![
                ExtractedChapter {
                    title: "  The hook  ".into(),
                    points: vec!["open".into(), "  ".into()],
                    verbatim: Some(" Say this ".into()),
                    cues: vec!["slow".into()],
                },
                ExtractedChapter {
                    title: "   ".into(),
                    points: vec!["gone".into()],
                    verbatim: None,
                    cues: vec![],
                },
            ],
        };
        let data = extracted.into_notes("Demo", Some(2));
        assert_eq!(data.version, Some(2));
        assert_eq!(data.chapters.len(), 1);
        assert_eq!(data.chapters[0].title, "The hook");
        assert_eq!(data.chapters[0].verbatim.as_deref(), Some("Say this"));
        assert_eq!(data.chapters[0].cues, vec!["slow"]);
        assert_eq!(data.chapters[0].points, vec!["open"]);
    }

    #[test]
    fn slides_alias_deserializes() {
        let extracted: NotesExtraction =
            serde_json::from_str(r#"{"slides":[{"title":"Hi","points":["a"]}]}"#).unwrap();
        assert_eq!(extracted.chapters[0].title, "Hi");
    }
}
