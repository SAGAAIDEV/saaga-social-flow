//! Background AssemblyAI job for one closed chapter.
//!
//! The Router calls [`spawn_chapter_transcript`] after the mp3 lands. The
//! thread uploads, polls, and writes `{status, text, words?}` beside the
//! chapter. A missing key, a network blip, or an AssemblyAI error is logged
//! and recorded in that file — none of it fails the cut. So is a chapter with
//! no signal in it: measured before the upload, skipped with the reason, so
//! the notes stage can say "silent" instead of "no text".
//!
//! ## Knowing a job is alive
//!
//! A `processing` file says a job *started*, not that one is still running — a
//! crash, a quit mid-upload or a panic all leave it behind, and the render used
//! to wait on it for ten minutes. So every job is also registered in
//! [`IN_FLIGHT`] for exactly as long as its thread lives (a drop guard, so a
//! panic unregisters too), with the step it is on. The waiters read that:
//! a chapter that is neither finished nor running is not "still transcribing",
//! and they say so at once.
//!
//! ## The ledger
//!
//! Each step is also appended to `transcripts.jsonl` beside the chapters — key
//! present or not, upload size and time, the AssemblyAI job id, how it ended.
//! stderr is lost when the app is not run from a terminal, and "it got stuck on
//! chapter 02" is otherwise unanswerable after the fact.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::capture::level::METER_FLOOR_DBFS;

const UPLOAD_URL: &str = "https://api.assemblyai.com/v2/upload";
const TRANSCRIPT_URL: &str = "https://api.assemblyai.com/v2/transcript";
const POLL_EVERY: Duration = Duration::from_secs(3);
const POLL_FOR: Duration = Duration::from_secs(600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// The upload's own, far longer timeout: ureq's is for the whole request, and a
/// long chapter's mp3 over a home uplink takes minutes to send. Under the old
/// shared two minutes that failed as a timeout part-way through the upload.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(900);

/// The longest a job can run: the upload, the submit, the poll, and slack for
/// the peak measurement. A waiter that outlasts this has a job that is wedged
/// in a way the timeouts above did not catch.
pub(crate) const JOB_MAX: Duration = Duration::from_secs(900 + 120 + 600 + 120);

const LEDGER: &str = "transcripts.jsonl";

/// Transcript files with a live job behind them, and the step each is on.
static IN_FLIGHT: Mutex<BTreeMap<PathBuf, Stage>> = Mutex::new(BTreeMap::new());

#[derive(Debug, Clone)]
pub(crate) struct Stage {
    pub step: String,
    pub since: Instant,
}

fn in_flight() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Stage>> {
    IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner())
}

/// The step the job writing `out` is on, when one is running.
pub(crate) fn running(out: &Path) -> Option<Stage> {
    in_flight().get(out).cloned()
}

fn set_step(out: &Path, step: impl Into<String>) {
    if let Some(stage) = in_flight().get_mut(out) {
        stage.step = step.into();
    }
}

/// Holds `out`'s place in [`IN_FLIGHT`] and gives it up however the thread ends.
struct Registered(PathBuf);

impl Registered {
    /// `None` when a job for `out` is already running — a second one would
    /// upload the same audio twice and race it to the same file.
    fn claim(out: &Path) -> Option<Registered> {
        let mut jobs = in_flight();
        if jobs.contains_key(out) {
            return None;
        }
        jobs.insert(
            out.to_path_buf(),
            Stage {
                step: "starting".into(),
                since: Instant::now(),
            },
        );
        Some(Registered(out.to_path_buf()))
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        in_flight().remove(&self.0);
    }
}

/// Append one step of `media`'s transcript job to the session's ledger.
///
/// Best effort: the ledger is for reading afterwards, and a write that fails
/// is said on stderr and otherwise ignored rather than failing the job.
pub(crate) fn ledger(media: &Path, event: &str, detail: serde_json::Value) {
    let Some(dir) = media.parent() else { return };
    let mut row = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "chapter": media.file_name().map(|n| n.to_string_lossy().into_owned()),
        "event": event,
    });
    if let (Some(row), serde_json::Value::Object(extra)) = (row.as_object_mut(), detail) {
        row.extend(extra);
    }
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LEDGER))
        .and_then(|mut file| writeln!(file, "{row}"));
    if let Err(err) = written {
        eprintln!(
            "stream-recorder: could not append to {}: {err}",
            dir.join(LEDGER).display()
        );
    }
}

