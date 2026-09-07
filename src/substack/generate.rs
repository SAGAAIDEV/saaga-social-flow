//! Turning the longform's transcript into notes someone types an essay from.
//!
//! The system prompt is the builtin below unless an overlay outranks it — see
//! [`crate::agent::prompt::resolve`], which now looks in the standing prompt
//! library as well as the project, so a preamble tuned once survives the next
//! recording.
//!
//! Like [`crate::posts::generate`], the output *shape* is deliberately not in
//! the prompt. It is a `JsonSchema` on [`NotesExtraction`], enforced by the
//! extractor, so an overlay can rewrite the voice and the rules without being
//! able to break parsing.
//!
//! The one instruction that is load-bearing here, and the reason this is a
//! separate prompt from `posts.social` rather than a ninth platform inside it:
//! **beats, not prose.** Social copy is graded on being sendable. This is graded
//! on being typeable from, and a finished paragraph is worse than a one-line
//! beat because it has to be deleted before writing can start.

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::longform::Longform;
use super::schema::{Link, Quote, Section, SubstackNotes};

pub const SYSTEM_PROMPT: &str = r#"You turn a recorded technical video into WRITING NOTES for a Substack essay.

You are not writing the essay. Someone types it by hand from what you return, and
anything that reads as finished prose is deleted rather than typed. So: beats, not
paragraphs. One line each, naming the thing to say and — where the transcript has
one — the concrete detail that proves it: a number, a name, a before and after, a
mistake and what it cost. A beat that could have been written without watching
this particular video is a bad beat.

Work only from the transcript. Do not invent facts, numbers, tool names, timings
or outcomes that are not in it. If the video does not support a section, return
fewer sections.

titles: 3 options. Plain and specific. No "the ultimate guide", no clickbait
  framing, no clever-colon constructions.
subtitles: 3 options. A Substack subtitle is a sentence, not a slogan.
hooks: 3 opening lines. The first sentence is the whole email preview, so each is
  a concrete claim or a scene — never a throat-clear, never "in this post I'll".
sections: 4 to 7, in the order the video makes them. The heading is a working
  heading; the writer will rewrite it. 2 to 5 beats each. Set "chapter" to the
  chapter number the material came from.
quotes: lines the speaker actually said, verbatim from the transcript, worth
  reproducing as a pull quote. Trim to a sentence or two and change nothing else.
  If nothing is quotable, return none — never paraphrase something into a quote.
close: 2 options for the last beat. What the reader does or thinks next."#;

