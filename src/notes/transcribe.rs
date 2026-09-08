//! Background AssemblyAI job for one closed chapter.
//!
//! The Router calls [`spawn_chapter_transcript`] after the mp3 lands. The
//! thread uploads, polls, and writes `{status, text, words?}` beside the
//! chapter. A missing key, a network blip, or an AssemblyAI error is logged
//! and recorded in that file — none of it fails the cut. So is a chapter with
//! no signal in it: measured before the upload, skipped with the reason, so
//! the notes stage can say "silent" instead of "no text".

use std::path::{Path, PathBuf};
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
    fn poll(&self, id: &str) -> Result<ChapterTranscript>;
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

    fn poll(&self, id: &str) -> Result<ChapterTranscript> {
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
        return write_transcript(out, &ChapterTranscript::skipped(&silent_reason(peak)));
    }
    write_transcript(out, &ChapterTranscript::processing())?;
    let upload_url = client.upload(audio)?;
    let id = client.submit(&upload_url)?;
    let result = client.poll(&id)?;
    write_transcript(out, &result)?;
    Ok(())
}

fn finish_job(client: &impl TranscribeApi, audio: &Path, out: &Path, peak_dbfs: Option<f32>) {
    if let Err(err) = run_transcribe(client, audio, out, peak_dbfs) {
        eprintln!(
            "stream-recorder: transcript failed for {}: {err:#}",
            audio.display()
        );
        if let Err(write_err) = write_transcript(out, &ChapterTranscript::from_error(&err)) {
            eprintln!(
                "stream-recorder: could not write transcript error to {}: {write_err:#}",
                out.display()
            );
        }
    }
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
    let key = match std::env::var("ASSEMBLYAI_API_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            eprintln!(
                "stream-recorder: ASSEMBLYAI_API_KEY unset; not transcribing {}",
                audio.display()
            );
            let _ = write_transcript(
                &out,
                &ChapterTranscript::skipped("ASSEMBLYAI_API_KEY unset"),
            );
            return;
        }
    };
    eprintln!(
        "stream-recorder: transcribing {} → {}",
        audio.display(),
        out.display()
    );
    if let Err(err) = thread::Builder::new()
        .name("chapter-transcribe".into())
        .spawn(move || {
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
        eprintln!("stream-recorder: could not start transcript job: {err}");
    }
}

#[cfg(test)]
#[path = "transcribe_tests.rs"]
mod tests;