/// A chapter whose loudest sample never reached this was recorded from nothing:
/// a muted interface, or a loopback device with no audio routed into it.
///
/// The floor the input meter clamps to, so "the peak never left the bottom of
/// the meter" and "silent" are the same statement. Speech at -60 dBFS is a
/// thousandth of full scale, under any microphone's own noise; no real take is
/// this quiet. Skipping the upload saves a round trip, and — the reason this
/// exists — records *why* there are no words instead of leaving a "completed"
/// transcript with an empty string in it.
const SILENT_PEAK_DBFS: f32 = METER_FLOOR_DBFS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TranscriptStatus {
    Processing,
    Completed,
    Error,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TranscriptWord {
    pub text: String,
    pub start: i64,
    pub end: i64,
    #[serde(default)]
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChapterTranscript {
    pub status: TranscriptStatus,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<TranscriptWord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ChapterTranscript {
    fn processing() -> ChapterTranscript {
        ChapterTranscript {
            status: TranscriptStatus::Processing,
            text: String::new(),
            words: Vec::new(),
            error: None,
        }
    }

    fn from_error(err: &anyhow::Error) -> ChapterTranscript {
        ChapterTranscript {
            status: TranscriptStatus::Error,
            text: String::new(),
            words: Vec::new(),
            error: Some(format!("{err:#}")),
        }
    }

    fn skipped(reason: &str) -> ChapterTranscript {
        ChapterTranscript {
            status: TranscriptStatus::Skipped,
            text: String::new(),
            words: Vec::new(),
            error: Some(reason.to_string()),
        }
    }
}

trait TranscribeApi {
    fn upload(&self, audio: &Path) -> Result<String>;
    fn submit(&self, audio_url: &str) -> Result<String>;
    /// `step` is told AssemblyAI's own status as it changes, for [`IN_FLIGHT`].
    fn poll(&self, id: &str, step: &dyn Fn(&str)) -> Result<ChapterTranscript>;
}

struct AssemblyAi {
    key: String,
}

impl AssemblyAi {
    fn request(&self, req: ureq::Request) -> ureq::Request {
        req.set("Authorization", &self.key).timeout(HTTP_TIMEOUT)
    }
}

impl TranscribeApi for AssemblyAi {
    fn upload(&self, audio: &Path) -> Result<String> {
        let file = std::fs::File::open(audio)
            .with_context(|| format!("opening {} for AssemblyAI upload", audio.display()))?;
        let body: UploadResponse = self
            .request(ureq::post(UPLOAD_URL))
            .timeout(UPLOAD_TIMEOUT)
            .set("Content-Type", "application/octet-stream")
            .send(file)
            .context("uploading audio to AssemblyAI")?
            .into_json()
            .context("parsing AssemblyAI upload response")?;
        if body.upload_url.is_empty() {
            bail!("AssemblyAI upload returned an empty url");
        }
        Ok(body.upload_url)
    }

    fn submit(&self, audio_url: &str) -> Result<String> {
        let body: SubmitResponse = self
            .request(ureq::post(TRANSCRIPT_URL))
            .send_json(ureq::json!({
                "audio_url": audio_url,
                "language_code": "en",
                "disfluencies": true,
                "speech_model": "universal",
            }))
            .context("submitting AssemblyAI transcript job")?
            .into_json()
            .context("parsing AssemblyAI submit response")?;
        if body.id.is_empty() {
            bail!("AssemblyAI submit returned an empty id");
        }
        Ok(body.id)
    }

    fn poll(&self, id: &str, step: &dyn Fn(&str)) -> Result<ChapterTranscript> {
        let url = format!("{TRANSCRIPT_URL}/{id}");
        let deadline = Instant::now() + POLL_FOR;
        loop {
            if Instant::now() > deadline {
                bail!("timed out waiting for AssemblyAI transcript {id}");
            }
            let body: serde_json::Value = self
                .request(ureq::get(&url))
                .call()
                .context("polling AssemblyAI transcript")?
                .into_json()
                .context("parsing AssemblyAI poll response")?;
            if let Some(status) = body.get("status").and_then(|s| s.as_str()) {
                step(&format!("AssemblyAI {status}"));
            }
            match parse_poll(&body)? {
                Poll::Pending => thread::sleep(POLL_EVERY),
                Poll::Done(transcript) => return Ok(transcript),
            }
        }
    }
}

#[derive(Deserialize)]
struct UploadResponse {
    upload_url: String,
}

#[derive(Deserialize)]
struct SubmitResponse {
    id: String,
}

enum Poll {
    Pending,
    Done(ChapterTranscript),
}

fn parse_poll(value: &serde_json::Value) -> Result<Poll> {
    let status = value.get("status").and_then(|s| s.as_str()).unwrap_or("");
    match status {
        "queued" | "processing" => Ok(Poll::Pending),
        "completed" => Ok(Poll::Done(ChapterTranscript {
            status: TranscriptStatus::Completed,
            text: value
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            words: parse_words(value.get("words")),
            error: None,
        })),
        "error" => Ok(Poll::Done(ChapterTranscript {
            status: TranscriptStatus::Error,
            text: String::new(),
            words: Vec::new(),
            error: Some(
                value
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("AssemblyAI error")
                    .to_string(),
            ),
        })),
        other => bail!("unexpected AssemblyAI status {other:?}"),
    }
}

fn parse_words(value: Option<&serde_json::Value>) -> Vec<TranscriptWord> {
    let Some(serde_json::Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            Some(TranscriptWord {
                text: item.get("text")?.as_str()?.to_string(),
                start: item.get("start")?.as_i64()?,
                end: item.get("end")?.as_i64()?,
                confidence: item
                    .get("confidence")
                    .and_then(|c| c.as_f64())
                    .unwrap_or(1.0),
            })
        })
        .collect()
}

pub(crate) fn transcript_path(media: &Path) -> PathBuf {
    let stem = media.file_stem().unwrap_or_default();
    media.with_file_name(format!("{}.transcript.json", stem.to_string_lossy()))
}

fn already_completed(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<ChapterTranscript>(&text).ok())
        .is_some_and(|t| t.status == TranscriptStatus::Completed)
}

fn write_transcript(path: &Path, transcript: &ChapterTranscript) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(transcript).context("serializing transcript")?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

fn silent_reason(peak_dbfs: f32) -> String {
    format!("silent audio (peak {peak_dbfs:.1} dBFS) — the microphone recorded nothing")
}

/// `peak_dbfs` is the chapter's loudest sample when it could be measured;
/// `None` says nothing about the audio and the upload goes ahead as it always
/// did.
fn run_transcribe(
    client: &impl TranscribeApi,
    audio: &Path,
    out: &Path,
    peak_dbfs: Option<f32>,
) -> Result<()> {
    if already_completed(out) {
        return Ok(());
    }
    if let Some(peak) = peak_dbfs.filter(|peak| *peak <= SILENT_PEAK_DBFS) {
        eprintln!(
            "stream-recorder: {} is silent (peak {peak:.1} dBFS) — not transcribing; \
             check the Microphone dropdown",
            audio.display()
        );
        ledger(audio, "silent", serde_json::json!({ "peak_dbfs": peak }));
        return write_transcript(out, &ChapterTranscript::skipped(&silent_reason(peak)));
    }
    write_transcript(out, &ChapterTranscript::processing())?;
    let bytes = std::fs::metadata(audio).map(|m| m.len()).unwrap_or(0);
    let mb = bytes as f64 / 1_000_000.0;
    set_step(out, format!("uploading {mb:.1} MB"));
    ledger(
        audio,
        "upload_started",
        serde_json::json!({ "bytes": bytes }),
    );
    let started = Instant::now();
    let upload_url = client.upload(audio)?;
    ledger(
        audio,
        "uploaded",
        serde_json::json!({ "bytes": bytes, "secs": started.elapsed().as_secs() }),
    );
    set_step(out, "submitting to AssemblyAI");
    let id = client.submit(&upload_url)?;
    ledger(
        audio,
        "submitted",
        serde_json::json!({ "assemblyai_id": id }),
    );
    set_step(out, "AssemblyAI queued");
    let result = client.poll(&id, &|status| set_step(out, status))?;
    ledger(
        audio,
        "finished",
        serde_json::json!({
            "assemblyai_id": id,
            "status": result.status,
            "words": result.words.len(),
            "error": result.error,
            "secs": started.elapsed().as_secs(),
        }),
    );
    write_transcript(out, &result)?;
    Ok(())
}

fn finish_job(client: &impl TranscribeApi, audio: &Path, out: &Path, peak_dbfs: Option<f32>) {
    // A panic is caught rather than left to end the thread, because what it
    // leaves behind otherwise is a `processing` file that nothing will finish.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_transcribe(client, audio, out, peak_dbfs)
    }))
    .unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".into());
        Err(anyhow::anyhow!("the transcript job crashed: {what}"))
    });
    if let Err(err) = outcome {
        record_error(audio, out, &err);
    }
}

