//! What the Edit tab shows: one chapter open on a timeline, the rest on a rail.
//!
//! Read entirely off disk, like every other pane, so the tab cannot hold an opinion
//! about a take that disagrees with the files. The chapter rail is built from the
//! recorded chapters; the open chapter carries everything its timeline needs in one
//! payload — the waveform peaks, the keep-spans, and the transcript word by word.
//!
//! # The proposal is visible before anything is cut
//!
//! A chapter with no hand edit is not shown blank. Its spans are
//! [`super::compute::compute_edits`] run live against the transcript — the exact cut
//! Render *would* make. That is the point of putting this tab before Render rather than
//! after it: you adjust the proposal, then Render executes it, instead of rendering
//! first and discovering what it decided.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::compute::{compute_edits, DEFAULT_PADDING_MS};
use super::keep::{self, Keep, KeepList};
use super::waveform::{self, Peaks, PEAKS_JSON};
use crate::notes::{closed_chapter_numbers, load_transcript, TranscriptWord};
use crate::ui::file_url;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pane {
    pub chapters: Vec<Row>,
    /// The chapter being edited. `None` only when there are no chapters at all.
    pub open: Option<Chapter>,
    /// Why there is nothing to edit, when there is nothing.
    pub blocked: Option<String>,
}

/// One entry on the chapter rail.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub n: u32,
    pub label: String,
    pub source_label: String,
    pub cut_label: String,
    /// True when a hand edit is on disk — the rail marks these, because it is the
    /// difference between "Render will do what I said" and "Render will decide".
    pub edited: bool,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chapter {
    pub n: u32,
    pub label: String,
    /// The recorded take, played as the timeline's transport.
    pub video_url: String,
    pub duration_ms: i64,
    pub peaks_hz: u32,
    pub peaks: Vec<u8>,
    /// Kept spans, `[start_ms, end_ms]` — the wire format the pane posts back.
    pub spans: Vec<[i64; 2]>,
    pub words: Vec<Word>,
    pub edited: bool,
    pub source_label: String,
    pub cut_label: String,
    pub dropped_label: String,
    pub segments: usize,
    /// Anything the tab should admit to rather than leave the user guessing about —
    /// a transcript still in flight, a waveform that would not decode.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Word {
    pub text: String,
    pub start: i64,
    pub end: i64,
    /// False when this word falls in dropped time, so the pane can strike it through —
    /// reading the cut is faster than watching it.
    pub kept: bool,
}

/// Every recorded chapter, with `open` (or the first) opened for editing.
pub fn build(session_dir: &Path, edit_root: &Path, open: Option<u32>) -> Pane {
    let numbers = closed_chapter_numbers(session_dir);
    if numbers.is_empty() {
        return Pane {
            chapters: Vec::new(),
            open: None,
            blocked: Some(format!(
                "No chapters recorded yet — record one first. ({})",
                session_dir.display()
            )),
        };
    }
    // An out-of-range request falls back rather than showing an empty tab: the chapter
    // it named may have been recorded under a version that is no longer open.
    let opened = open
        .filter(|n| numbers.contains(n))
        .or_else(|| numbers.first().copied());

    let chapters: Vec<Row> = numbers
        .iter()
        .map(|&n| {
            let state = State::read(session_dir, edit_root, n);
            Row {
                n,
                label: format!("{n:02}"),
                source_label: clock(state.duration_ms),
                cut_label: clock(state.keep.kept_ms()),
                edited: state.keep.is_hand(),
                open: Some(n) == opened,
            }
        })
        .collect();

    let open = opened.map(|n| open_chapter(session_dir, edit_root, n));
    Pane {
        chapters,
        open,
        blocked: None,
    }
}

/// A chapter's keep-list and length, without the cost of its waveform.
struct State {
    keep: KeepList,
    duration_ms: i64,
    words: Vec<TranscriptWord>,
    has_transcript: bool,
}

