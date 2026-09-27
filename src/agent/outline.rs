//! The extractor behind [`crate::outline`]: a chapter transcript in, its
//! on-screen talking points out, each anchored to the words where it begins.
//!
//! The model is asked for *anchors*, not timestamps. A model reading a
//! transcript is good at quoting the sentence where a point starts and bad at
//! guessing when it was said; the recorder already knows when every word was
//! said, so the anchor is matched back to the word list and the time comes
//! from there — see [`crate::outline::place`]. A wrong anchor costs one point
//! landing on the wrong sentence; a wrong timestamp would have cost the same
//! with no way to check it.

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::outline::schema::{ChapterOutline, OutlinePoint};
use crate::outline::{MAX_POINTS, MAX_TEXT_CHARS};

use super::prompt;

pub const SYSTEM: &str = "You write the on-screen outline for a recorded video chapter. \
It runs beside the speaker while they talk: one line per point, appearing the moment \
they reach it, so the viewer always sees where the chapter is.\n\n\
Output a JSON object with a `chapters` array, one entry per chapter given, carrying \
the chapter's `n` and its `points` in speaking order.\n\n\
Each point:\n\
- text: the point as a viewer would skim it. 2-7 words, at most 56 characters, a \
fragment rather than a sentence, no trailing punctuation, no numbering. Name the \
thing being said, not the fact that it is being said.\n\
- anchor: the first 4-8 words of the transcript passage where the speaker begins \
this point, copied exactly as the transcript spells them — filler words and all. \
The anchor is matched back to the recording to time the point, so it must be a \
verbatim quote, never a paraphrase.\n\n\
3 to 8 points per chapter, never more than 8. Merge asides into the point they \
belong to; a point the speaker returns to later is still one point. Do not restate \
the chapter title as a point. Nothing the speaker did not say.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct OutlineExtraction {
    chapters: Vec<ExtractedChapter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedChapter {
    n: u32,
    #[serde(default)]
    points: Vec<ExtractedPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedPoint {
    text: String,
    #[serde(default)]
    anchor: String,
}

impl OutlineExtraction {
    /// One outline per chapter *asked for*, in the order asked, whatever the
    /// model returned: a chapter it skipped is an empty outline — the block
    /// renders its heading alone — rather than a hole a later index falls into.
    fn into_outlines(self, chapters: &[u32]) -> Vec<ChapterOutline> {
        chapters
            .iter()
            .map(|&n| {
                let points = self
                    .chapters
                    .iter()
                    .find(|chapter| chapter.n == n)
                    .map(|chapter| clean_points(&chapter.points))
                    .unwrap_or_default();
                ChapterOutline {
                    n,
                    points,
                    approved: false,
                    face_y: None,
                }
            })
            .collect()
    }
}

/// Trimmed, clipped to the block's line, capped at what fits the panel, and
/// never empty — enforced here rather than trusted to the prompt, because the
/// composition lays these out at a fixed size and an over-long point wraps
/// into the one under it.
fn clean_points(points: &[ExtractedPoint]) -> Vec<OutlinePoint> {
    points
        .iter()
        .filter_map(|point| {
            let text = clip(
                point.text.trim().trim_end_matches(['.', ',', ';', ':']),
                MAX_TEXT_CHARS,
            );
            if text.is_empty() {
                return None;
            }
            Some(OutlinePoint {
                text,
                anchor: point.anchor.trim().to_string(),
                at: None,
            })
        })
        .take(MAX_POINTS)
        .collect()
}

/// Character-wise, so a multi-byte point is cut at a boundary. Ends on a word
/// where it can, with no ellipsis: a point that had to be cut is one to edit,
/// and an ellipsis on screen would present the cut as intended.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(space) if space > max / 2 => cut[..space].trim_end().to_string(),
        _ => cut.trim_end().to_string(),
    }
}

/// One chapter to write points for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chapter {
    pub n: u32,
    pub transcript: String,
    /// So the points do not restate it, and so a chapter the notes already
    /// named reads as that.
    pub title: Option<String>,
    /// The approved plan's points for this chapter, empty without one. A hint,
    /// so the card on screen reads like the plan the take was recorded
    /// against; see [`user_prompt`] for why it is only that.
    pub planned: Vec<String>,
}

/// Every chapter, with its title and — when the approved plan has them — its
/// planned points.
///
/// The planned points are labelled as the plan and fenced off from the
/// transcript, and the label says what they are for: wording and order. What
/// was actually said can differ from what was planned, and an anchor is only
/// worth anything as a verbatim quote of the recording — an anchor copied from
/// the plan would match no word and leave the point untimed. So the transcript
/// stays the source of every point and every anchor, and a planned point the
/// speaker skipped is not put on screen.
pub fn user_prompt(chapters: &[Chapter], project: &str) -> String {
    let mut user = format!("Video: {project}\n");
    for chapter in chapters {
        user.push_str(&format!("\n<Chapter {}>\n", chapter.n));
        if let Some(title) = chapter
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            user.push_str(&format!("Title: {title}\n"));
        }
        let planned: Vec<&str> = chapter
            .planned
            .iter()
            .map(|point| point.trim())
            .filter(|point| !point.is_empty())
            .collect();
        if !planned.is_empty() {
            user.push_str(
                "Planned points (from the plan made before recording — a hint for the \
                 wording and order of the points, not a source: write only points the \
                 transcript below actually covers, and copy every anchor from the \
                 transcript, never from these):\n",
            );
            for point in planned {
                user.push_str(&format!("- {point}\n"));
            }
            user.push_str("Transcript:\n");
        }
        user.push_str(chapter.transcript.trim());
        user.push('\n');
    }
    user
}

