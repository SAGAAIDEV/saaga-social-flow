//! Where in the cut chapter each point appears.
//!
//! The model quotes the words a point begins on; AssemblyAI says when each
//! word was said; the keep-list says which of those moments survived the cut
//! and where they landed in it. This module joins the three. Every number it
//! produces is seconds into the *cut* chapter — the file the composition
//! plays — never into the raw take.
//!
//! Anchors are matched as normalised tokens in order, tolerating a dropped or
//! inserted filler word, because the two texts come from different mouths: the
//! transcriber's and the model's copy of it. A point whose anchor cannot be
//! found is not dropped — it was still said — it is placed between its
//! neighbours, and a point is never placed before the one that precedes it.

use crate::edit::compute::Edit;
use crate::notes::TranscriptWord;

use super::schema::OutlinePoint;
use super::{FIRST_POINT_SECONDS, MIN_GAP_SECONDS};

/// Anchor tokens considered, from the front. The model is asked for 4–8; a
/// longer quote only makes a match harder to find.
const ANCHOR_TOKENS: usize = 8;
/// How far past the anchor's own length a window may run, so one filler word
/// the model dropped or added does not lose the match.
const SLACK: usize = 2;
/// A point may not appear in the chapter's last moments, where it would flash
/// on and be cut off. Seconds.
const TAIL: f64 = 0.5;

/// Places `points` against the chapter's words and its cut, returning them
/// with `at` filled in, in order, monotonic, and inside the cut.
///
/// `edits` are the kept spans in playing order, raw-take milliseconds; their
/// durations sum to the cut's length. With no edits at all — a chapter that
/// somehow has words but no keep-list — the raw times are used as they are.
pub fn place(
    points: &[OutlinePoint],
    words: &[TranscriptWord],
    edits: &[Edit],
) -> Vec<OutlinePoint> {
    let tokens: Vec<String> = words.iter().map(|word| normalise(&word.text)).collect();
    let total = cut_length_seconds(edits, words);

    // Pass one: every anchor that can be found, searched forward from the
    // previous match so the points keep their speaking order.
    let mut times: Vec<Option<f64>> = Vec::with_capacity(points.len());
    let mut from = 0usize;
    for point in points {
        let found = find_anchor(&point.anchor, &tokens, from);
        match found {
            Some(idx) => {
                from = idx + 1;
                times.push(Some(cut_time(words[idx].start, edits)));
            }
            None => times.push(None),
        }
    }

    // Pass two: the unfound ones land between their neighbours — halfway when
    // both sides are known, a gap after the previous otherwise.
    let mut placed: Vec<f64> = Vec::with_capacity(points.len());
    for (i, time) in times.iter().enumerate() {
        let at = match time {
            Some(at) => *at,
            None => {
                let before = placed.last().copied().unwrap_or(0.0);
                let after = times[i + 1..].iter().find_map(|t| *t);
                match after {
                    Some(after) if after > before => (before + after) / 2.0,
                    _ => before + MIN_GAP_SECONDS,
                }
            }
        };
        placed.push(at);
    }

    // Pass three: never before the heading has arrived, never before the point
    // before it, never in the last moments of the chapter.
    let ceiling = (total - TAIL).max(FIRST_POINT_SECONDS);
    let mut floor = FIRST_POINT_SECONDS;
    points
        .iter()
        .zip(placed)
        .map(|(point, at)| {
            let at = at.max(floor).min(ceiling);
            floor = at + MIN_GAP_SECONDS;
            OutlinePoint {
                text: point.text.clone(),
                anchor: point.anchor.clone(),
                at: Some((at * 100.0).round() / 100.0),
            }
        })
        .collect()
}