/// What the model returns. Separate from [`SubstackNotes`] so it is never asked
/// for bookkeeping it cannot know — the version, the prompt hash, where a
/// chapter falls in the longform, or what URL the upload got.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct NotesExtraction {
    titles: Vec<String>,
    subtitles: Vec<String>,
    hooks: Vec<String>,
    sections: Vec<ExtractedSection>,
    quotes: Vec<ExtractedQuote>,
    close: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedSection {
    heading: String,
    beats: Vec<String>,
    /// Which recorded chapter this came from, so it can be checked against the
    /// video without hunting.
    chapter: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedQuote {
    text: String,
    chapter: Option<u32>,
}

impl NotesExtraction {
    /// Cleans the model's output and stamps on what only this side knows.
    ///
    /// Sections with no beats are dropped, for the reason `posts` drops captions
    /// with no body: a truncated response otherwise produces headings that look
    /// like a finished outline right up until someone tries to type from one.
    ///
    /// Quotes are trimmed of whitespace and nothing else. A quote that has been
    /// tidied is not a quote, and the whole reason it is on the page is that it
    /// is the one thing safe to reproduce without checking.
    fn into_notes(
        self,
        version: Option<u32>,
        prompt: &crate::agent::prompt::Resolved,
        timestamps: &[(u32, String)],
        links: Vec<Link>,
    ) -> SubstackNotes {
        let at = |chapter: Option<u32>| -> Option<String> {
            let n = chapter?;
            timestamps
                .iter()
                .find(|(number, _)| *number == n)
                .map(|(_, stamp)| stamp.clone())
        };

        SubstackNotes {
            version,
            prompt_version: prompt.version,
            prompt_hash: prompt.hash.clone(),
            titles: lines(self.titles),
            subtitles: lines(self.subtitles),
            hooks: lines(self.hooks),
            sections: self
                .sections
                .into_iter()
                .map(|section| Section {
                    heading: section.heading.trim().to_string(),
                    beats: lines(section.beats),
                    timestamp: at(section.chapter),
                    chapter: section.chapter,
                })
                .filter(|section| !section.beats.is_empty() && !section.heading.is_empty())
                .collect(),
            quotes: self
                .quotes
                .into_iter()
                .map(|quote| Quote {
                    text: quote.text.trim().to_string(),
                    timestamp: at(quote.chapter),
                    chapter: quote.chapter,
                })
                .filter(|quote| !quote.text.is_empty())
                .collect(),
            close: lines(self.close),
            links,
        }
    }
}

fn lines(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

pub fn generate_notes(
    longform: &Longform,
    version: Option<u32>,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(SubstackNotes, crate::agent::trace::LlmStep)> {
    if longform.chapters.is_empty() {
        bail!("no transcribed chapters to write notes from");
    }

    let system = crate::agent::prompt::resolve(
        crate::agent::prompt::SUBSTACK,
        SYSTEM_PROMPT,
        prompt_root,
    );
    let prompt = build_user_prompt(longform);

    let (extracted, step) = crate::agent::extract::extract::<NotesExtraction>(
        crate::agent::prompt::SUBSTACK,
        "substack",
        &system,
        prompt,
        model,
        provider,
    )?;

    let notes = extracted.into_notes(version, &system, &longform.timestamps, longform.links.clone());
    if notes.is_empty() {
        bail!("the model returned nothing to type from");
    }
    Ok((notes, step))
}

/// The whole longform in one prompt, chapter by chapter.
///
/// Deliberately not a summary of a summary: the essay is written from what was
/// said, so the transcript goes in whole, exactly as the Post stage already
/// sends it. Chapter numbers are labelled because the model is asked to say
/// which chapter each section and quote came from, and it cannot without them.
fn build_user_prompt(longform: &Longform) -> String {
    let mut out = format!("Video: {}\n", longform.project_title);
    if !longform.links.is_empty() {
        out.push_str("\nAlready published at:\n");
        for link in &longform.links {
            out.push_str(&format!("- {}: {}\n", link.label, link.url));
        }
    }
    for chapter in &longform.chapters {
        out.push_str(&format!("\n<Chapter {}>\n", chapter.n));
        if !chapter.title.trim().is_empty() {
            out.push_str(&format!("Working title: {}\n", chapter.title.trim()));
        }
        if !chapter.points.is_empty() {
            out.push_str("Notes:\n");
            for point in &chapter.points {
                out.push_str(&format!("- {point}\n"));
            }
        }
        if !chapter.transcript.trim().is_empty() {
            out.push_str(&format!("Transcript:\n{}\n", chapter.transcript.trim()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::longform::ChapterContext;

    fn resolved() -> crate::agent::prompt::Resolved {
        crate::agent::prompt::Resolved {
            text: "guidance".into(),
            version: Some(2),
            hash: "feedfacefeedface".into(),
        }
    }

    fn extraction() -> NotesExtraction {
        NotesExtraction {
            titles: vec!["  A title  ".into(), "   ".into()],
            subtitles: vec!["A subtitle".into()],
            hooks: vec!["A hook".into()],
            sections: vec![
                ExtractedSection {
                    heading: "Where it broke".into(),
                    beats: vec!["the retry had no ceiling".into(), "  ".into()],
                    chapter: Some(2),
                },
                ExtractedSection {
                    heading: "Truncated".into(),
                    beats: vec![],
                    chapter: Some(3),
                },
            ],
            quotes: vec![ExtractedQuote {
                text: "  I assumed the ledger would catch it.  ".into(),
                chapter: Some(2),
            }],
            close: vec!["Check what your retries cannot see.".into()],
        }
    }

    fn notes() -> SubstackNotes {
        extraction().into_notes(
            Some(3),
            &resolved(),
            &[(2, "04:12".to_string())],
            vec![Link { label: "Watch".into(), url: "https://y/1".into() }],
        )
    }

    /// The instruction the whole stage exists for. An overlay is free to rewrite
    /// the voice, but if the shipped default ever stops saying this, every run
    /// quietly starts producing an essay nobody asked for.
    #[test]
    fn the_builtin_prompt_asks_for_beats_not_prose() {
        assert!(SYSTEM_PROMPT.contains("beats, not"));
        assert!(SYSTEM_PROMPT.contains("You are not writing the essay"));
        assert!(SYSTEM_PROMPT.contains("verbatim"));
    }

    /// The shape is the extractor's job, so the prompt must not also specify it —
    /// that duplication is exactly what an overlay could break.
    #[test]
    fn the_builtin_prompt_carries_no_output_schema() {
        assert!(!SYSTEM_PROMPT.contains("Return JSON"));
        assert!(!SYSTEM_PROMPT.contains("\"items\""));
    }

    /// A truncated response used to be the failure that looked like success:
    /// headings with nothing under them read as a finished outline.
    #[test]
    fn a_section_with_no_beats_is_dropped() {
        let notes = notes();
        assert_eq!(notes.sections.len(), 1);
        assert_eq!(notes.sections[0].heading, "Where it broke");
        assert_eq!(notes.sections[0].beats, vec!["the retry had no ceiling"]);
    }

    #[test]
    fn blank_options_are_dropped_and_the_rest_trimmed() {
        assert_eq!(notes().titles, vec!["A title"]);
    }

    /// Trimmed of whitespace and nothing else. A tidied quote is not a quote,
    /// and this is the one line on the page that gets reproduced unchecked.
    #[test]
    fn a_quote_keeps_the_words_it_was_given() {
        assert_eq!(
            notes().quotes[0].text,
            "I assumed the ledger would catch it."
        );
    }

    /// The model is never asked where a chapter falls in the longform, so this
    /// is where that gets attached — and a chapter with no known offset simply
    /// has none rather than a guess.
    #[test]
    fn timestamps_are_stamped_on_here_by_chapter() {
        let notes = notes();
        assert_eq!(notes.sections[0].timestamp.as_deref(), Some("04:12"));
        assert_eq!(notes.quotes[0].timestamp.as_deref(), Some("04:12"));

        let unstamped = extraction().into_notes(Some(3), &resolved(), &[], Vec::new());
        assert_eq!(unstamped.sections[0].timestamp, None);
        assert_eq!(unstamped.sections[0].chapter, Some(2), "the chapter still stands");
    }

    #[test]
    fn the_notes_record_which_prompt_version_wrote_them() {
        let notes = notes();
        assert_eq!(notes.version, Some(3));
        assert_eq!(notes.prompt_version, Some(2));
        assert_eq!(notes.prompt_hash, "feedfacefeedface");
        assert_eq!(notes.links.len(), 1);
    }

    #[test]
    fn the_user_prompt_labels_every_chapter_and_carries_its_words() {
        let longform = Longform {
            project_title: "vd-42-retries".into(),
            chapters: vec![ChapterContext {
                n: 2,
                title: "Where it broke".into(),
                points: vec!["no ceiling".into()],
                transcript: "spoken words".into(),
                figures: Vec::new(),
            }],
            timestamps: vec![(2, "04:12".into())],
            links: vec![Link { label: "Watch".into(), url: "https://y/1".into() }],
            loose_figures: Vec::new(),
        };
        let user = build_user_prompt(&longform);
        assert!(user.contains("Video: vd-42-retries"));
        assert!(user.contains("<Chapter 2>"));
        assert!(user.contains("Working title: Where it broke"));
        assert!(user.contains("- no ceiling"));
        assert!(user.contains("spoken words"));
        assert!(user.contains("Watch: https://y/1"));
    }
}
