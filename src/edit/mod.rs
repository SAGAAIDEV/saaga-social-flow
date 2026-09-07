//! Post-record cut & render.
//!
//! Render runs both stages: disfluency cut into `edit/vN/`, then HyperFrames
//! compose/render into `compose/vN/` and `render/vN/`.
//!
//! The keep-list a chapter is cut to comes from one of two places, and the order
//! matters: a hand edit from the Edit tab ([`keep`]) if there is one, else the
//! disfluency cut computed from the transcript ([`compute`]). Hand first, because the
//! alternative is that pressing Render silently undoes an edit — the machine
//! recomputing over a decision a person already made.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

pub mod compose;
pub mod compute;
pub mod cut;
pub mod keep;
pub mod pane;
pub mod render;
pub mod waveform;

use crate::notes::{
    closed_chapter_numbers, load_transcript, ChapterTranscript, TranscriptStatus,
};
use crate::session::Session;
use compute::{compute_edits, DEFAULT_PADDING_MS};

const WAIT_EVERY: Duration = Duration::from_secs(1);
const WAIT_FOR: Duration = Duration::from_secs(600);

const CHAPTER_FILES: &[&str] = &["-horizontal", "-vertical"];

pub enum RenderEvent {
    Status(String),
    Ready(PathBuf),
    /// Distinct from a `Status` carrying the same words: the app has to know the
    /// thread is gone so it can re-enable the button.
    Failed(String),
}

pub fn spawn_render(session: Session, tx: Sender<RenderEvent>) {
    // Kept back from the closure so a thread that never starts still reports —
    // otherwise the app waits on a render that does not exist.
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new().name("render-hf".into()).spawn(move || {
        match run_render(&session, &tx) {
            Ok(dir) => {
                eprintln!("stream-recorder: render ready → {}", dir.display());
                let _ = tx.send(RenderEvent::Ready(dir));
            }
            Err(err) => {
                eprintln!("stream-recorder: render failed: {err:#}");
                let _ = tx.send(RenderEvent::Failed(format!("Render failed: {err:#}")));
            }
        }
    }) {
        eprintln!("stream-recorder: could not start render job: {err}");
        let _ = unstarted.send(RenderEvent::Failed(format!(
            "Could not start the render job: {err}"
        )));
    }
}

pub fn run_cut(session: &Session, status: &dyn Fn(&str)) -> Result<PathBuf> {
    let edit_root = session.edit_dir();
    // Hand-edited chapters are collected before anything waits. Their spans are
    // absolute milliseconds, so they need no words at all — and a chapter whose
    // transcript failed is exactly the one most likely to have been fixed by hand.
    let by_hand = hand_edited_chapters(&session.dir, &edit_root);
    if by_hand.is_empty() {
        status("Waiting for chapter transcripts…");
    }
    let transcribed = wait_for_words(&session.dir, &by_hand, status)?;

    let mut chapters: Vec<(u32, Option<ChapterTranscript>)> = transcribed
        .into_iter()
        .map(|(n, transcript)| (n, Some(transcript)))
        .collect();
    for n in &by_hand {
        if !chapters.iter().any(|(have, _)| have == n) {
            chapters.push((*n, None));
        }
    }
    chapters.sort_by_key(|(n, _)| *n);

    std::fs::create_dir_all(&edit_root)
        .with_context(|| format!("creating {}", edit_root.display()))?;
    let mut cut_count = 0usize;
    for (n, transcript) in &chapters {
        status(&format!("Cutting chapter {n:02}…"));
        if edit_chapter(&session.dir, &edit_root, *n, transcript.as_ref())? {
            cut_count += 1;
        }
    }
    if cut_count == 0 {
        bail!(
            "no chapters had anything to cut in {}",
            session.dir.display()
        );
    }
    Ok(edit_root)
}

