//! Draft/notes pipeline: chapter transcripts, Gemini slides, a deck to read.
//!
//! Notes waits for every *closed* chapter's AssemblyAI job (the mp3 is on
//! disk) before asking Gemini. The cut itself never waits. No command queue:
//! finish the open chapter, then this thread polls the transcript files.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

mod deck;
mod openrouter;
mod picker;
mod transcribe;
mod view;

pub(crate) use deck::Chapter;
pub use deck::{load as load_notes, NotesData};
pub use openrouter::{default_model, load_providers, ModelMenuRow, AUTO_PROVIDER};
pub use picker::Picker;
pub use transcribe::spawn_chapter_transcript;
pub use view::NotesPane;

use crate::session::Session;
pub(crate) use transcribe::{ChapterTranscript, TranscriptStatus, TranscriptWord};

const WAIT_EVERY: Duration = Duration::from_secs(1);
const WAIT_FOR: Duration = Duration::from_secs(600);

pub enum NotesEvent {
    Status(String),
    Ready(PathBuf),
    /// Notes failed because nothing was said — see [`NoSpeech`].
    NoSpeech(NoSpeech),
}

/// Every closed chapter finished transcribing and none produced a word.
///
/// Its own type rather than a message, so the App can add the one thing only
/// it knows — which microphone these chapters were recorded from — instead of
/// the pane reading "none have text" with nothing about why. The reasons are
/// the transcript files' own, so a silent chapter says it was silent.
#[derive(Debug, Clone, PartialEq)]
pub struct NoSpeech {
    /// `(chapter, reason)` for every closed chapter.
    pub chapters: Vec<(u32, String)>,
}

impl fmt::Display for NoSpeech {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no chapter has any speech")?;
        for (i, (n, reason)) in self.chapters.iter().enumerate() {
            let sep = if i == 0 { " —" } else { ";" };
            write!(f, "{sep} chapter {n:02}: {reason}")?;
        }
        Ok(())
    }
}

impl std::error::Error for NoSpeech {}

/// Why each closed chapter has no words, in the transcript's own terms.
fn no_speech(session_dir: &Path) -> NoSpeech {
    let chapters = closed_chapter_numbers(session_dir)
        .into_iter()
        .map(|n| {
            let reason = match load_transcript(session_dir, n) {
                Some(t) if t.status == TranscriptStatus::Completed => {
                    "transcribed as empty".to_string()
                }
                Some(t) => t.error.unwrap_or_else(|| "no reason recorded".into()),
                None => "no transcript".to_string(),
            };
            (n, reason)
        })
        .collect();
    NoSpeech { chapters }
}

pub fn copy_notes_into(from: &Session, to: &Session) -> Result<()> {
    let src = from.notes_dir()?;
    let dest = to.notes_dir()?;
    if src == dest {
        return Ok(());
    }
    deck::copy_into(&src, &dest)
}

pub fn existing_html(session: &Session) -> Option<PathBuf> {
    let html = deck::html_path(&session.notes_dir().ok()?);
    html.exists().then_some(html)
}

pub fn closed_chapter_numbers(session_dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    for n in 1..=99 {
        let stem = format!("chapter-{n:02}");
        if session_dir.join(format!("{stem}.mp3")).exists()
            || session_dir.join(format!("{stem}.mp4")).exists()
        {
            out.push(n);
        }
    }
    out
}

