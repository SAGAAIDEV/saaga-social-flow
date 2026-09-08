//! Keep-list from an AssemblyAI word transcript.
//!
//! Port of `screencast.workers.analyze.compute_edits` without verbal markers
//! or the retake filter — chapters are already split on disk. Drops filler
//! tokens, splits on extended inter-word silence, then pads each kept span
//! into the surrounding silence without swallowing what was cut.

use serde::{Deserialize, Serialize};

use crate::notes::TranscriptWord;

const DISFLUENCIES: &[&str] = &["um", "uh", "hmm", "mhm", "uh-huh", "ah", "huh", "hm", "m"];
const SILENCE_FACTOR: f64 = 5.0;
pub const DEFAULT_PADDING_MS: i64 = 100;

const NUM_WORDS: &[(&str, &str)] = &[
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub index: usize,
    pub start: i64,
    pub end: i64,
    pub duration: i64,
    pub text: String,
    pub start_word_idx: usize,
    pub end_word_idx: usize,
    pub disfluency_group: i64,
    pub extended_silence: i64,
}

#[derive(Clone)]
struct Tagged {
    text: String,
    start: i64,
    end: i64,
    idx: usize,
    disfluency_group: i64,
    extended_silence: i64,
    is_disfluency: bool,
}

pub fn compute_edits(words: &[TranscriptWord], pad_start_ms: i64, pad_end_ms: i64) -> Vec<Edit> {
    if words.is_empty() {
        return Vec::new();
    }
    let tagged = tag_words(words);
    let removed: Vec<(i64, i64)> = tagged
        .iter()
        .filter(|w| w.is_disfluency)
        .map(|w| (w.start, w.end))
        .collect();
    let kept: Vec<&Tagged> = tagged.iter().filter(|w| !w.is_disfluency).collect();
    if kept.is_empty() {
        return Vec::new();
    }
    let mut bounds: Vec<(i64, i64)> = Vec::new();
    let mut segments: Vec<Edit> = Vec::new();
    for word in kept {
        if let Some(last) = segments.last_mut() {
            if last.disfluency_group == word.disfluency_group
                && last.extended_silence == word.extended_silence
            {
                last.end = word.end;
                last.end_word_idx = word.idx;
                last.text.push(' ');
                last.text.push_str(&word.text);
                if let Some(bound) = bounds.last_mut() {
                    bound.1 = word.end;
                }
                continue;
            }
        }
        bounds.push((word.start, word.end));
        segments.push(Edit {
            index: segments.len(),
            start: word.start,
            end: word.end,
            duration: word.end - word.start,
            text: word.text.clone(),
            start_word_idx: word.idx,
            end_word_idx: word.idx,
            disfluency_group: word.disfluency_group,
            extended_silence: word.extended_silence,
        });
    }
    let padded = pad_segments(&bounds, &removed, pad_start_ms, pad_end_ms);
    for (edit, (start, end)) in segments.iter_mut().zip(padded) {
        edit.start = start;
        edit.end = end;
        edit.duration = end - start;
    }
    for (i, edit) in segments.iter_mut().enumerate() {
        edit.index = i;
    }
    segments
}

fn tag_words(words: &[TranscriptWord]) -> Vec<Tagged> {
    let ngrams: Vec<String> = words.iter().map(|w| clean_ngram(&w.text)).collect();
    let mut disfluency_group = 0_i64;
    let mut tagged: Vec<Tagged> = Vec::with_capacity(words.len());
    for (idx, (word, ngram)) in words.iter().zip(ngrams.iter()).enumerate() {
        let is_disfluency = is_filler(ngram);
        if is_disfluency {
            disfluency_group += 1;
        }
        tagged.push(Tagged {
            text: word.text.clone(),
            start: word.start,
            end: word.end,
            idx,
            disfluency_group,
            extended_silence: 0,
            is_disfluency,
        });
    }
    apply_silences(&mut tagged);
    tagged
}

fn apply_silences(words: &mut [Tagged]) {
    if words.is_empty() {
        return;
    }
    let mut prev = Vec::with_capacity(words.len());
    for i in 0..words.len() {
        let gap = if i == 0 {
            0
        } else {
            (words[i].start - words[i - 1].end).max(0)
        };
        prev.push(gap);
    }
    let mean = prev.iter().sum::<i64>() as f64 / prev.len() as f64;
    if !mean.is_finite() {
        return;
    }
    let threshold = mean * SILENCE_FACTOR;
    let mut group = 0_i64;
    for (word, gap) in words.iter_mut().zip(prev) {
        if gap as f64 > threshold {
            group += 1;
        }
        word.extended_silence = group;
    }
}

pub(crate) fn clean_ngram(text: &str) -> String {
    let mut cleaned: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    for &(word, digit) in NUM_WORDS {
        if cleaned == word {
            cleaned = digit.to_string();
            break;
        }
    }
    cleaned
}

fn is_filler(ngram: &str) -> bool {
    DISFLUENCIES.iter().any(|raw| clean_ngram(raw) == ngram)
}

