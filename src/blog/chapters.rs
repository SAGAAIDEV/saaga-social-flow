//! The longform's chapter timeline, in both units the CMS asks for.
//!
//! Two fields on `video-post` describe the same boundaries in different units:
//! `videoChapters[]` is whole **seconds** (the component's own description says
//! so) and the AssemblyAI-shaped `transcript` is **milliseconds**. They are
//! produced together, from one pass, so the conversion happens in one place
//! rather than twice with a chance of disagreeing.
//!
//! The offsets themselves are the risky part. Every chapter is transcribed on
//! its own, so its words start at zero — while the chapter sits minutes into the
//! finished video. [`crate::longform::offsets`] knows where each one really
//! begins, including that the first chapter has no title card in front of it.
//! Nothing downstream validates a timestamp, so a shift that is forgotten here
//! is a chapter marker that lands on the wrong sentence and never complains.

use std::path::Path;

use serde::Serialize;

use crate::longform::Longform;
use crate::notes::{ChapterTranscript, TranscriptWord};
use crate::session::Session;

/// `content.video-chapter` caps `name` at 200 characters, and Strapi rejects the
/// whole entry rather than trimming it.
const NAME_MAX: usize = 200;

const MS: f64 = 1000.0;

/// One `content.video-chapter` entry. Seconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoChapter {
    pub name: String,
    #[serde(rename = "startOffset")]
    pub start_offset: u32,
    #[serde(rename = "endOffset")]
    pub end_offset: u32,
}

/// An AssemblyAI auto-chapter. Milliseconds.
///
/// `headline` and `gist` carry the same approved chapter title: the landing
/// renders `headline || gist`, so filling only one would leave a blank heading
/// on whichever frontend reads the other.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptChapter {
    pub start: i64,
    pub end: i64,
    pub headline: String,
    pub gist: String,
    /// Omitted rather than sent empty — the reader tests truthiness.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

/// The `transcript` json column, shaped like an AssemblyAI response but in
/// longform time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transcript {
    pub status: &'static str,
    pub language_code: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<TranscriptWord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<TranscriptChapter>,
    /// Seconds, like AssemblyAI reports it.
    pub audio_duration: f64,
}

/// One chapter as this stage needs it, before any shifting.
#[derive(Debug, Clone, PartialEq)]
pub struct ChapterCut {
    pub n: u32,
    pub name: String,
    /// The cut's own length in seconds, from the rendered chapter video.
    pub seconds: f64,
    pub text: String,
    /// Chapter-local milliseconds, straight off AssemblyAI.
    pub words: Vec<TranscriptWord>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    pub chapters: Vec<VideoChapter>,
    /// `None` when nothing was said at all — an empty transcript column is
    /// better than one claiming a completed transcription of silence.
    pub transcript: Option<Transcript>,
}

/// Shifts every chapter into longform time and emits both representations.
///
/// `lead` is the longform's opening title card, `card` one chapter card. Both go
/// to [`crate::longform::offsets`], which is the single place that knows how the
/// video is laid out.
pub fn timeline(cuts: &[ChapterCut], card: f64, lead: f64) -> Timeline {
    let durations: Vec<(u32, f64)> = cuts.iter().map(|cut| (cut.n, cut.seconds)).collect();
    let starts = crate::longform::offsets(&durations, card, lead);

    let mut chapters = Vec::new();
    let mut marks = Vec::new();
    let mut words = Vec::new();
    let mut texts = Vec::new();
    let mut runs_to = 0.0f64;

    for (cut, (_, start)) in cuts.iter().zip(starts) {
        let end = start + cut.seconds;
        runs_to = runs_to.max(end);
        let name = clipped(&cut.name, NAME_MAX);
        chapters.push(VideoChapter {
            name: name.clone(),
            start_offset: seconds(start),
            end_offset: seconds(end),
        });
        marks.push(TranscriptChapter {
            start: millis(start),
            end: millis(end),
            headline: name.clone(),
            gist: name,
            summary: String::new(),
        });
        let shift = millis(start);
        words.extend(cut.words.iter().map(|word| TranscriptWord {
            text: word.text.clone(),
            start: word.start + shift,
            end: word.end + shift,
            confidence: word.confidence,
        }));
        let said = cut.text.trim();
        if !said.is_empty() {
            texts.push(said.to_string());
        }
    }

    let text = texts.join("\n\n");
    let transcript = (!text.is_empty() || !words.is_empty()).then(|| Transcript {
        status: "completed",
        language_code: "en",
        text,
        words,
        chapters: marks,
        audio_duration: (runs_to * MS).round() / MS,
    });
    Timeline { chapters, transcript }
}

/// Reads the timeline off the project.
///
/// A chapter whose cut cannot be measured takes the offsets down with it: the
/// list is cumulative, so one bad probe does not lose one marker, it silently
/// moves every later one. In that case this falls back to the flat transcript —
/// still useful to a reader and to a crawler, and honestly free of timestamps —
/// rather than shipping a timeline that is quietly three seconds out per
/// chapter.
pub fn build(session: &Session, longform: &Longform) -> Timeline {
    match cuts(&session.edit_dir(), &session.dir, longform) {
        Some(cuts) => timeline(
            &cuts,
            crate::edit::compose::CARD_SECONDS,
            crate::edit::compose::CARD_SECONDS,
        ),
        None => Timeline {
            chapters: Vec::new(),
            transcript: flat(longform),
        },
    }
}

