//! The hand-edited keep-list: which parts of a recorded chapter survive the cut.
//!
//! [`super::compute`] derives a keep-list from the transcript, which is right about
//! disfluencies and knows nothing about a sentence you fluffed or a tangent you want
//! gone. This is the same shape of answer, arrived at by hand in the Edit tab, and it
//! **wins** — see [`super::edit_chapter`]. Without that precedence every press of
//! Render would recompute over the top of the edit and quietly undo it.
//!
//! # One list, one idea
//!
//! A chapter's edit is a list of spans, in source milliseconds, that are *kept*. Time
//! between spans is dropped. That is the whole model: the timeline's regions are these
//! spans, so dragging one out over the audio, dropping a selection, and adjusting a cut
//! point are all the same edit to the same list. Nothing in the pane needs a second
//! concept.
//!
//! # Where the arithmetic lives, and why it is split
//!
//! The *interactive* operations — keep this range, drop that selection, keep only what
//! is highlighted — run in the pane, in JavaScript. They have to: they happen while a
//! marquee is being dragged, and a round trip through Rust and back would either lag the
//! drag or force a repaint that throws away the zoom and the playhead.
//!
//! What lives here is the boundary: [`KeepList::normalize`], which every list goes
//! through on its way to disk. That is deliberately the *whole* guarantee — whatever the
//! pane computed, what gets written is ordered, non-overlapping, inside the chapter and
//! free of spans too short to cut. So a mistake in the pane's arithmetic can cost the
//! last action, but it cannot write a keep-list that produces a nonsense cut, and it
//! cannot be shipped by a `to_edits` that trusted it.
//!
//! Duplicating the interactive ops here as well would look more rigorous and be less
//! true — a second implementation nothing calls is not a check on the first.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::compute::Edit;
use crate::notes::TranscriptWord;

pub const KEEP_JSON: &str = "keep.json";

/// One frame at 30fps, the finest cut the pipeline can actually express.
///
/// Two jobs. A span shorter than this is a stray click rather than an edit, so it is
/// dropped; a *gap* shorter than this cannot be cut out at all, so the spans around it
/// are merged instead of leaving the renderer to round it into a one-frame stutter.
const MIN_SPAN_MS: i64 = 34;

/// Where a keep-list came from, so the tab can say whether it is showing your edit or
/// the machine's proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// As [`super::compute::compute_edits`] proposed it. Never written to disk — an
    /// unedited chapter has no file at all, which is what makes "delete the file" a
    /// complete reset.
    Auto,
    /// Edited by hand. On disk, and Render obeys it.
    Hand,
}

/// A span of the source chapter that survives, in milliseconds.
///
/// Serialised as a two-element array: this file is read by a template and a person, and
/// `[0, 41200]` says the same thing as `{"start": 0, "end": 41200}` in a quarter of the
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "[i64; 2]", into = "[i64; 2]")]
pub struct Keep {
    pub start: i64,
    pub end: i64,
}

impl Keep {
    pub fn new(start: i64, end: i64) -> Keep {
        Keep { start, end }
    }

    pub fn len(&self) -> i64 {
        (self.end - self.start).max(0)
    }
}

impl From<[i64; 2]> for Keep {
    fn from([start, end]: [i64; 2]) -> Keep {
        Keep { start, end }
    }
}