impl State {
    fn read(session_dir: &Path, edit_root: &Path, n: u32) -> State {
        let chapter_dir = chapter_dir(edit_root, n);
        let transcript = load_transcript(session_dir, n);
        let words = transcript
            .as_ref()
            .map(|t| t.words.clone())
            .unwrap_or_default();
        let has_transcript = !words.is_empty();
        let duration_ms = duration_ms(session_dir, &chapter_dir, n);
        let keep = match keep::load(&chapter_dir) {
            Some(hand) => hand,
            // No hand edit: show what Render would do, which is the automatic cut when
            // there are words and the whole chapter when there are not. Never an empty
            // list — that would read as "everything is dropped".
            None if has_transcript => {
                let edits = compute_edits(&words, DEFAULT_PADDING_MS, DEFAULT_PADDING_MS);
                if edits.is_empty() {
                    KeepList::everything(n, duration_ms)
                } else {
                    KeepList::auto(n, duration_ms, &edits)
                }
            }
            None => KeepList::everything(n, duration_ms),
        };
        State {
            keep,
            duration_ms,
            words,
            has_transcript,
        }
    }
}

fn open_chapter(session_dir: &Path, edit_root: &Path, n: u32) -> Chapter {
    let chapter_dir = chapter_dir(edit_root, n);
    let state = State::read(session_dir, edit_root, n);
    let mut note = None;

    let peaks = match waveform::build(&mp3(session_dir, n), &chapter_dir.join(PEAKS_JSON)) {
        Ok(peaks) => peaks,
        Err(err) => {
            // The video and the word list still work without a waveform, so this is a
            // degraded tab rather than a dead one — but it has to say so, or the
            // timeline just looks empty.
            eprintln!("stream-recorder: no waveform for chapter {n:02}: {err:#}");
            note = Some(format!("No waveform for this chapter — {err}"));
            Peaks {
                hz: waveform::PEAKS_HZ,
                values: Vec::new(),
            }
        }
    };
    if note.is_none() && !state.has_transcript {
        note = Some(
            "No transcript yet, so there are no words to click and nothing was proposed \
             — the whole chapter is kept until you cut it by hand."
                .to_string(),
        );
    }

    let dropped = (state.duration_ms - state.keep.kept_ms()).max(0);
    Chapter {
        n,
        label: format!("{n:02}"),
        video_url: file_url(&video(session_dir, n)),
        duration_ms: state.duration_ms,
        peaks_hz: peaks.hz,
        peaks: peaks.values,
        spans: state
            .keep
            .spans
            .iter()
            .copied()
            .map(<[i64; 2]>::from)
            .collect(),
        words: words_with_keep(&state.words, &state.keep),
        edited: state.keep.is_hand(),
        source_label: clock(state.duration_ms),
        cut_label: clock(state.keep.kept_ms()),
        dropped_label: clock(dropped),
        segments: state.keep.spans.len(),
        note,
    }
}

fn words_with_keep(words: &[TranscriptWord], keep: &KeepList) -> Vec<Word> {
    words
        .iter()
        .map(|word| Word {
            text: word.text.clone(),
            start: word.start,
            end: word.end,
            kept: keep.overlaps(word.start, word.end),
        })
        .collect()
}

pub fn chapter_dir(edit_root: &Path, n: u32) -> PathBuf {
    edit_root.join(format!("chapter-{n:02}"))
}

/// The take the timeline plays: the composed landscape file, or the camera master when
/// this chapter was recorded before composed outputs existed.
fn video(session_dir: &Path, n: u32) -> PathBuf {
    let composed = session_dir.join(format!("chapter-{n:02}-horizontal.mp4"));
    if composed.is_file() {
        return composed;
    }
    session_dir.join(format!("chapter-{n:02}.mp4"))
}

fn mp3(session_dir: &Path, n: u32) -> PathBuf {
    session_dir.join(format!("chapter-{n:02}.mp3"))
}

/// How long the chapter runs, cheapest answer first.
///
/// A hand edit already recorded the length it was made against, and a chapter opened
/// once has cached peaks — so ffprobe is only reached for a chapter nobody has touched.
/// That matters on the rail, which asks for every chapter on every repaint.
fn duration_ms(session_dir: &Path, chapter_dir: &Path, n: u32) -> i64 {
    if let Some(list) = keep::load(chapter_dir) {
        if list.duration_ms > 0 {
            return list.duration_ms;
        }
    }
    if let Some(peaks) = waveform::cached(&chapter_dir.join(PEAKS_JSON)) {
        let ms = (peaks.seconds() * 1000.0).round() as i64;
        if ms > 0 {
            return ms;
        }
    }
    super::cut::probe_duration_seconds(&video(session_dir, n))
        .map(|seconds| (seconds * 1000.0).round() as i64)
        .unwrap_or(0)
}