/// Lower-case letters and digits only, so "Don't," and "dont" agree.
fn normalise(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The word index where `anchor` begins, searching from `from`, or `None`.
///
/// Scores each start by how many anchor tokens appear in order within a
/// window of the anchor's length plus [`SLACK`]; the best start wins if it
/// matches most of the anchor. Ties go to the earliest, which is also the
/// nearest to the previous point.
fn find_anchor(anchor: &str, tokens: &[String], from: usize) -> Option<usize> {
    let wanted: Vec<String> = anchor
        .split_whitespace()
        .map(normalise)
        .filter(|t| !t.is_empty())
        .take(ANCHOR_TOKENS)
        .collect();
    if wanted.is_empty() || from >= tokens.len() {
        return None;
    }
    let needed = if wanted.len() <= 2 {
        wanted.len()
    } else {
        (wanted.len() * 2).div_ceil(3)
    };
    let window = wanted.len() + SLACK;
    let mut best: Option<(usize, usize)> = None; // (score, index)
    for start in from..tokens.len() {
        // The first token has to be there: an anchor is where a point *starts*.
        if tokens[start] != wanted[0] {
            continue;
        }
        let end = (start + window).min(tokens.len());
        let score = in_order_matches(&wanted, &tokens[start..end]);
        if score >= needed && best.is_none_or(|(s, _)| score > s) {
            best = Some((score, start));
            if score == wanted.len() {
                break;
            }
        }
    }
    best.map(|(_, idx)| idx)
}

/// How many of `wanted` appear in `window`, in order, each token used once.
fn in_order_matches(wanted: &[String], window: &[String]) -> usize {
    let mut matched = 0;
    let mut cursor = 0;
    for token in wanted {
        if let Some(offset) = window[cursor..].iter().position(|w| w == token) {
            matched += 1;
            cursor += offset + 1;
        }
    }
    matched
}

/// Raw-take milliseconds to seconds into the cut.
///
/// Walks the kept spans in order, adding each one's length until the one that
/// contains `raw_ms`. A moment that was cut out — between two kept spans — is
/// snapped to the start of the next kept span, which is when the viewer next
/// hears anything from after it. A moment past the last span is the cut's end.
fn cut_time(raw_ms: i64, edits: &[Edit]) -> f64 {
    if edits.is_empty() {
        return raw_ms.max(0) as f64 / 1000.0;
    }
    let mut offset: i64 = 0;
    for edit in edits {
        if raw_ms < edit.start {
            return offset as f64 / 1000.0;
        }
        if raw_ms < edit.end {
            return (offset + raw_ms - edit.start) as f64 / 1000.0;
        }
        offset += edit.duration;
    }
    offset as f64 / 1000.0
}

/// The cut's length: the kept spans' durations, or the last word's end when
/// there is no keep-list at all.
fn cut_length_seconds(edits: &[Edit], words: &[TranscriptWord]) -> f64 {
    if edits.is_empty() {
        return words.last().map(|w| w.end as f64 / 1000.0).unwrap_or(0.0);
    }
    edits.iter().map(|e| e.duration).sum::<i64>() as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One word a second, each 800ms long: "hello" at 0, "there" at 1.0…
    /// Whole seconds, so a placed time reads straight off a word's index.
    fn words(text: &str) -> Vec<TranscriptWord> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| TranscriptWord {
                text: w.into(),
                start: i as i64 * 1000,
                end: i as i64 * 1000 + 800,
                confidence: 0.9,
            })
            .collect()
    }

    fn edit(start: i64, end: i64) -> Edit {
        Edit {
            index: 0,
            start,
            end,
            duration: end - start,
            text: String::new(),
            start_word_idx: 0,
            end_word_idx: 0,
            disfluency_group: 0,
            extended_silence: 0,
        }
    }

    fn point(text: &str, anchor: &str) -> OutlinePoint {
        OutlinePoint {
            text: text.into(),
            anchor: anchor.into(),
            at: None,
        }
    }

    fn keep_all(words: &[TranscriptWord]) -> Vec<Edit> {
        vec![edit(0, words.last().unwrap().end)]
    }

    /// The core of it: a verbatim anchor lands on its word, and the time is
    /// that word's start.
    #[test]
    fn an_anchor_is_placed_where_its_words_begin() {
        let w = words(
            "okay so the first thing that broke was the retry storm and then we fixed it for good",
        );
        let placed = place(
            &[
                point("The retry storm", "first thing that broke"),
                point("The fix", "and then we fixed"),
            ],
            &w,
            &keep_all(&w),
        );
        assert_eq!(placed[0].at, Some(3.0), "'first' is word 3");
        assert_eq!(placed[1].at, Some(11.0), "'and' is word 11");
    }

    /// The model copies the transcript imperfectly: punctuation, case, and a
    /// filler word it left out. The match survives all three.
    #[test]
    fn punctuation_case_and_a_dropped_filler_do_not_lose_the_match() {
        let w = words("Right, um, don't open the config yet because it is not saved ok");
        let placed = place(
            &[point("Wait on the config", "open the config, yet")],
            &w,
            &keep_all(&w),
        );
        assert_eq!(placed[0].at, Some(3.0), "'open' is word 3");
        let placed = place(
            &[point("The unsaved config", "because it's not saved")],
            &w,
            &keep_all(&w),
        );
        assert_eq!(placed[0].at, Some(7.0), "'because' is word 7");
    }

    /// Transcript times are raw-take times. Cut a span out and every word
    /// after it moves earlier by exactly that much.
    #[test]
    fn times_are_mapped_through_the_keep_list() {
        let w = words("one two three four five six seven eight nine ten");
        // Keep 0–3.8s (one..four) and 6.0–9.8s (seven..ten); cut five and six.
        let edits = vec![edit(0, 3800), edit(6000, 9800)];
        // "seven" started at 6.0s raw; 3.8s of kept audio precede its span.
        assert_eq!(cut_time(6000, &edits), 3.8);
        assert_eq!(cut_time(7000, &edits), 4.8, "a second into the span");
        // A moment inside the cut-out middle snaps to the next kept span.
        assert_eq!(cut_time(4500, &edits), 3.8);
        // Past the last span is the end of the cut.
        assert_eq!(cut_time(20_000, &edits), 7.6);
        let placed = place(
            &[point("Kept", "three four"), point("Later", "eight nine")],
            &w,
            &edits,
        );
        assert_eq!(
            placed[0].at,
            Some(FIRST_POINT_SECONDS),
            "word 2 at 2.0s, held to the floor"
        );
        assert_eq!(
            placed[1].at,
            Some(4.8),
            "'eight' at 7.0s raw is 4.8s into the cut"
        );
    }

    /// An anchor nobody can find is not a dropped point — it lands between
    /// its neighbours, and after the previous one when it is last.
    #[test]
    fn an_unfound_anchor_lands_between_its_neighbours() {
        let w = words("a b c d e f g h i j k l m n o p q r s t");
        let placed = place(
            &[
                point("First", "d e"),
                point("Unfound", "zebra quokka"),
                point("Third", "o p"),
                point("Also unfound", ""),
            ],
            &w,
            &keep_all(&w),
        );
        assert_eq!(placed[0].at, Some(3.0));
        assert_eq!(placed[1].at, Some(8.5), "halfway between 3.0 and 14.0");
        assert_eq!(placed[2].at, Some(14.0));
        assert_eq!(placed[3].at, Some(14.0 + MIN_GAP_SECONDS));
    }

    /// Order is the speaker's. A later anchor that matches earlier text is
    /// searched for *after* the previous point, and two points can never
    /// appear closer than the gap or the second would land on the first.
    #[test]
    fn points_stay_in_order_and_apart() {
        let w = words("so the plan the plan again the plan once more and done");
        let placed = place(
            &[
                point("One", "the plan"),
                point("Two", "the plan"),
                point("Three", "the plan"),
            ],
            &w,
            &keep_all(&w),
        );
        let at: Vec<f64> = placed.iter().map(|p| p.at.unwrap()).collect();
        assert_eq!(
            at[0], FIRST_POINT_SECONDS,
            "word 1 at 1.0s, held to the floor"
        );
        assert_eq!(
            at[1], 3.6,
            "word 3 at 3.0s, held to the gap after the first"
        );
        assert_eq!(
            at[2], 6.0,
            "word 6, clear of the gap, lands where it was said"
        );
    }

    /// Nothing appears in the chapter's last half second, and a point pushed
    /// past the end by the gap is held at the ceiling rather than lost.
    #[test]
    fn nothing_lands_in_the_tail_of_the_chapter() {
        let w = words("a b c d e f g h i j");
        // The cut ends 0.4s after "j" begins, so "j" is inside the 0.5s tail.
        let placed = place(&[point("Late", "j")], &w, &[edit(0, 9400)]);
        assert_eq!(placed[0].at, Some(8.9));
    }

    /// A chapter too short for the floor still places, at the floor.
    #[test]
    fn a_short_chapter_places_at_the_floor() {
        let w = words("a b c");
        let placed = place(&[point("Only", "b")], &w, &keep_all(&w));
        assert_eq!(placed[0].at, Some(FIRST_POINT_SECONDS));
    }

    #[test]
    fn no_keep_list_means_raw_times() {
        assert_eq!(cut_time(2500, &[]), 2.5);
        assert_eq!(cut_time(-5, &[]), 0.0);
        let w = words("a b c");
        assert_eq!(cut_length_seconds(&[], &w), 2.8);
    }

    #[test]
    fn find_anchor_needs_the_first_word_and_most_of_the_rest() {
        let tokens: Vec<String> = "we open the file and then we save the file"
            .split(' ')
            .map(String::from)
            .collect();
        assert_eq!(find_anchor("we save the file", &tokens, 0), Some(6));
        assert_eq!(
            find_anchor("we open a door", &tokens, 1),
            None,
            "first word alone is not a match"
        );
        assert_eq!(
            find_anchor("open the file", &tokens, 2),
            None,
            "not before 'from'"
        );
        assert_eq!(
            find_anchor("then the file", &tokens, 0),
            Some(5),
            "one dropped word"
        );
        assert_eq!(find_anchor("then nothing here matches", &tokens, 0), None);
        assert_eq!(find_anchor("", &tokens, 0), None);
    }
}