impl From<Keep> for [i64; 2] {
    fn from(keep: Keep) -> [i64; 2] {
        [keep.start, keep.end]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepList {
    /// Which chapter, so a file found on its own still says what it belongs to.
    pub chapter: u32,
    pub source: Source,
    /// The recorded length, which is what spans are clamped against. `0` means it was
    /// not known when the list was written, and nothing is clamped.
    #[serde(default)]
    pub duration_ms: i64,
    pub spans: Vec<Keep>,
}

impl KeepList {
    /// A hand list, normalised on the way in — every constructor path goes through
    /// [`KeepList::normalize`] so an un-normalised list never exists for long enough
    /// to be written or measured.
    pub fn hand(chapter: u32, duration_ms: i64, spans: Vec<Keep>) -> KeepList {
        let mut list = KeepList {
            chapter,
            source: Source::Hand,
            duration_ms,
            spans,
        };
        list.normalize();
        list
    }

    /// The proposal, from a keep-list already computed by the transcript pass.
    pub fn auto(chapter: u32, duration_ms: i64, edits: &[Edit]) -> KeepList {
        let mut list = KeepList {
            chapter,
            source: Source::Auto,
            duration_ms,
            spans: edits.iter().map(|e| Keep::new(e.start, e.end)).collect(),
        };
        list.normalize();
        list
    }

    /// The whole chapter, for a take with no transcript to propose anything.
    pub fn everything(chapter: u32, duration_ms: i64) -> KeepList {
        KeepList {
            chapter,
            source: Source::Auto,
            duration_ms,
            spans: vec![Keep::new(0, duration_ms.max(0))],
        }
    }

    pub fn is_hand(&self) -> bool {
        self.source == Source::Hand
    }

    /// Ordered, non-overlapping, inside the chapter, nothing too short to cut.
    ///
    /// Sub-frame *gaps* close rather than survive: a gap the cut cannot express would
    /// otherwise become a rounding decision inside ffmpeg, which is a stutter nobody
    /// asked for at a place nobody chose.
    pub fn normalize(&mut self) {
        let ceiling = self.duration_ms;
        for span in self.spans.iter_mut() {
            if span.start > span.end {
                std::mem::swap(&mut span.start, &mut span.end);
            }
            span.start = span.start.max(0);
            span.end = span.end.max(0);
            if ceiling > 0 {
                span.start = span.start.min(ceiling);
                span.end = span.end.min(ceiling);
            }
        }
        self.spans.sort_by_key(|span| (span.start, span.end));

        let mut merged: Vec<Keep> = Vec::with_capacity(self.spans.len());
        for span in self.spans.drain(..) {
            match merged.last_mut() {
                Some(last) if span.start - last.end < MIN_SPAN_MS => {
                    last.end = last.end.max(span.end);
                }
                _ => merged.push(span),
            }
        }
        merged.retain(|span| span.len() >= MIN_SPAN_MS);
        self.spans = merged;
    }

    pub fn kept_ms(&self) -> i64 {
        self.spans.iter().map(|span| span.len()).sum()
    }

    /// Whether any of `start..end` survives the cut.
    ///
    /// Overlap rather than containment, because the caller is asking about a word: one
    /// straddling a cut edge is still partly heard, and reporting it as dropped would
    /// strike through a word you can hear.
    pub fn overlaps(&self, start: i64, end: i64) -> bool {
        self.spans
            .iter()
            .any(|span| end > span.start && start < span.end)
    }

    /// Into the keep-list [`super::cut::cut_file`] already takes.
    ///
    /// `text` and the word indices are filled from whatever words the span covers, so
    /// the `edits.json` left beside the cut still reads as a transcript of it rather
    /// than as bare numbers. `disfluency_group` and `extended_silence` are why the
    /// *automatic* pass split where it did; a hand span has no such reason, and
    /// inventing one would be a lie in a file people read to understand a cut.
    pub fn to_edits(&self, words: &[TranscriptWord]) -> Vec<Edit> {
        self.spans
            .iter()
            .enumerate()
            .map(|(index, span)| {
                let covered: Vec<(usize, &TranscriptWord)> = words
                    .iter()
                    .enumerate()
                    .filter(|(_, word)| word.end > span.start && word.start < span.end)
                    .collect();
                let text = covered
                    .iter()
                    .map(|(_, word)| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                Edit {
                    index,
                    start: span.start,
                    end: span.end,
                    duration: span.len(),
                    text,
                    start_word_idx: covered.first().map(|(i, _)| *i).unwrap_or(0),
                    end_word_idx: covered.last().map(|(i, _)| *i).unwrap_or(0),
                    disfluency_group: 0,
                    extended_silence: 0,
                }
            })
            .collect()
    }
}

pub fn path_in(chapter_dir: &Path) -> PathBuf {
    chapter_dir.join(KEEP_JSON)
}

/// The hand edit for this chapter, if there is one.
///
/// A file that will not parse is reported and ignored rather than failing the cut: the
/// worst case is a render from the automatic keep-list, which is recoverable, and the
/// alternative is a chapter that cannot be rendered at all until someone edits JSON.
pub fn load(chapter_dir: &Path) -> Option<KeepList> {
    let path = path_in(chapter_dir);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<KeepList>(&text) {
        Ok(mut list) => {
            list.normalize();
            Some(list)
        }
        Err(err) => {
            eprintln!(
                "stream-recorder: ignoring unreadable {}: {err}",
                path.display()
            );
            None
        }
    }
}

/// Writes the edit, refusing to write one that would produce no video.
pub fn save(chapter_dir: &Path, list: &KeepList) -> Result<PathBuf> {
    if list.spans.is_empty() {
        bail!(
            "refusing to save an empty keep-list for chapter {:02} — that is a chapter \
             with nothing in it, not an edit",
            list.chapter
        );
    }
    std::fs::create_dir_all(chapter_dir)
        .with_context(|| format!("creating {}", chapter_dir.display()))?;
    let path = path_in(chapter_dir);
    let body = serde_json::to_string_pretty(list).context("serializing the keep-list")? + "\n";
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Back to the automatic cut. Absence *is* the reset, so there is no "auto" file to
/// write and no third state to get out of step.
pub fn clear(chapter_dir: &Path) -> Result<()> {
    let path = path_in(chapter_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(spans: &[[i64; 2]]) -> KeepList {
        KeepList::hand(1, 10_000, spans.iter().copied().map(Keep::from).collect())
    }

    fn spans(list: &KeepList) -> Vec<[i64; 2]> {
        list.spans.iter().copied().map(<[i64; 2]>::from).collect()
    }

    fn word(text: &str, start: i64, end: i64) -> TranscriptWord {
        TranscriptWord {
            text: text.into(),
            start,
            end,
            confidence: 1.0,
        }
    }

    #[test]
    fn normalize_orders_and_merges_overlapping_spans() {
        let list = list(&[[4000, 6000], [0, 1000], [500, 2000]]);
        assert_eq!(spans(&list), [[0, 2000], [4000, 6000]]);
    }

    /// Two spans that meet exactly are one span. Left as two, the cut would encode a
    /// seam in the middle of continuous speech.
    #[test]
    fn touching_spans_become_one() {
        assert_eq!(spans(&list(&[[0, 1000], [1000, 2000]])), [[0, 2000]]);
    }

    /// A gap the cut cannot express is closed here rather than rounded inside ffmpeg.
    #[test]
    fn a_sub_frame_gap_closes() {
        assert_eq!(spans(&list(&[[0, 1000], [1020, 2000]])), [[0, 2000]]);
        // One frame or more is a real cut and survives.
        assert_eq!(
            spans(&list(&[[0, 1000], [1040, 2000]])),
            [[0, 1000], [1040, 2000]]
        );
    }

    #[test]
    fn spans_are_clamped_to_the_chapter_and_slivers_dropped() {
        let list = list(&[[-500, 1000], [9000, 99_000], [5000, 5010]]);
        assert_eq!(spans(&list), [[0, 1000], [9000, 10_000]]);
    }

    #[test]
    fn a_reversed_span_is_read_the_way_it_was_dragged() {
        assert_eq!(spans(&list(&[[2000, 500]])), [[500, 2000]]);
    }

    /// The guarantee the pane's arithmetic is allowed to lean on: whatever it posts,
    /// what lands on disk is a list that can be cut. This is the case that matters —
    /// a pane that produced overlapping spans would otherwise cut the same footage
    /// twice and lengthen the chapter it was asked to shorten.
    #[test]
    fn a_list_the_pane_got_wrong_is_still_written_sane() {
        let mangled = list(&[[3000, 1000], [900, 2000], [0, 1000], [7000, 7010]]);
        let out = spans(&mangled);
        assert_eq!(out, [[0, 3000]]);
        for pair in out.windows(2) {
            assert!(pair[0][1] <= pair[1][0], "no overlap survives");
        }
        assert_eq!(mangled.kept_ms(), 3000);
    }

    #[test]
    fn overlaps_reports_a_word_that_is_partly_heard() {
        let list = list(&[[1000, 2000]]);
        assert!(list.overlaps(1200, 1400), "inside");
        assert!(
            list.overlaps(900, 1100),
            "straddling the start is still heard"
        );
        assert!(list.overlaps(1900, 2100), "straddling the end too");
        assert!(
            !list.overlaps(2000, 2500),
            "touching the end is not overlapping"
        );
        assert!(!list.overlaps(0, 1000), "nor is touching the start");
    }

    #[test]
    fn kept_ms_is_the_sum_of_the_spans_not_the_chapter() {
        let list = list(&[[0, 1000], [2000, 2500]]);
        assert_eq!(list.kept_ms(), 1500);
        assert_eq!(list.duration_ms, 10_000);
    }

    #[test]
    fn everything_keeps_the_whole_chapter() {
        let list = KeepList::everything(2, 12_000);
        assert_eq!(spans(&list), [[0, 12_000]]);
        assert!(!list.is_hand());
    }

    #[test]
    fn an_auto_list_is_built_from_computed_edits() {
        let words = [
            word("hello", 200, 400),
            word("um", 600, 700),
            word("world", 1200, 1500),
        ];
        let edits = super::super::compute::compute_edits(&words, 100, 100);
        let list = KeepList::auto(1, 10_000, &edits);
        assert_eq!(list.spans.len(), edits.len());
        assert!(!list.is_hand());
    }

    #[test]
    fn to_edits_names_the_words_each_span_covers() {
        let words = [
            word("the", 0, 300),
            word("quick", 300, 700),
            word("brown", 4000, 4400),
        ];
        let edits = list(&[[0, 1000], [3900, 5000]]).to_edits(&words);
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].text, "the quick");
        assert_eq!(edits[0].start_word_idx, 0);
        assert_eq!(edits[0].end_word_idx, 1);
        assert_eq!(edits[1].text, "brown");
        assert_eq!(edits[1].start_word_idx, 2);
        // Indices are the position in the cut, and durations agree with the spans.
        assert_eq!(edits[1].index, 1);
        assert_eq!(edits[1].duration, 1100);
    }

    /// A span over silence is still a span. Empty text must not become an empty cut.
    #[test]
    fn a_span_with_no_words_still_becomes_an_edit() {
        let edits = list(&[[0, 1000]]).to_edits(&[]);
        assert_eq!(edits.len(), 1);
        assert!(edits[0].text.is_empty());
        assert_eq!(edits[0].duration, 1000);
    }

    #[test]
    fn save_load_round_trips_and_clear_removes() {
        let dir =
            std::env::temp_dir().join(format!("stream-recorder-keep-{}-round", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let list = list(&[[0, 1000], [2000, 3000]]);
        let path = save(&dir, &list).unwrap();
        assert!(path.ends_with(KEEP_JSON));
        let back = load(&dir).unwrap();
        assert_eq!(back, list);
        assert!(back.is_hand());
        clear(&dir).unwrap();
        assert!(load(&dir).is_none());
        // Clearing what is already clear is not an error — Reset is idempotent.
        clear(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_keep_list_is_refused_rather_than_saved() {
        let dir =
            std::env::temp_dir().join(format!("stream-recorder-keep-{}-empty", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let empty = KeepList::hand(3, 5000, Vec::new());
        let err = save(&dir, &empty).unwrap_err().to_string();
        assert!(err.contains("nothing in it"), "{err}");
        assert!(load(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt file must degrade to the automatic cut, not block the chapter.
    #[test]
    fn an_unparseable_keep_file_is_ignored() {
        let dir =
            std::env::temp_dir().join(format!("stream-recorder-keep-{}-bad", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path_in(&dir), "{ not json").unwrap();
        assert!(load(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The wire format the pane posts and the template reads.
    #[test]
    fn spans_serialize_as_pairs() {
        let json = serde_json::to_string(&list(&[[0, 1000]])).unwrap();
        assert!(json.contains(r#""spans":[[0,1000]]"#), "{json}");
        assert!(json.contains(r#""source":"hand""#), "{json}");
    }
}