pub(crate) fn pad_segments(
    bounds: &[(i64, i64)],
    removed: &[(i64, i64)],
    padding_start_ms: i64,
    padding_end_ms: i64,
) -> Vec<(i64, i64)> {
    if bounds.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<(i64, i64)> = bounds.to_vec();
    ordered.sort_unstable();
    let pad_start = padding_start_ms.max(0);
    let pad_end = padding_end_ms.max(0);
    let mut starts: Vec<i64> = ordered.iter().map(|(s, _)| *s).collect();
    let mut ends: Vec<i64> = ordered.iter().map(|(_, e)| *e).collect();
    let floor_ms = 0;

    let block = blockers(removed, floor_ms, starts[0]);
    let head_room = starts[0] - block.map(|(_, e)| e).unwrap_or(floor_ms);
    starts[0] -= pad_start.min(head_room.max(0));

    for i in 1..ordered.len() {
        let lo = ends[i - 1];
        let hi = starts[i];
        let (room_left, room_right) = match blockers(removed, lo, hi) {
            Some((bs, be)) => ((bs - lo).max(0), (hi - be).max(0)),
            None => {
                let total = (hi - lo).max(0);
                let denom = pad_start + pad_end;
                let left = if denom == 0 {
                    0
                } else {
                    total * pad_end / denom
                };
                (left, total - left)
            }
        };
        ends[i - 1] += pad_end.min(room_left);
        starts[i] -= pad_start.min(room_right);
    }

    let tail_room = match blockers(removed, ends[ends.len() - 1], i64::MAX) {
        Some((bs, _)) => bs - ends[ends.len() - 1],
        None => pad_end,
    };
    let last = ends.len() - 1;
    ends[last] += pad_end.min(tail_room.max(0));

    let mut out: Vec<(i64, i64)> = Vec::with_capacity(starts.len());
    for i in 0..starts.len() {
        let floor = if i == 0 { floor_ms } else { out[i - 1].1 };
        let start = starts[i].max(floor);
        out.push((start, start.max(ends[i])));
    }
    out
}

fn blockers(removed: &[(i64, i64)], lo: i64, hi: i64) -> Option<(i64, i64)> {
    let hits: Vec<(i64, i64)> = removed
        .iter()
        .copied()
        .filter(|&(s, e)| e > lo && s < hi)
        .collect();
    if hits.is_empty() {
        return None;
    }
    Some((
        hits.iter().map(|(s, _)| *s).min().unwrap(),
        hits.iter().map(|(_, e)| *e).max().unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: i64, end: i64) -> TranscriptWord {
        TranscriptWord {
            text: text.into(),
            start,
            end,
            confidence: 1.0,
        }
    }

    #[test]
    fn empty_words_yield_no_edits() {
        assert!(compute_edits(&[], 100, 100).is_empty());
    }

    #[test]
    fn all_fillers_yield_no_edits() {
        assert!(compute_edits(&[w("um", 0, 80), w("uh", 100, 160)], 100, 100).is_empty());
    }

    #[test]
    fn a_clean_run_is_one_padded_segment() {
        let edits = compute_edits(&[w("hello", 200, 400), w("world", 450, 700)], 100, 100);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 100);
        assert_eq!(edits[0].end, 800);
        assert_eq!(edits[0].text, "hello world");
        assert_eq!(edits[0].start_word_idx, 0);
        assert_eq!(edits[0].end_word_idx, 1);
    }

    #[test]
    fn a_filler_splits_and_is_not_swallowed_by_padding() {
        let edits = compute_edits(
            &[w("hello", 0, 200), w("um", 250, 400), w("world", 500, 700)],
            100,
            100,
        );
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].text, "hello");
        assert_eq!(edits[1].text, "world");
        assert_eq!(edits[0].end, 250);
        assert_eq!(edits[1].start, 400);
        assert!(edits[0].end <= 250);
        assert!(edits[1].start >= 400);
    }

    #[test]
    fn uh_huh_is_a_filler_after_cleaning() {
        let edits = compute_edits(
            &[w("Yes", 0, 100), w("uh-huh", 120, 200), w("okay", 300, 400)],
            0,
            0,
        );
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].text, "Yes");
        assert_eq!(edits[1].text, "okay");
    }

    #[test]
    fn a_long_pause_splits_when_it_exceeds_five_times_the_mean_gap() {
        let words = vec![
            w("a", 0, 40),
            w("b", 70, 110),
            w("c", 140, 180),
            w("d", 210, 250),
            w("e", 280, 320),
            w("f", 3000, 3100),
        ];
        let edits = compute_edits(&words, 0, 0);
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].text, "a b c d e");
        assert_eq!(edits[1].text, "f");
    }

    #[test]
    fn pad_splits_a_silent_gap_without_overlap() {
        let padded = pad_segments(&[(0, 200), (500, 700)], &[], 100, 100);
        assert_eq!(padded, vec![(0, 300), (400, 800)]);
        assert!(padded[0].1 <= padded[1].0);
    }

    #[test]
    fn pad_stops_at_removed_speech() {
        let padded = pad_segments(&[(0, 200), (500, 700)], &[(250, 400)], 100, 100);
        assert_eq!(padded, vec![(0, 250), (400, 800)]);
    }

    #[test]
    fn clean_ngram_strips_punct_and_maps_digits() {
        assert_eq!(clean_ngram("Hello,"), "hello");
        assert_eq!(clean_ngram("uh-huh"), "uhhuh");
        assert_eq!(clean_ngram("Two"), "2");
    }
}
