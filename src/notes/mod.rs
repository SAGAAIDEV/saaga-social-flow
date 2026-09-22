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
pub(crate) use transcribe::record_failure as record_transcript_failure;
pub use transcribe::spawn_chapter_transcript;
pub use view::NotesPane;

use crate::session::Session;
pub(crate) use transcribe::{ChapterTranscript, TranscriptStatus, TranscriptWord};

const WAIT_EVERY: Duration = Duration::from_secs(1);
/// As long as a job can legitimately take, plus a minute: a waiter that gives
/// up sooner than the job it waits on drops a chapter that was about to land.
pub(crate) const WAIT_FOR: Duration = Duration::from_secs(transcribe::JOB_MAX.as_secs() + 60);

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
pub(crate) fn no_speech(session_dir: &Path) -> NoSpeech {
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

/// The audio of every closed chapter whose transcript never finished: skipped
/// for a key that was unset at the time, failed on a blip, left `processing`
/// by a crash, or never written. A chapter that completed — with words or
/// without — is not here, and neither is one with no mp3 to send.
fn unfinished_chapters(session_dir: &Path) -> Vec<PathBuf> {
    closed_chapter_numbers(session_dir)
        .into_iter()
        .filter(|&n| {
            !matches!(job_state(session_dir, n), JobState::Running(_))
                && !load_transcript(session_dir, n)
                    .is_some_and(|t| t.status == TranscriptStatus::Completed)
        })
        .map(|n| session_dir.join(format!("chapter-{n:02}.mp3")))
        .filter(|audio| audio.is_file())
        .collect()
}

/// Give every chapter in [`unfinished_chapters`] another go.
///
/// Until this existed nothing ever came back for a chapter the job passed
/// over, so a key that arrived after the take — a login, a paste in Settings —
/// left every chapter recorded before it wordless for good. Run at launch and
/// after a save. A chapter that is still silent is skipped again for the same
/// reason, at the cost of one ffmpeg pass; with the key still unset, the skip
/// is rewritten with the current reason and nothing is uploaded. Returns how
/// many chapters were handed to the job.
pub fn retry_unfinished_transcripts(session_dir: &Path) -> usize {
    let audio = unfinished_chapters(session_dir);
    for path in &audio {
        spawn_chapter_transcript(path.clone());
    }
    audio.len()
}

/// Make sure every closed chapter a job is about to wait on will finish, and
/// stop right away, with the fix, when one cannot.
///
/// Called by the render and the notes job before they wait. Without it a
/// chapter that could never finish — its key unset because the team file did
/// not decrypt, its mp3 never made, its job gone with a crash — sat behind
/// "Waiting for chapter 02 transcript…" for ten minutes and then went missing.
///
/// - The AssemblyAI key unset while the team file is the one holding it: the
///   decrypt is tried again ([`crate::settings::sops::retry`]), so a login
///   since launch is enough. Still failing, this is the error, and it names
///   the login command.
/// - Every chapter that is not transcribed and has no job running is started
///   again: a skip or an error from an earlier try, a `processing` left by a
///   crash, or no file at all. A chapter with an mp4 and no mp3 has the mp3
///   made first.
///
/// `skip` is chapters the caller does not need words for (hand-edited ones, for
/// the render). Returns how many chapters were started.
pub fn prepare_transcripts(session_dir: &Path, skip: &[u32]) -> Result<usize> {
    let restart: Vec<u32> = closed_chapter_numbers(session_dir)
        .into_iter()
        .filter(|n| !skip.contains(n))
        .filter(|&n| match job_state(session_dir, n) {
            JobState::Running(_) => false,
            JobState::Finished(t) => t.status != TranscriptStatus::Completed,
            JobState::Orphaned => true,
        })
        .collect();
    if restart.is_empty() {
        return Ok(0);
    }
    let list = restart
        .iter()
        .map(|n| format!("{n:02}"))
        .collect::<Vec<_>>()
        .join(", ");

    const KEY: &str = "ASSEMBLYAI_API_KEY";
    let key_unset = || std::env::var(KEY).map_or(true, |k| k.trim().is_empty());
    if key_unset() && crate::settings::sops::undecrypted(KEY) {
        if let Err(failed) = crate::settings::sops::retry() {
            transcribe::ledger(
                &session_dir.join(format!("chapter-{:02}.mp3", restart[0])),
                "blocked",
                serde_json::json!({ "chapters": list, "reason": failed.reason }),
            );
            bail!(
                "AWS login needed — chapter {list} cannot transcribe: the AssemblyAI key is in \
                 {}, which did not decrypt ({}). Run `aws sso login --profile dev` in a \
                 terminal, then press this again (or restart the app).",
                failed.path.display(),
                failed.reason
            );
        }
    }
    if key_unset() {
        bail!(
            "{KEY} is not set, so chapter {list} cannot transcribe — add it in Settings, \
             then press this again"
        );
    }

    eprintln!("stream-recorder: starting transcripts again for chapter {list}");
    for &n in &restart {
        let mp3 = session_dir.join(format!("chapter-{n:02}.mp3"));
        if mp3.is_file() {
            spawn_chapter_transcript(mp3);
        } else {
            transcribe::spawn_from_video(session_dir.join(format!("chapter-{n:02}.mp4")));
        }
    }
    Ok(restart.len())
}

/// Where one closed chapter's transcript stands, for a job about to wait on it.
pub(crate) enum JobState {
    /// The file says the job is over: completed, error or skipped.
    Finished(ChapterTranscript),
    /// A job is running for it in this process right now.
    Running(transcribe::Stage),
    /// Neither — no file, or a `processing` one with no job behind it. Nothing
    /// is going to finish this chapter; waiting on it is waiting forever.
    Orphaned,
}

pub(crate) fn job_state(session_dir: &Path, n: u32) -> JobState {
    let out = session_dir.join(format!("chapter-{n:02}.transcript.json"));
    if let Some(stage) = transcribe::running(&out) {
        return JobState::Running(stage);
    }
    match load_transcript(session_dir, n) {
        Some(t) if is_terminal(&t) => JobState::Finished(t),
        _ => JobState::Orphaned,
    }
}

/// The chapters among `chapters` a job is still running for, with its step.
///
/// An orphaned chapter is marked failed on the way — with a reason, so the
/// render's and the notes' "none have words" can say what happened — rather
/// than waited on.
pub(crate) fn still_running(session_dir: &Path, chapters: &[u32]) -> Vec<(u32, transcribe::Stage)> {
    chapters
        .iter()
        .filter_map(|&n| match job_state(session_dir, n) {
            JobState::Running(stage) => Some((n, stage)),
            JobState::Finished(_) => None,
            JobState::Orphaned => {
                let mp3 = session_dir.join(format!("chapter-{n:02}.mp3"));
                let media = if mp3.is_file() {
                    mp3
                } else {
                    session_dir.join(format!("chapter-{n:02}.mp4"))
                };
                transcribe::record_failure(
                    &media,
                    "no transcript job is running for this chapter (the app may have quit \
                     mid-transcript) — press Render or Notes again to retry it",
                );
                None
            }
        })
        .collect()
}

/// "Waiting for chapter 02 transcript — uploading 28.1 MB, 1m05s…"
pub(crate) fn waiting_message(running: &[(u32, transcribe::Stage)]) -> String {
    let parts: Vec<String> = running
        .iter()
        .map(|(n, stage)| {
            let secs = stage.since.elapsed().as_secs();
            format!("{n:02} ({}, {}m{:02}s)", stage.step, secs / 60, secs % 60)
        })
        .collect();
    format!("Waiting for chapter {} transcript…", parts.join(", "))
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

#[cfg(test)]
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
        let running = still_running(session_dir, &closed);
        if running.is_empty() {
            let chapters = collect_completed(session_dir);
            if chapters.is_empty() {
                return Err(no_speech(session_dir).into());
            }
            return Ok(chapters);
        }
        if Instant::now() > deadline {
            // Not "use what is ready": that built notes missing a chapter and
            // said so only on stderr.
            bail!(
                "gave up after {} minutes — {}",
                WAIT_FOR.as_secs() / 60,
                waiting_message(&running)
            );
        }
        let msg = waiting_message(&running);
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
    prepare_transcripts(&session.dir, &[])?;
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

    /// The retry has to reach every chapter the job passed over and none it
    /// finished: a completed-but-empty transcript is an answer, and re-sending
    /// it on every launch would bill for the same silence each time.
    #[test]
    fn the_retry_picks_every_unfinished_chapter_and_no_finished_one() {
        let dir = temp("retry");
        for n in 1..=5 {
            std::fs::write(dir.join(format!("chapter-{n:02}.mp3")), b"x").unwrap();
        }
        // 06 closed as an mp4 only: nothing to send.
        std::fs::write(dir.join("chapter-06.mp4"), b"x").unwrap();
        let transcripts = [
            (1, r#"{"status":"completed","text":"hello"}"#),
            (2, r#"{"status":"completed","text":""}"#),
            (
                3,
                r#"{"status":"skipped","text":"","error":"ASSEMBLYAI_API_KEY unset"}"#,
            ),
            (
                4,
                r#"{"status":"error","text":"","error":"upload refused"}"#,
            ),
            // 05 has no transcript file at all.
        ];
        for (n, json) in transcripts {
            std::fs::write(dir.join(format!("chapter-{n:02}.transcript.json")), json).unwrap();
        }
        let picked: Vec<String> = unfinished_chapters(&dir)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            picked,
            ["chapter-03.mp3", "chapter-04.mp3", "chapter-05.mp3"]
        );
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

    /// The hang Laura hit: a `processing` file with no job behind it (a crash,
    /// a quit mid-upload) used to be waited on for ten minutes. Now it is
    /// marked failed with the reason and the wait ends at once.
    #[test]
    fn a_processing_file_with_no_job_behind_it_is_not_waited_on() {
        let dir = temp("orphan");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"processing","text":""}"#,
        )
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        let err = wait_for_closed_transcripts(&dir, &tx).expect_err("nothing to build from");
        assert!(started.elapsed() < Duration::from_secs(5));
        let text = err.to_string();
        assert!(text.contains("no transcript job is running"), "{text}");
        let ledger = std::fs::read_to_string(dir.join("transcripts.jsonl")).unwrap();
        assert!(ledger.contains(r#""event":"failed""#), "{ledger}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without the key there is no point waiting: the guard says which
    /// chapters and what to do before anything waits.
    #[test]
    fn the_guard_fails_fast_when_the_key_is_unset() {
        if std::env::var_os("ASSEMBLYAI_API_KEY").is_some() {
            return;
        }
        let dir = temp("guard");
        std::fs::write(dir.join("chapter-01.mp3"), b"x").unwrap();
        std::fs::write(dir.join("chapter-02.mp3"), b"x").unwrap();
        std::fs::write(
            dir.join("chapter-01.transcript.json"),
            r#"{"status":"completed","text":"hello"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("chapter-02.transcript.json"),
            r#"{"status":"skipped","error":"ASSEMBLYAI_API_KEY unset"}"#,
        )
        .unwrap();
        let err = prepare_transcripts(&dir, &[]).expect_err("no key");
        let text = err.to_string();
        assert!(text.contains("chapter 02"), "{text}");
        assert!(
            !text.contains("01"),
            "a finished chapter is not in it: {text}"
        );
        // Hand-edited chapters need no words, so they do not trip it.
        assert_eq!(prepare_transcripts(&dir, &[2]).unwrap(), 0);
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