pub(crate) fn load_transcript(session_dir: &Path, n: u32) -> Option<ChapterTranscript> {
    let path = session_dir.join(format!("chapter-{n:02}.transcript.json"));
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The transcript beside `media`, written by the job [`spawn_chapter_transcript`]
/// starts — for audio that is not a chapter and so has no number to look it up
/// by. A figure's aside is the caller.
pub(crate) fn load_transcript_at(media: &Path) -> Option<ChapterTranscript> {
    let text = std::fs::read_to_string(transcribe::transcript_path(media)).ok()?;
    serde_json::from_str(&text).ok()
}

fn chapter_text(t: &ChapterTranscript) -> Option<String> {
    match t.status {
        TranscriptStatus::Completed if !t.text.trim().is_empty() => Some(t.text.clone()),
        _ => None,
    }
}

/// Whether a transcript job is over, one way or another. Completed with words,
/// completed empty, failed and skipped all count; only `Processing` does not.
pub(crate) fn is_terminal(t: &ChapterTranscript) -> bool {
    matches!(
        t.status,
        TranscriptStatus::Completed | TranscriptStatus::Error | TranscriptStatus::Skipped
    )
}

pub fn collect_completed(session_dir: &Path) -> Vec<(u32, String)> {
    closed_chapter_numbers(session_dir)
        .into_iter()
        .filter_map(|n| {
            let t = load_transcript(session_dir, n)?;
            chapter_text(&t).map(|text| (n, text))
        })
        .collect()
}

/// Every transcribed chapter as one chapter-headed document, for reading
/// somewhere that is not this app.
///
/// `None` rather than an empty string when nothing has landed yet: the caller
/// puts this on the pasteboard, and silently replacing whatever was there with
/// nothing is the one outcome a copy button must not have.
///
/// Built on [`collect_completed`], so it inherits that function's judgement
/// about which chapters count — a take still transcribing, or one whose job
/// failed, is absent rather than present as a gap. The headers number the
/// chapters that *are* here, so a missing 02 is visible as a missing 02.
pub fn full_transcript(session_dir: &Path) -> Option<String> {
    let chapters = collect_completed(session_dir);
    if chapters.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (n, text) in chapters {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("## Chapter {n:02}\n\n{}", text.trim()));
    }
    Some(out)
}

fn pending_closed(session_dir: &Path) -> Vec<u32> {
    closed_chapter_numbers(session_dir)
        .into_iter()
        .filter(|&n| match load_transcript(session_dir, n) {
            Some(t) => !is_terminal(&t),
            None => true,
        })
        .collect()
}