fn record_error(audio: &Path, out: &Path, err: &anyhow::Error) {
    eprintln!(
        "stream-recorder: transcript failed for {}: {err:#}",
        audio.display()
    );
    ledger(
        audio,
        "failed",
        serde_json::json!({ "error": format!("{err:#}") }),
    );
    if let Err(write_err) = write_transcript(out, &ChapterTranscript::from_error(err)) {
        eprintln!(
            "stream-recorder: could not write transcript error to {}: {write_err:#}",
            out.display()
        );
    }
}

/// Mark `media`'s chapter as failed without a job ever having run — its audio
/// could not be made, so there is nothing to send. Written so the waiters see a
/// finished chapter with a reason, rather than one that never starts.
pub(crate) fn record_failure(media: &Path, reason: &str) {
    let out = transcript_path(media);
    if already_completed(&out) || running(&out).is_some() {
        return;
    }
    record_error(media, &out, &anyhow::anyhow!("{reason}"));
}

/// Kick off a non-blocking AssemblyAI job for `audio`.
///
/// Writes `chapter-NN.transcript.json` next to it. Missing `ASSEMBLYAI_API_KEY`
/// skips the job (and does not write a file) so a later retry can still run.
/// The cut must not wait on this, and this function never returns an error.
pub fn spawn_chapter_transcript(audio: PathBuf) {
    let out = transcript_path(&audio);
    if already_completed(&out) {
        return;
    }
    let Some(registered) = Registered::claim(&out) else {
        eprintln!(
            "stream-recorder: {} is already transcribing",
            audio.display()
        );
        return;
    };
    start(audio, out, registered);
}