#[tracing::instrument(skip(chapters, prompt_root), fields(chapters = chapters.len(), model, provider))]
pub fn extract_outline(
    chapters: &[Chapter],
    project: &str,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(Vec<ChapterOutline>, super::trace::LlmStep)> {
    if chapters.is_empty() {
        bail!("no chapters to outline");
    }
    let preamble = prompt::resolve(prompt::OUTLINE, SYSTEM, prompt_root);
    let prompt = user_prompt(chapters, project);
    let (extracted, step) = super::extract::extract::<OutlineExtraction>(
        prompt::OUTLINE,
        "outline",
        &preamble,
        prompt,
        model,
        provider,
    )?;
    let numbers: Vec<u32> = chapters.iter().map(|chapter| chapter.n).collect();
    Ok((extracted.into_outlines(&numbers), step))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(text: &str, anchor: &str) -> ExtractedPoint {
        ExtractedPoint {
            text: text.into(),
            anchor: anchor.into(),
        }
    }

    fn chapter(n: u32, transcript: &str, title: Option<&str>) -> Chapter {
        Chapter {
            n,
            transcript: transcript.into(),
            title: title.map(str::to_string),
            planned: Vec::new(),
        }
    }

    #[test]
    fn user_prompt_carries_every_chapter_and_its_title() {
        let prompt = user_prompt(
            &[
                chapter(1, "hello there", Some("The hook")),
                chapter(2, "the fix", None),
            ],
            "Demo",
        );
        assert!(prompt.contains("Video: Demo"));
        assert!(prompt.contains("<Chapter 1>\nTitle: The hook\nhello there"));
        assert!(prompt.contains("<Chapter 2>\nthe fix"));
        assert!(!prompt.contains("Title: \n"), "no title is no line");
        assert!(
            !prompt.contains("Planned points"),
            "no plan, the prompt it always was"
        );
    }

    /// The plan's points ride along as a labelled hint, between the title and
    /// the transcript, and the label says the transcript still decides what is
    /// on screen and where each anchor comes from.
    #[test]
    fn planned_points_are_a_labelled_hint_and_the_transcript_stays_the_source() {
        let mut planned = chapter(2, "so the first thing we did", Some("The fix"));
        planned.planned = vec!["Cache the build".into(), "  ".into(), "Ship it".into()];
        let prompt = user_prompt(&[planned, chapter(3, "and then", None)], "Demo");
        let two = prompt
            .split("<Chapter 3>")
            .next()
            .unwrap()
            .split("<Chapter 2>")
            .nth(1)
            .unwrap();
        assert!(two.contains("Planned points (from the plan made before recording"));
        assert!(two.contains("not a source"), "{two}");
        assert!(
            two.contains("copy every anchor from the transcript"),
            "{two}"
        );
        assert!(two.contains("- Cache the build\n- Ship it\nTranscript:\nso the first"));
        assert!(!two.contains("- \n"), "a blank planned point is dropped");
        let three = prompt.split("<Chapter 3>").nth(1).unwrap();
        assert!(
            !three.contains("Planned points"),
            "only the chapter with a plan gets the hint"
        );
    }

    /// The block lays points out at a fixed size, so the caps are enforced
    /// here, not hoped for: a long point is cut on a word, a ninth is dropped,
    /// and a blank one never reaches the screen.
    #[test]
    fn points_are_clipped_capped_and_never_blank() {
        let long = "a".repeat(30) + " " + &"b".repeat(30);
        let mut points: Vec<ExtractedPoint> = (0..10)
            .map(|i| point(&format!("point {i}."), &format!("anchor {i}")))
            .collect();
        points.insert(0, point(&long, "x"));
        points.insert(1, point("   ", "y"));
        let cleaned = clean_points(&points);
        assert_eq!(cleaned.len(), MAX_POINTS);
        assert_eq!(
            cleaned[0].text,
            "a".repeat(30),
            "cut on the word, no ellipsis"
        );
        assert_eq!(cleaned[1].text, "point 0", "trailing punctuation dropped");
        assert!(
            cleaned.iter().all(|p| p.at.is_none()),
            "timing is placed later"
        );
    }

    /// One outline per chapter asked for, in the order asked — a chapter the
    /// model skipped is empty, not missing, so the caller's zip cannot slip.
    #[test]
    fn every_requested_chapter_gets_an_outline_even_when_the_model_skips_one() {
        let extracted = OutlineExtraction {
            chapters: vec![ExtractedChapter {
                n: 3,
                points: vec![point("The retry storm", "so the first thing")],
            }],
        };
        let outlines = extracted.into_outlines(&[2, 3]);
        assert_eq!(outlines.len(), 2);
        assert_eq!(outlines[0].n, 2);
        assert!(outlines[0].points.is_empty());
        assert_eq!(outlines[1].n, 3);
        assert_eq!(outlines[1].points[0].text, "The retry storm");
        assert_eq!(outlines[1].points[0].anchor, "so the first thing");
    }

    #[test]
    fn a_multibyte_point_is_clipped_without_panicking() {
        let text = "é".repeat(80);
        assert_eq!(clip(&text, MAX_TEXT_CHARS).chars().count(), MAX_TEXT_CHARS);
    }
}