/// `None` if any chapter that has a cut cannot be probed.
fn cuts(edit_dir: &Path, drafts: &Path, longform: &Longform) -> Option<Vec<ChapterCut>> {
    let mut out = Vec::new();
    for chapter in &longform.chapters {
        let n = chapter.n;
        let cut = edit_dir
            .join(format!("chapter-{n:02}"))
            .join(format!("chapter-{n:02}-horizontal.mp4"));
        // Not every closed chapter is cut yet. Only the cut ones are in the
        // longform, so only they get a marker — the same filter
        // `edit::compose::prepare` applies when it lays the video out.
        if !cut.is_file() {
            continue;
        }
        let seconds = match crate::edit::cut::probe_duration_seconds(&cut) {
            Ok(seconds) => seconds,
            Err(err) => {
                eprintln!("stream-recorder: no blog chapter timeline — {err:#}");
                return None;
            }
        };
        out.push(ChapterCut {
            n,
            name: chapter.title.clone(),
            seconds,
            text: chapter.transcript.clone(),
            words: words_of(drafts, n),
        });
    }
    Some(out)
}

/// The words AssemblyAI returned for one chapter.
///
/// Read through [`ChapterTranscript`] — the type `crate::notes` *writes* the file
/// with — rather than a local mirror of it. There was a mirror here, defended by
/// a comment claiming the writer's type was unreachable; it is re-exported
/// crate-wide and `edit::keep` and `edit::compute` both use it. Two structs
/// describing one file on disk is a drift waiting for whichever one nobody
/// remembers to change.
///
/// An unreadable or half-written file yields no words rather than an error: the
/// chapter still divides the video, it just has nothing to quote.
fn words_of(drafts: &Path, n: u32) -> Vec<TranscriptWord> {
    let path = drafts.join(format!("chapter-{n:02}.transcript.json"));
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<ChapterTranscript>(&text)
        .map(|parsed| parsed.words)
        .unwrap_or_default()
}