/// `m:ss`, which is how long a chapter is talked about.
fn clock(ms: i64) -> String {
    let total = (ms.max(0) as f64 / 1000.0).round() as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// The whole-list edit the pane posts back, normalised and written.
///
/// Takes raw pairs because that is what arrives over the bridge — validating them into
/// a [`KeepList`] is this function's job, not the caller's.
pub fn save_spans(
    edit_root: &Path,
    session_dir: &Path,
    n: u32,
    spans: &[[i64; 2]],
) -> anyhow::Result<PathBuf> {
    let chapter_dir = chapter_dir(edit_root, n);
    let duration = duration_ms(session_dir, &chapter_dir, n);
    let list = KeepList::hand(n, duration, spans.iter().copied().map(Keep::from).collect());
    keep::save(&chapter_dir, &list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-editpane-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A chapter on disk as the recorder leaves it: a video, an mp3 and a transcript.
    fn record(session_dir: &Path, n: u32, words: &[(&str, i64, i64)]) {
        std::fs::create_dir_all(session_dir).unwrap();
        std::fs::write(session_dir.join(format!("chapter-{n:02}.mp4")), b"video").unwrap();
        std::fs::write(session_dir.join(format!("chapter-{n:02}.mp3")), b"audio").unwrap();
        let words: Vec<_> = words
            .iter()
            .map(|(text, start, end)| {
                serde_json::json!({ "text": text, "start": start, "end": end, "confidence": 1.0 })
            })
            .collect();
        let transcript = serde_json::json!({
            "status": "completed",
            "text": "",
            "words": words,
        });
        std::fs::write(
            session_dir.join(format!("chapter-{n:02}.transcript.json")),
            serde_json::to_string(&transcript).unwrap(),
        )
        .unwrap();
    }

    /// Stands in for the peaks a real chapter would have decoded, which also fixes the
    /// duration without ffprobe having anything to probe.
    fn cache_peaks(edit_root: &Path, n: u32, seconds: f64) {
        let dir = chapter_dir(edit_root, n);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(PEAKS_JSON),
            serde_json::to_string(&Peaks {
                hz: 100,
                values: vec![64; (seconds * 100.0) as usize],
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn nothing_recorded_says_so_and_names_the_folder() {
        let root = temp("empty");
        let pane = build(&root.join("drafts"), &root.join("edit"), None);
        assert!(pane.chapters.is_empty());
        assert!(pane.open.is_none());
        assert!(pane.blocked.unwrap().contains("record one first"));
    }

    /// The headline behaviour: with no hand edit, the timeline shows the cut Render
    /// would make, not an empty or a whole-chapter list.
    #[test]
    fn an_unedited_chapter_shows_the_proposed_cut() {
        let root = temp("proposed");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        record(&drafts, 1, &[("hello", 0, 500), ("um", 700, 900), ("world", 1400, 1800)]);
        cache_peaks(&edits, 1, 3.0);

        let pane = build(&drafts, &edits, None);
        let open = pane.open.unwrap();
        assert!(!open.edited, "nothing has been edited by hand");
        // The filler split it in two, exactly as compute_edits would.
        assert_eq!(open.segments, 2);
        assert_eq!(open.spans.len(), 2);
        // And the words say which of them survived.
        let dropped: Vec<&str> = open
            .words
            .iter()
            .filter(|w| !w.kept)
            .map(|w| w.text.as_str())
            .collect();
        assert_eq!(dropped, ["um"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hand_edit_wins_and_is_marked_on_the_rail() {
        let root = temp("hand");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        record(&drafts, 1, &[("hello", 0, 500), ("um", 700, 900), ("world", 1400, 1800)]);
        cache_peaks(&edits, 1, 3.0);
        save_spans(&edits, &drafts, 1, &[[0, 1000]]).unwrap();

        let pane = build(&drafts, &edits, None);
        assert!(pane.chapters[0].edited);
        let open = pane.open.unwrap();
        assert!(open.edited);
        assert_eq!(open.spans, [[0, 1000]]);
        assert_eq!(open.segments, 1);
        // "world" is outside the kept span now, so it reads as dropped.
        assert!(!open.words.iter().last().unwrap().kept);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_rail_lists_every_chapter_and_opens_the_one_asked_for() {
        let root = temp("rail");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        for n in 1..=3 {
            record(&drafts, n, &[("word", 0, 500)]);
            cache_peaks(&edits, n, 2.0);
        }
        let pane = build(&drafts, &edits, Some(2));
        assert_eq!(
            pane.chapters.iter().map(|r| r.n).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(pane.open.unwrap().n, 2);
        assert!(pane.chapters[1].open);
        assert!(!pane.chapters[0].open);

        // A chapter that is not there falls back rather than emptying the tab.
        assert_eq!(build(&drafts, &edits, Some(9)).open.unwrap().n, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A take whose transcript failed is still editable — that is the case where hand
    /// editing matters most, because nothing was proposed.
    #[test]
    fn a_chapter_with_no_transcript_keeps_everything_and_says_why() {
        let root = temp("nowords");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        std::fs::create_dir_all(&drafts).unwrap();
        std::fs::write(drafts.join("chapter-01.mp4"), b"video").unwrap();
        std::fs::write(drafts.join("chapter-01.mp3"), b"audio").unwrap();
        cache_peaks(&edits, 1, 4.0);

        let open = build(&drafts, &edits, None).open.unwrap();
        assert!(open.words.is_empty());
        assert_eq!(open.spans, [[0, 4000]]);
        assert!(open.note.unwrap().contains("no words"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_labels_report_source_cut_and_dropped_time() {
        let root = temp("labels");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        record(&drafts, 1, &[("word", 0, 500)]);
        cache_peaks(&edits, 1, 125.0);
        save_spans(&edits, &drafts, 1, &[[0, 60_000], [65_000, 125_000]]).unwrap();

        let open = build(&drafts, &edits, None).open.unwrap();
        assert_eq!(open.source_label, "2:05");
        assert_eq!(open.cut_label, "2:00");
        assert_eq!(open.dropped_label, "0:05");
        assert_eq!(open.duration_ms, 125_000);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_open_chapter_carries_the_waveform_and_a_playable_url() {
        let root = temp("payload");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        record(&drafts, 1, &[("word", 0, 500)]);
        cache_peaks(&edits, 1, 2.0);
        let open = build(&drafts, &edits, None).open.unwrap();
        assert_eq!(open.peaks.len(), 200);
        assert_eq!(open.peaks_hz, 100);
        assert!(open.video_url.starts_with("file:///"));
        assert!(open.video_url.ends_with("chapter-01.mp4"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The composed landscape file is the one to play when it exists; the camera master
    /// is the fallback for takes recorded before composed outputs.
    #[test]
    fn the_composed_landscape_file_is_preferred() {
        let root = temp("composed");
        let drafts = root.join("drafts");
        record(&drafts, 1, &[]);
        assert!(video(&drafts, 1).ends_with("chapter-01.mp4"));
        std::fs::write(drafts.join("chapter-01-horizontal.mp4"), b"v").unwrap();
        assert!(video(&drafts, 1).ends_with("chapter-01-horizontal.mp4"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_spans_normalizes_them_on_the_way_to_disk() {
        let root = temp("save");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        record(&drafts, 2, &[("word", 0, 500)]);
        cache_peaks(&edits, 2, 10.0);
        // Out of order, overlapping, and one sliver — all three are the pane's problem
        // to send and this function's problem to fix.
        save_spans(&edits, &drafts, 2, &[[5000, 6000], [0, 1000], [900, 2000], [7000, 7005]])
            .unwrap();
        let saved = keep::load(&chapter_dir(&edits, 2)).unwrap();
        assert_eq!(
            saved.spans.iter().copied().map(<[i64; 2]>::from).collect::<Vec<_>>(),
            [[0, 2000], [5000, 6000]]
        );
        assert_eq!(saved.duration_ms, 10_000);
        assert!(saved.is_hand());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clock_reads_as_minutes_and_seconds() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(5_500), "0:06");
        assert_eq!(clock(125_000), "2:05");
        assert_eq!(clock(-1), "0:00");
    }
}