fn wait_for_closed_transcripts(
    session_dir: &Path,
    tx: &Sender<NotesEvent>,
) -> Result<Vec<(u32, String)>> {
    let closed = closed_chapter_numbers(session_dir);
    if closed.is_empty() {
        bail!("no closed chapters in {}", session_dir.display());
    }
    let deadline = Instant::now() + WAIT_FOR;
    loop {
        let pending = pending_closed(session_dir);
        if pending.is_empty() {
            let chapters = collect_completed(session_dir);
            if chapters.is_empty() {
                return Err(no_speech(session_dir).into());
            }
            return Ok(chapters);
        }
        if Instant::now() > deadline {
            let chapters = collect_completed(session_dir);
            if chapters.is_empty() {
                bail!(
                    "timed out waiting for chapter {pending:?} transcripts in {}",
                    session_dir.display()
                );
            }
            eprintln!(
                "stream-recorder: timed out waiting for chapter {pending:?}; using {} ready",
                chapters.len()
            );
            return Ok(chapters);
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
        let _ = tx.send(NotesEvent::Status(msg));
        thread::sleep(WAIT_EVERY);
    }
}

/// Build notes on a background thread. Sends status, then the HTML path.
pub fn spawn_notes(
    session: Session,
    title: String,
    model: String,
    provider: Option<String>,
    extra_prompt: Option<String>,
    tx: Sender<NotesEvent>,
) {
    if let Err(err) = thread::Builder::new()
        .name("notes-deck".into())
        .spawn(move || {
            match build_notes(
                &session,
                &title,
                &model,
                provider.as_deref(),
                extra_prompt.as_deref(),
                &tx,
            ) {
                Ok(html) => {
                    eprintln!("stream-recorder: notes ready → {}", html.display());
                    let _ = tx.send(NotesEvent::Ready(html));
                }
                Err(err) => {
                    eprintln!("stream-recorder: notes failed: {err:#}");
                    let event = match err.downcast::<NoSpeech>() {
                        Ok(why) => NotesEvent::NoSpeech(why),
                        Err(err) => NotesEvent::Status(format!("Notes failed: {err:#}")),
                    };
                    let _ = tx.send(event);
                }
            }
        })
    {
        eprintln!("stream-recorder: could not start notes job: {err}");
    }
}

fn build_notes(
    session: &Session,
    title: &str,
    model: &str,
    provider: Option<&str>,
    extra_prompt: Option<&str>,
    tx: &Sender<NotesEvent>,
) -> Result<PathBuf> {
    let _ = tx.send(NotesEvent::Status(
        "Waiting for chapter transcripts…".into(),
    ));
    let chapters = wait_for_closed_transcripts(&session.dir, tx)?;
    eprintln!(
        "stream-recorder: building notes from {} chapter(s) via {model}",
        chapters.len()
    );
    let _ = tx.send(NotesEvent::Status(format!(
        "Building notes from {} chapter(s)…",
        chapters.len()
    )));
    let (data, step) = crate::agent::notes::extract_notes(
        &chapters,
        title,
        session.version,
        model,
        provider,
        extra_prompt,
        Some(&session.root),
    )?;
    let notes_dir = session.notes_dir()?;
    crate::agent::trace::write_step(&notes_dir, &session.root, &step)?;
    let html = deck::write(&notes_dir, &data)?;
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-notes-wait-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_completed_skips_processing_and_empty() {
        let dir = temp("collect");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        std::fs::write(dir.join("chapter-02.mp3"), b"x").unwrap();
        std::fs::write(dir.join("chapter-03.mp3"), b"x").unwrap();
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"completed","text":"hello"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-02.transcript.json"),
            r#"{"status":"processing","text":""}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-03.transcript.json"),
            r#"{"status":"completed","text":""}"#,
        )
        .unwrap();
        let got = collect_completed(&dir);
        assert_eq!(got, vec![(1, "hello".into())]);
        assert_eq!(pending_closed(&dir), vec![2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The headers carry the chapter's own number, so a chapter that never
    /// transcribed reads as a gap in the sequence rather than silently
    /// renumbering the ones after it.
    #[test]
    fn full_transcript_heads_each_chapter_with_its_own_number() {
        let dir = temp("full");
        for n in 1..=3 {
            std::fs::write(dir.join(format!("chapter-0{n}.mp3")), b"x").unwrap();
        }
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"completed","text":"  hello  "}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-02.transcript.json"),
            r#"{"status":"error","text":""}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-03.transcript.json"),
            r#"{"status":"completed","text":"world"}"#,
        )
        .unwrap();
        assert_eq!(
            full_transcript(&dir).unwrap(),
            "## Chapter 01\n\nhello\n\n## Chapter 03\n\nworld"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing transcribed is `None`, not `Some("")` — the button that reads
    /// this must be able to tell the operator why instead of clearing their
    /// pasteboard.
    #[test]
    fn full_transcript_is_none_before_anything_lands() {
        let dir = temp("full-empty");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        assert_eq!(full_transcript(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_closed_chapter_without_a_transcript_is_pending() {
        let dir = temp("pending");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        std::fs::write(dir.join("chapter-02.mp4"), b"x").unwrap();
        assert_eq!(closed_chapter_numbers(&dir), vec![1, 2]);
        assert_eq!(pending_closed(&dir), vec![1, 2]);
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"skipped","error":"no key"}"#,
        )
        .unwrap();
        assert_eq!(pending_closed(&dir), vec![2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_returns_once_every_closed_chapter_is_terminal() {
        let dir = temp("wait");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        std::fs::write(dir.join("chapter-02.mp3"), b"x").unwrap();
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"completed","text":"one"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-02.transcript.json"),
            r#"{"status":"completed","text":"two"}"#,
        )
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let got = wait_for_closed_transcripts(&dir, &tx).expect("ready");
        assert_eq!(got, vec![(1, "one".into()), (2, "two".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the pane used to say was "none have text". Now it says which
    /// chapter and why, from the transcript files themselves.
    #[test]
    fn nothing_to_say_names_each_chapter_and_its_reason() {
        let dir = temp("nospeech");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        std::fs::write(dir.join("chapter-02.mp3"), b"x").unwrap();
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"skipped","error":"silent audio (peak -91.0 dBFS) — the microphone recorded nothing"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-02.transcript.json"),
            r#"{"status":"completed","text":""}"#,
        )
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = wait_for_closed_transcripts(&dir, &tx).expect_err("nothing to build from");
        let why = err
            .downcast::<NoSpeech>()
            .expect("a typed reason the App can add the microphone to");
        assert_eq!(why.chapters.len(), 2);
        assert_eq!(why.chapters[1], (2, "transcribed as empty".into()));
        let text = why.to_string();
        assert!(
            text.contains("chapter 01: silent audio (peak -91.0 dBFS)"),
            "{text}"
        );
        assert!(text.contains("chapter 02: transcribed as empty"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