/// Words with no timeline: the text, and nothing that claims to be timed.
fn flat(longform: &Longform) -> Option<Transcript> {
    let text = longform
        .chapters
        .iter()
        .map(|chapter| chapter.transcript.trim())
        .filter(|said| !said.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then(|| Transcript {
        status: "completed",
        language_code: "en",
        text,
        words: Vec::new(),
        chapters: Vec::new(),
        audio_duration: 0.0,
    })
}

fn seconds(value: f64) -> u32 {
    value.max(0.0).round() as u32
}

fn millis(value: f64) -> i64 {
    (value.max(0.0) * MS).round() as i64
}

/// Character-wise, so a multi-byte title is cut at a boundary rather than
/// panicking on a slice.
fn clipped(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: i64, end: i64) -> TranscriptWord {
        TranscriptWord { text: text.into(), start, end, confidence: 0.9 }
    }

    fn cut(n: u32, name: &str, seconds: f64, words: Vec<TranscriptWord>) -> ChapterCut {
        ChapterCut {
            n,
            name: name.into(),
            seconds,
            text: format!("chapter {n} said something"),
            words,
        }
    }

    /// The bug this exists to stop: every chapter is transcribed on its own, so
    /// its words say `start: 0` while the chapter itself begins minutes into the
    /// video. Unshifted, every marker points at the opening — and nothing
    /// downstream would ever say so.
    #[test]
    fn a_later_chapters_words_are_shifted_into_longform_time() {
        let got = timeline(
            &[
                cut(1, "Where it broke", 60.0, vec![word("first", 0, 500)]),
                cut(2, "The fix", 120.0, vec![word("second", 0, 500)]),
            ],
            3.0,
            3.0,
        );
        let transcript = got.transcript.expect("something was said");
        // Only the 3s opening title plays before chapter one says a word.
        assert_eq!(transcript.words[0].start, 3_000);
        // + 60s of chapter one + chapter two's 3s card = 66s.
        assert_eq!(transcript.words[1].start, 66_000);
        assert_eq!(transcript.words[1].end, 66_500);
    }

    /// Seconds on the component, milliseconds in the transcript, one pass — so
    /// the two can never describe different boundaries.
    #[test]
    fn the_two_units_describe_the_same_boundaries() {
        let got = timeline(
            &[cut(1, "One", 60.0, vec![]), cut(2, "Two", 120.0, vec![])],
            3.0,
            3.0,
        );
        assert_eq!(got.chapters[0].start_offset, 3);
        assert_eq!(got.chapters[0].end_offset, 63);
        assert_eq!(got.chapters[1].start_offset, 66);
        assert_eq!(got.chapters[1].end_offset, 186);

        let marks = got.transcript.unwrap().chapters;
        assert_eq!((marks[0].start, marks[0].end), (3_000, 63_000));
        assert_eq!((marks[1].start, marks[1].end), (66_000, 186_000));
    }

    /// Chapter one sits behind the opening title and nothing else — the title
    /// card stands in for a card of its own.
    #[test]
    fn the_first_chapter_sits_behind_the_opening_title_alone() {
        let got = timeline(&[cut(1, "One", 10.0, vec![])], 3.0, 3.0);
        assert_eq!(got.chapters[0].start_offset, 3);
        assert_eq!(got.chapters[0].end_offset, 13);
    }

    /// Both halves of the heading are filled: the landing renders
    /// `headline || gist`, and a frontend reading the other one would show a
    /// blank chapter title.
    #[test]
    fn a_chapter_names_itself_in_both_fields_the_readers_check() {
        let got = timeline(&[cut(1, "Where it broke", 10.0, vec![])], 3.0, 3.0);
        let mark = &got.transcript.as_ref().unwrap().chapters[0];
        assert_eq!(mark.headline, "Where it broke");
        assert_eq!(mark.gist, "Where it broke");
        assert_eq!(got.chapters[0].name, "Where it broke");
    }

    /// Strapi rejects the whole entry over one long field rather than trimming
    /// it, so the trim happens before the request.
    #[test]
    fn a_title_too_long_for_the_component_is_cut_not_rejected() {
        let long = "x".repeat(260);
        let got = timeline(&[cut(1, &long, 10.0, vec![])], 3.0, 3.0);
        assert_eq!(got.chapters[0].name.chars().count(), NAME_MAX);
    }

    /// A multi-byte title must be cut at a character boundary — slicing bytes
    /// would panic on exactly the titles most likely to be long.
    #[test]
    fn a_multibyte_title_is_cut_without_panicking() {
        let long = "é".repeat(260);
        let got = timeline(&[cut(1, &long, 10.0, vec![])], 3.0, 3.0);
        assert_eq!(got.chapters[0].name.chars().count(), NAME_MAX);
    }

    /// An empty `transcript` column beats one asserting a completed
    /// transcription of silence.
    #[test]
    fn nothing_said_is_no_transcript_rather_than_an_empty_one() {
        let mut silent = cut(1, "One", 10.0, vec![]);
        silent.text = "   ".into();
        let got = timeline(&[silent], 3.0, 3.0);
        assert!(got.transcript.is_none());
        assert_eq!(got.chapters.len(), 1, "the chapter still has a marker");
    }

    /// A chapter whose AssemblyAI job never landed still divides the video.
    #[test]
    fn a_chapter_with_no_words_still_gets_a_marker() {
        let got = timeline(
            &[cut(1, "One", 60.0, vec![]), cut(2, "Two", 30.0, vec![word("hi", 0, 10)])],
            3.0,
            3.0,
        );
        assert_eq!(got.chapters.len(), 2);
        let transcript = got.transcript.unwrap();
        assert_eq!(transcript.words.len(), 1);
        assert_eq!(transcript.words[0].start, 66_000);
    }

    #[test]
    fn the_transcript_runs_as_long_as_the_last_chapter_ends() {
        let got = timeline(
            &[cut(1, "One", 60.0, vec![]), cut(2, "Two", 120.0, vec![])],
            3.0,
            3.0,
        );
        assert_eq!(got.transcript.unwrap().audio_duration, 186.0);
    }

    #[test]
    fn no_chapters_is_no_timeline() {
        let got = timeline(&[], 3.0, 3.0);
        assert!(got.chapters.is_empty());
        assert!(got.transcript.is_none());
    }

    /// The wire shape the CMS validates against: camelCase offsets on the
    /// component, snake_case on the transcript, and no empty `summary`.
    #[test]
    fn the_json_uses_the_names_strapi_and_assemblyai_expect() {
        let got = timeline(&[cut(1, "One", 60.0, vec![word("hi", 0, 10)])], 3.0, 3.0);
        let chapter = serde_json::to_value(&got.chapters[0]).unwrap();
        assert!(chapter.get("startOffset").is_some());
        assert!(chapter.get("endOffset").is_some());

        let transcript = serde_json::to_value(got.transcript.unwrap()).unwrap();
        assert_eq!(transcript["audio_duration"], 63.0);
        assert_eq!(transcript["language_code"], "en");
        assert_eq!(transcript["status"], "completed");
        assert!(transcript["chapters"][0].get("summary").is_none());
    }

    /// A project whose cuts cannot be measured keeps its words and loses only
    /// its timestamps — the fallback is honest rather than empty.
    #[test]
    fn an_unmeasurable_project_keeps_the_text_and_drops_the_timing() {
        use crate::longform::{ChapterContext, Longform};
        let longform = Longform {
            project_title: "A video".into(),
            chapters: vec![ChapterContext {
                n: 1,
                title: "One".into(),
                points: Vec::new(),
                transcript: "we said this".into(),
                figures: Vec::new(),
            }],
            timestamps: Vec::new(),
            loose_figures: Vec::new(),
            links: Vec::new(),
        };
        let transcript = flat(&longform).expect("the words survive");
        assert_eq!(transcript.text, "we said this");
        assert!(transcript.chapters.is_empty());
        assert!(transcript.words.is_empty(), "no timing is claimed");
    }
}