/// Transcribe a chapter that closed without its mp3: make that first, then go
/// on as [`spawn_chapter_transcript`]. The job is registered from the start,
/// so a waiter sees "making the chapter's mp3" rather than nothing running.
pub(crate) fn spawn_from_video(video: PathBuf) {
    let out = transcript_path(&video);
    if already_completed(&out) {
        return;
    }
    let Some(registered) = Registered::claim(&out) else {
        return;
    };
    ledger(&video, "remaking_mp3", serde_json::json!({}));
    let _ = write_transcript(&out, &ChapterTranscript::processing());
    set_step(&out, "making the chapter's mp3");
    let failed_to_start = (video.clone(), out.clone());
    if let Err(err) = thread::Builder::new()
        .name("chapter-mp3".into())
        .spawn(move || match crate::transcode::to_mp3(&video) {
            Ok(mp3) => start(mp3, out, registered),
            Err(err) => record_error(
                &video,
                &out,
                &err.context("making the chapter's mp3 to transcribe"),
            ),
        })
    {
        let (video, out) = failed_to_start;
        record_error(
            &video,
            &out,
            &anyhow::anyhow!("could not start the mp3 job: {err}"),
        );
    }
}

fn start(audio: PathBuf, out: PathBuf, registered: Registered) {
    let key = match std::env::var("ASSEMBLYAI_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            // With the reason the key is missing when the team file has it
            // and did not decrypt: "unset" alone reads as a key to go and
            // copy, when the fix is a login and a relaunch.
            let reason = match crate::settings::sops::unset_hint("ASSEMBLYAI_API_KEY") {
                Some(hint) => format!("ASSEMBLYAI_API_KEY unset — {hint}"),
                None => "ASSEMBLYAI_API_KEY unset".to_string(),
            };
            eprintln!(
                "stream-recorder: {reason}; not transcribing {}",
                audio.display()
            );
            ledger(&audio, "skipped", serde_json::json!({ "reason": reason }));
            let _ = write_transcript(&out, &ChapterTranscript::skipped(&reason));
            return;
        }
    };
    eprintln!(
        "stream-recorder: transcribing {} → {}",
        audio.display(),
        out.display()
    );
    ledger(&audio, "started", serde_json::json!({}));
    // Before the thread, not in it: a file left `skipped` or `error` from an
    // earlier try reads as finished, and a render waiting on this chapter
    // would take that and move on before the thread got round to replacing it.
    let _ = write_transcript(&out, &ChapterTranscript::processing());
    set_step(&out, "measuring the audio");
    let failed_to_start = (audio.clone(), out.clone());
    if let Err(err) = thread::Builder::new()
        .name("chapter-transcribe".into())
        .spawn(move || {
            let _registered = registered;
            // Measured here, off the main thread: an ffmpeg pass over a long
            // chapter is not instant, and the cut must not wait on it. A
            // measurement that fails is not a silent chapter.
            let peak = crate::transcode::peak_dbfs(&audio)
                .map_err(|err| {
                    eprintln!(
                        "stream-recorder: could not measure {}: {err:#}",
                        audio.display()
                    )
                })
                .ok();
            finish_job(&AssemblyAi { key }, &audio, &out, peak);
        })
    {
        let (audio, out) = failed_to_start;
        record_error(
            &audio,
            &out,
            &anyhow::anyhow!("could not start the transcript job: {err}"),
        );
    }
}

#[cfg(test)]
#[path = "transcribe_tests.rs"]
mod tests;
