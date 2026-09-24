use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::shorts::{Suggestion, Suggestions};

use super::prompt;

pub const SYSTEM: &str = "You read the transcript of a finished video take and pick the \
asides worth recording again as their own YouTube Short: a separate 30-60 second vertical \
video, filmed on its own, that has to work for someone who never sees the video it came from.\n\n\
Output a JSON object with a `shorts` array of 1-3 suggestions, best first. Fewer is fine; \
none is not. Pick moments with one idea that lands on its own: a surprising result, a \
sharp opinion, a before-and-after, a mistake worth avoiding. Not a summary of the video, \
and not a trailer for it.\n\n\
Each suggestion:\n\
- title: 2-6 words naming the idea.\n\
- hook: the opening line, word for word, as it should be said on camera. One sentence that \
makes a scrolling viewer stop. No greeting, no \"in this video\".\n\
- points: the beats after the hook, as short fragments. 2-4; a glance has to be enough.\n\
- why: one line on what the viewer walks away with.\n\
- chapters: the chapter numbers the idea came from.\n\n\
This is a teleprompter for a new take, not an edit of the old one: tighten, and say it \
the way it should be said, not the way it was.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ShortsExtraction {
    shorts: Vec<ExtractedShort>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedShort {
    title: String,
    hook: String,
    #[serde(default)]
    points: Vec<String>,
    #[serde(default)]
    why: String,
    #[serde(default)]
    chapters: Vec<u32>,
}

impl ShortsExtraction {
    fn into_suggestions(self, version: Option<u32>) -> Suggestions {
        Suggestions {
            version,
            shorts: self
                .shorts
                .into_iter()
                .filter_map(|short| {
                    let title = short.title.trim().to_string();
                    let hook = short.hook.trim().to_string();
                    // A short with no opening line has nothing to say first,
                    // and one with no name cannot be picked from the menu.
                    if title.is_empty() || hook.is_empty() {
                        return None;
                    }
                    Some(Suggestion {
                        title,
                        hook,
                        points: short
                            .points
                            .into_iter()
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect(),
                        why: short.why.trim().to_string(),
                        chapters: short.chapters,
                    })
                })
                .collect(),
        }
    }
}

pub fn user_prompt(chapters: &[(u32, String)], title: &str) -> String {
    let mut lines = vec![format!("Video: {title}")];
    for (n, text) in chapters {
        lines.push(format!("\n<Chapter {n}>\n{text}"));
    }
    lines.join("\n")
}

#[tracing::instrument(skip(chapters, prompt_root), fields(chapters = chapters.len(), model, provider))]
pub fn suggest_shorts(
    chapters: &[(u32, String)],
    title: &str,
    version: Option<u32>,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(Suggestions, super::trace::LlmStep)> {
    let preamble = prompt::resolve(prompt::SHORTS, SYSTEM, prompt_root);
    let prompt = user_prompt(chapters, title);
    let (extracted, step) = super::extract::extract::<ShortsExtraction>(
        prompt::SHORTS,
        "shorts",
        &preamble,
        prompt,
        model,
        provider,
    )?;
    let suggestions = extracted.into_suggestions(version);
    if suggestions.shorts.is_empty() {
        bail!("OpenRouter suggested no shorts");
    }
    Ok((suggestions, step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_prompt_carries_every_chapter() {
        let prompt = user_prompt(&[(1, "intro".into()), (3, "the trick".into())], "Demo");
        assert!(prompt.contains("Video: Demo"));
        assert!(prompt.contains("<Chapter 1>\nintro"));
        assert!(prompt.contains("<Chapter 3>\nthe trick"));
    }

    #[test]
    fn suggestions_without_a_title_or_a_hook_are_dropped() {
        let extracted: ShortsExtraction = serde_json::from_str(
            r#"{"shorts":[
                {"title":" The trick ","hook":" Stop doing this. ","points":["a"," "],"why":"x","chapters":[3]},
                {"title":"No hook","hook":"  "},
                {"title":"  ","hook":"Nameless"}
            ]}"#,
        )
        .unwrap();
        let suggestions = extracted.into_suggestions(Some(2));
        assert_eq!(suggestions.version, Some(2));
        assert_eq!(suggestions.shorts.len(), 1);
        let short = &suggestions.shorts[0];
        assert_eq!(short.title, "The trick");
        assert_eq!(short.hook, "Stop doing this.");
        assert_eq!(short.points, vec!["a"]);
        assert_eq!(short.chapters, vec![3]);
    }
}