/// Cut and re-render just these chapters, for a hand edit made in the Edit tab.
///
/// The same two stages Render runs, over a named subset. Everything downstream is
/// already incremental — [`compose::prepare`] only rewrites what changed and
/// [`render::render_plan`] skips jobs whose output is newer than their inputs — so a
/// one-chapter fix costs one chapter's render plus the longform concat, not the take's.
///
/// It reports on [`RenderEvent`] and is guarded by the app's render flag rather than
/// having its own: both write `edit/vN` and `render/vN`, and two of them at once would
/// be two threads composing into the same folders.
pub fn spawn_recut(session: Session, chapters: Vec<u32>, tx: Sender<RenderEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("recut-hf".into())
        .spawn(move || match run_recut(&session, &chapters, &tx) {
            Ok(dir) => {
                eprintln!("stream-recorder: re-render ready → {}", dir.display());
                let _ = tx.send(RenderEvent::Ready(dir));
            }
            Err(err) => {
                eprintln!("stream-recorder: re-render failed: {err:#}");
                let _ = tx.send(RenderEvent::Failed(format!("Re-render failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start re-render job: {err}");
        let _ = unstarted.send(RenderEvent::Failed(format!(
            "Could not start the re-render job: {err}"
        )));
    }
}

fn run_recut(session: &Session, chapters: &[u32], tx: &Sender<RenderEvent>) -> Result<PathBuf> {
    let status = |msg: &str| {
        let _ = tx.send(RenderEvent::Status(msg.to_string()));
    };
    if chapters.is_empty() {
        bail!("no chapters to re-cut");
    }
    let edit_root = session.edit_dir();
    std::fs::create_dir_all(&edit_root)
        .with_context(|| format!("creating {}", edit_root.display()))?;
    let mut cut_count = 0usize;
    for n in chapters {
        status(&format!("Cutting chapter {n:02}…"));
        let transcript = load_transcript(&session.dir, *n);
        if edit_chapter(&session.dir, &edit_root, *n, transcript.as_ref())? {
            cut_count += 1;
        }
    }
    if cut_count == 0 {
        bail!(
            "chapter {chapters:?} had nothing to cut in {}",
            session.dir.display()
        );
    }
    compose_and_render(session, &edit_root, &status)
}

/// Chapters carrying a hand edit, whatever their transcript did.
fn hand_edited_chapters(session_dir: &Path, edit_root: &Path) -> Vec<u32> {
    closed_chapter_numbers(session_dir)
        .into_iter()
        .filter(|n| {
            keep::load(&pane::chapter_dir(edit_root, *n))
                .is_some_and(|list| list.is_hand())
        })
        .collect()
}

pub fn run_render(session: &Session, tx: &Sender<RenderEvent>) -> Result<PathBuf> {
    let status = |msg: &str| {
        let _ = tx.send(RenderEvent::Status(msg.to_string()));
    };
    run_cut(session, &status)?;
    compose_and_render(session, &session.edit_dir(), &status)
}

/// The second half of a render: whatever is in `edit/vN` becomes compositions and then
/// video. Shared by Render and by a single-chapter re-cut, so the longform is assembled
/// the same way whichever one produced the cut.
fn compose_and_render(
    session: &Session,
    edit_root: &Path,
    status: &dyn Fn(&str),
) -> Result<PathBuf> {
    let numbers = existing_cut_numbers(edit_root);
    if numbers.is_empty() {
        bail!(
            "no cut chapters found in {} after the disfluency cut",
            edit_root.display()
        );
    }
    let titles = chapter_titles(session, &numbers);
    // The opening card carries the video's own title once Titles has written
    // one; the project folder's name is only a fallback for a render that runs
    // before that.
    let longform_title = crate::titles::load(&session.titles_dir())
        .ok()
        .and_then(|manifest| manifest.longform_title().map(str::to_string))
        .unwrap_or_else(|| session.title());
    status("Preparing HyperFrames compositions…");
    let plan = compose::prepare(
        edit_root,
        &session.compose_dir(),
        &compose::components_root(),
        &titles,
        &longform_title,
    )?;
    if plan.is_empty() {
        bail!("no horizontal or vertical cuts to compose");
    }
    // Only the title cards and the verticals are drawn; the chapter bodies go into the
    // longform as they were cut. Saying so keeps the count honest against the log.
    let total = plan.render_count();
    status(&format!("Rendering {total} HyperFrames composition(s)…"));
    let render_out = render::render_plan(&plan, &session.render_dir())?;
    Ok(render_out)
}

fn existing_cut_numbers(edit_root: &Path) -> Vec<u32> {
    let mut numbers = Vec::new();
    for n in 1..=99 {
        let ch_dir = edit_root.join(format!("chapter-{n:02}"));
        if ch_dir.exists()
            && (ch_dir.join(format!("chapter-{n:02}-horizontal.mp4")).exists()
                || ch_dir.join(format!("chapter-{n:02}-vertical.mp4")).exists())
        {
            numbers.push(n);
        }
    }
    numbers
}

fn chapter_titles(session: &Session, numbers: &[u32]) -> Vec<(u32, String)> {
    let titles = crate::titles::load(&session.titles_dir()).ok();
    let notes = session
        .notes_dir()
        .ok()
        .and_then(|dir| crate::notes::load_notes(&dir).ok());
    numbers
        .iter()
        .map(|&n| {
            let from_titles = titles.as_ref().and_then(|t| t.title_for(n));
            let from_notes = notes
                .as_ref()
                .and_then(|data| data.chapters.get(n.saturating_sub(1) as usize))
                .map(|c| c.title.trim())
                .filter(|t| !t.is_empty());
            let title = from_titles
                .or(from_notes)
                .map(str::to_string)
                .unwrap_or_else(|| format!("Chapter {n}"));
            (n, title)
        })
        .collect()
}

fn edit_chapter(
    draft: &Path,
    edit_root: &Path,
    n: u32,
    transcript: Option<&ChapterTranscript>,
) -> Result<bool> {
    let out = pane::chapter_dir(edit_root, n);
    // A hand edit wins. Without this, Render would recompute the disfluency cut over
    // the top of it and the Edit tab would be a tab that does nothing.
    let hand = keep::load(&out).filter(|list| list.is_hand());
    let words = transcript.map(|t| t.words.as_slice()).unwrap_or(&[]);
    let edits = match &hand {
        Some(list) => list.to_edits(words),
        None => compute_edits(words, DEFAULT_PADDING_MS, DEFAULT_PADDING_MS),
    };
    if edits.is_empty() {
        eprintln!("stream-recorder: chapter {n:02} has nothing left after the cut — skipping");
        return Ok(false);
    }
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let edits_path = out.join("edits.json");
    // Written only when it differs, because the cut below treats it as an input: a
    // rewrite with identical bytes would bump its mtime and make every cut look stale.
    compose::write_if_changed(
        &edits_path,
        &(serde_json::to_string_pretty(&edits).context("serializing edits")? + "\n"),
    )?;

    let stem = format!("chapter-{n:02}");
    let mut wrote = false;
    let mut cut_any = false;
    for suffix in CHAPTER_FILES {
        let name = format!("{stem}{suffix}.mp4");
        let source = draft.join(&name);
        if !source.exists() {
            continue;
        }
        let dest = out.join(&name);
        wrote = true;
        // Re-cutting an unchanged chapter is not free and not harmless: it rewrites the
        // file, which makes the workspace copy stale, which makes every composition
        // that reads it re-render. That is why fixing one chapter used to cost a whole
        // take's render — the freshness check in `render` was doing its job over inputs
        // this stage had just invalidated for no reason.
        if render::is_fresh(&dest, &cut_inputs(&source, &edits_path)) {
            eprintln!(
                "stream-recorder: {} is already current, skipping the cut",
                dest.display()
            );
            continue;
        }
        eprintln!(
            "stream-recorder: cutting {} → {}",
            source.display(),
            dest.display()
        );
        cut::cut_file(&source, &dest, &edits)
            .with_context(|| format!("cutting {}", source.display()))?;
        cut_any = true;
    }
    if !wrote {
        eprintln!("stream-recorder: chapter {n:02} has no horizontal/vertical mp4 to cut");
        return Ok(false);
    }
    // The mp3 is what the compositions play, so it is derived from the cut rather than
    // from the take. Re-extracted when the cut moved — or when it is simply absent,
    // which is the case a `cut_any` check alone would strand: every cut current, no mp3,
    // and a render that composes the chapter with no sound and says nothing.
    let audio = out.join("audio.mp3");
    if cut_any || !audio.is_file() {
        let audio_from = CHAPTER_FILES
            .iter()
            .map(|suffix| out.join(format!("{stem}{suffix}.mp4")))
            .find(|path| path.is_file());
        if let Some(source) = audio_from {
            cut::extract_mp3(&source, &audio)
                .with_context(|| format!("extracting audio from {}", source.display()))?;
        }
    }
    Ok(true)
}

/// What a cut is derived from: the take it came out of, and the keep-list that decided
/// where. Either one moving means the cut has to be made again.
fn cut_inputs(source: &Path, edits_path: &Path) -> Vec<PathBuf> {
    vec![source.to_path_buf(), edits_path.to_path_buf()]
}

/// Blocks until every closed chapter's transcript has landed, or given up.
///
/// `by_hand` names the chapters that already have an edit. They are not waited on and
/// their absence is not an error — a hand edit is expressed in milliseconds, so it needs
/// no words, and a chapter whose transcription failed is the very case someone reaches
/// for the Edit tab to rescue.
fn wait_for_words(
    session_dir: &Path,
    by_hand: &[u32],
    status: &dyn Fn(&str),
) -> Result<Vec<(u32, ChapterTranscript)>> {
    let closed = closed_chapter_numbers(session_dir);
    if closed.is_empty() {
        bail!("no closed chapters in {}", session_dir.display());
    }
    let closed: Vec<u32> = closed
        .into_iter()
        .filter(|n| !by_hand.contains(n))
        .collect();
    if closed.is_empty() {
        return Ok(Vec::new());
    }
    let deadline = Instant::now() + WAIT_FOR;
    loop {
        let mut pending = Vec::new();
        let mut ready = Vec::new();
        for n in &closed {
            match load_transcript(session_dir, *n) {
                Some(t) if is_ready(&t) => ready.push((*n, t)),
                Some(t) if is_terminal(&t) => {
                    eprintln!(
                        "stream-recorder: chapter {n:02} transcript is {:?} — skipping",
                        t.status
                    );
                }
                _ => pending.push(*n),
            }
        }
        if pending.is_empty() {
            // Nothing transcribed is only fatal when nothing was edited by hand
            // either — otherwise there is still real work to cut.
            if ready.is_empty() && by_hand.is_empty() {
                bail!(
                    "closed chapters finished transcribing but none have words in {}",
                    session_dir.display()
                );
            }
            return Ok(ready);
        }
        if Instant::now() > deadline {
            if ready.is_empty() && by_hand.is_empty() {
                bail!(
                    "timed out waiting for chapter {pending:?} transcripts in {}",
                    session_dir.display()
                );
            }
            eprintln!(
                "stream-recorder: timed out waiting for chapter {pending:?}; editing {} ready",
                ready.len()
            );
            return Ok(ready);
        }
        let msg = format!(
            "Waiting for chapter {} transcript…",
            pending
                .iter()
                .map(|n| format!("{n:02}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        eprintln!("stream-recorder: {msg}");
        status(&msg);
        thread::sleep(WAIT_EVERY);
    }
}

fn is_ready(t: &ChapterTranscript) -> bool {
    t.status == TranscriptStatus::Completed && !t.words.is_empty()
}

fn is_terminal(t: &ChapterTranscript) -> bool {
    matches!(
        t.status,
        TranscriptStatus::Completed | TranscriptStatus::Error | TranscriptStatus::Skipped
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::TranscriptWord;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-editmod-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn transcript(words: &[(&str, i64, i64)]) -> ChapterTranscript {
        ChapterTranscript {
            status: TranscriptStatus::Completed,
            text: String::new(),
            words: words
                .iter()
                .map(|(text, start, end)| TranscriptWord {
                    text: (*text).into(),
                    start: *start,
                    end: *end,
                    confidence: 1.0,
                })
                .collect(),
            error: None,
        }
    }

    /// The keep-list `edits.json` records, which is what `cut_file` is handed.
    fn written_edits(edit_root: &Path, n: u32) -> Vec<compute::Edit> {
        let path = pane::chapter_dir(edit_root, n).join("edits.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// A draft chapter with no real media, so `edit_chapter` writes its keep-list and
    /// then fails at ffmpeg — which is all these tests need to see.
    fn draft(dir: &Path, n: u32) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("chapter-{n:02}-horizontal.mp4")), b"nope").unwrap();
    }

    /// The whole point of the Edit tab: a hand edit survives a cut, rather than the
    /// transcript pass recomputing over the top of it.
    #[test]
    fn a_hand_keep_list_beats_the_computed_one() {
        let root = temp("precedence");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        draft(&drafts, 1);
        let words = transcript(&[("hello", 0, 500), ("um", 700, 900), ("world", 1400, 1800)]);

        // Without a hand edit, the disfluency cut splits around "um".
        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        assert_eq!(written_edits(&edits, 1).len(), 2, "the automatic cut splits");

        // With one, the cut is exactly what was asked for.
        keep::save(
            &pane::chapter_dir(&edits, 1),
            &keep::KeepList::hand(1, 2000, vec![keep::Keep::new(0, 1200)]),
        )
        .unwrap();
        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        let after = written_edits(&edits, 1);
        assert_eq!(after.len(), 1);
        assert_eq!((after[0].start, after[0].end), (0, 1200));
        // And it still reads as a transcript of itself.
        assert_eq!(after[0].text, "hello um");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A hand edit needs no words at all, which is what makes the tab a rescue for a
    /// chapter whose transcription failed.
    #[test]
    fn a_hand_edited_chapter_cuts_without_a_transcript() {
        let root = temp("nowords");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        draft(&drafts, 2);
        keep::save(
            &pane::chapter_dir(&edits, 2),
            &keep::KeepList::hand(2, 5000, vec![keep::Keep::new(500, 4000)]),
        )
        .unwrap();

        let _ = edit_chapter(&drafts, &edits, 2, None);
        let written = written_edits(&edits, 2);
        assert_eq!((written[0].start, written[0].end), (500, 4000));
        assert!(written[0].text.is_empty(), "no words to name, and that is fine");

        // Where the same chapter with no hand edit has nothing to go on.
        keep::clear(&pane::chapter_dir(&edits, 2)).unwrap();
        assert!(
            !edit_chapter(&drafts, &edits, 2, None).unwrap(),
            "nothing proposed and nothing asked for is not a cut"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `edits.json` is an input to the cut beside it, so an unchanged keep-list must
    /// keep its mtime — otherwise every run looks stale and re-renders the whole take.
    #[test]
    fn an_unchanged_keep_list_is_not_rewritten() {
        let root = temp("mtime");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        draft(&drafts, 1);
        let words = transcript(&[("hello", 0, 500), ("world", 1400, 1800)]);

        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        let path = pane::chapter_dir(&edits, 1).join("edits.json");
        let first = path.metadata().unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        assert_eq!(
            path.metadata().unwrap().modified().unwrap(),
            first,
            "the same keep-list must not look like a new one"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A cut newer than both its take and its keep-list is left alone. This is the
    /// check that makes fixing one chapter cost one chapter.
    #[test]
    fn a_current_cut_is_skipped_and_a_changed_keep_list_un_skips_it() {
        let root = temp("fresh");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        draft(&drafts, 1);
        let chapter = pane::chapter_dir(&edits, 1);
        std::fs::create_dir_all(&chapter).unwrap();
        let edits_path = chapter.join("edits.json");
        let dest = chapter.join("chapter-01-horizontal.mp4");
        let words = transcript(&[("hello", 0, 500), ("world", 1400, 1800)]);

        // Write the keep-list, then a cut that postdates it and the take.
        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&dest, b"a rendered cut").unwrap();
        assert!(render::is_fresh(&dest, &cut_inputs(
            &drafts.join("chapter-01-horizontal.mp4"),
            &edits_path,
        )));

        // A second pass leaves it exactly as it was — no ffmpeg, no new mtime.
        let before = dest.metadata().unwrap().modified().unwrap();
        // Stand in for the mp3 the first real cut would have made, so the skip is
        // testing freshness rather than the missing-audio path below it.
        std::fs::write(chapter.join("audio.mp3"), b"audio").unwrap();
        assert!(edit_chapter(&drafts, &edits, 1, Some(&words)).unwrap());
        assert_eq!(dest.metadata().unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read(&dest).unwrap(), b"a rendered cut");

        // Editing by hand changes the keep-list, so the cut is stale again.
        std::thread::sleep(Duration::from_millis(20));
        keep::save(
            &chapter,
            &keep::KeepList::hand(1, 2000, vec![keep::Keep::new(0, 900)]),
        )
        .unwrap();
        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        assert!(
            !render::is_fresh(
                &dest,
                &cut_inputs(&drafts.join("chapter-01-horizontal.mp4"), &edits_path)
            ),
            "a hand edit has to invalidate the cut it changes"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A current cut with no `audio.mp3` beside it still gets one. Left to the
    /// `cut_any` check alone, the compositions would play a silent chapter.
    #[test]
    fn a_missing_audio_track_is_re_extracted_even_when_the_cut_is_current() {
        let root = temp("audio");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        draft(&drafts, 1);
        let chapter = pane::chapter_dir(&edits, 1);
        let words = transcript(&[("hello", 0, 500), ("world", 1400, 1800)]);
        let _ = edit_chapter(&drafts, &edits, 1, Some(&words));
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(chapter.join("chapter-01-horizontal.mp4"), b"a rendered cut").unwrap();
        let _ = std::fs::remove_file(chapter.join("audio.mp3"));

        // ffmpeg is reached because the mp3 is missing, and fails on the stub media —
        // which is the observable proof that the extraction was attempted at all.
        let err = edit_chapter(&drafts, &edits, 1, Some(&words))
            .unwrap_err()
            .to_string();
        assert!(err.contains("extracting audio from"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Hand-edited chapters are never waited on: their spans are milliseconds, and a
    /// transcript that will never arrive must not hold up a cut that does not need it.
    #[test]
    fn hand_edited_chapters_are_not_waited_for() {
        let root = temp("wait");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        std::fs::create_dir_all(&drafts).unwrap();
        // Two closed chapters, neither transcribed.
        std::fs::write(drafts.join("chapter-01.mp3"), b"a").unwrap();
        std::fs::write(drafts.join("chapter-02.mp3"), b"b").unwrap();
        keep::save(
            &pane::chapter_dir(&edits, 1),
            &keep::KeepList::hand(1, 1000, vec![keep::Keep::new(0, 1000)]),
        )
        .unwrap();

        assert_eq!(hand_edited_chapters(&drafts, &edits), vec![1]);

        // Chapter 2 is still pending, so this would block — but with every *other*
        // chapter hand-edited there is nothing left to wait on and it returns at once.
        keep::save(
            &pane::chapter_dir(&edits, 2),
            &keep::KeepList::hand(2, 1000, vec![keep::Keep::new(0, 1000)]),
        )
        .unwrap();
        let by_hand = hand_edited_chapters(&drafts, &edits);
        assert_eq!(by_hand, vec![1, 2]);
        let waited = wait_for_words(&drafts, &by_hand, &|_| {}).unwrap();
        assert!(waited.is_empty(), "nothing to wait for, nothing transcribed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The automatic list on disk is not a hand edit and must not be treated as one —
    /// it is a proposal the pane showed, and Render is free to recompute it.
    #[test]
    fn an_auto_keep_list_does_not_count_as_a_hand_edit() {
        let root = temp("auto");
        let (drafts, edits) = (root.join("drafts"), root.join("edit"));
        std::fs::create_dir_all(&drafts).unwrap();
        std::fs::write(drafts.join("chapter-01.mp3"), b"a").unwrap();
        keep::save(
            &pane::chapter_dir(&edits, 1),
            &keep::KeepList::auto(
                1,
                1000,
                &[compute::Edit {
                    index: 0,
                    start: 0,
                    end: 1000,
                    duration: 1000,
                    text: String::new(),
                    start_word_idx: 0,
                    end_word_idx: 0,
                    disfluency_group: 0,
                    extended_silence: 0,
                }],
            ),
        )
        .unwrap();
        assert!(hand_edited_chapters(&drafts, &edits).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

