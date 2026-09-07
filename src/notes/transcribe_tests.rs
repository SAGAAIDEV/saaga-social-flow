//! Tests for the chapter transcript job. See `transcribe.rs`.

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

fn temp_pair(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "stream-recorder-notes-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let audio = dir.join("chapter-01.mp3");
    std::fs::write(&audio, b"fake-mp3").expect("audio");
    let out = transcript_path(&audio);
    (audio, out)
}

fn completed() -> ChapterTranscript {
    ChapterTranscript {
        status: TranscriptStatus::Completed,
        text: "hello there".into(),
        words: vec![TranscriptWord {
            text: "hello".into(),
            start: 0,
            end: 400,
            confidence: 0.99,
        }],
        error: None,
    }
}

struct OkApi {
    transcript: ChapterTranscript,
    uploads: AtomicU32,
}

impl TranscribeApi for OkApi {
    fn upload(&self, _: &Path) -> Result<String> {
        self.uploads.fetch_add(1, Ordering::SeqCst);
        Ok("https://cdn.example/a".into())
    }
    fn submit(&self, _: &str) -> Result<String> {
        Ok("id-1".into())
    }
    fn poll(&self, _: &str) -> Result<ChapterTranscript> {
        Ok(self.transcript.clone())
    }
}

struct FailUpload;

impl TranscribeApi for FailUpload {
    fn upload(&self, _: &Path) -> Result<String> {
        bail!("upload refused");
    }
    fn submit(&self, _: &str) -> Result<String> {
        unreachable!("submit")
    }
    fn poll(&self, _: &str) -> Result<ChapterTranscript> {
        unreachable!("poll")
    }
}

#[test]
fn transcript_sits_beside_the_chapter_stem() {
    let path = PathBuf::from("/tmp/session/chapter-03.mp3");
    assert_eq!(
        transcript_path(&path),
        PathBuf::from("/tmp/session/chapter-03.transcript.json")
    );
    assert_eq!(
        transcript_path(&PathBuf::from("/tmp/session/chapter-03.mp4")),
        PathBuf::from("/tmp/session/chapter-03.transcript.json")
    );
}

#[test]
fn a_completed_poll_keeps_text_and_words() {
    let body = serde_json::json!({
        "status": "completed",
        "text": "hello there",
        "words": [
            {"text": "hello", "start": 0, "end": 400, "confidence": 0.99},
            {"text": "there", "start": 410, "end": 800}
        ]
    });
    match parse_poll(&body).expect("parses") {
        Poll::Done(t) => {
            assert_eq!(t.status, TranscriptStatus::Completed);
            assert_eq!(t.text, "hello there");
            assert_eq!(t.words.len(), 2);
            assert_eq!(t.words[1].confidence, 1.0);
        }
        Poll::Pending => panic!("completed is not pending"),
    }
}

#[test]
fn queued_and_processing_are_pending() {
    for status in ["queued", "processing"] {
        let body = serde_json::json!({"status": status});
        assert!(matches!(parse_poll(&body).expect(status), Poll::Pending));
    }
}

#[test]
fn an_api_error_becomes_an_error_transcript() {
    let body = serde_json::json!({"status": "error", "error": "Transcoding failed"});
    match parse_poll(&body).expect("parses") {
        Poll::Done(t) => {
            assert_eq!(t.status, TranscriptStatus::Error);
            assert_eq!(t.error.as_deref(), Some("Transcoding failed"));
            assert!(t.text.is_empty());
        }
        Poll::Pending => panic!("error is not pending"),
    }
}

#[test]
fn completed_json_round_trips() {
    let text = serde_json::to_string(&completed()).expect("ser");
    let back: ChapterTranscript = serde_json::from_str(&text).expect("de");
    assert_eq!(back, completed());
    assert!(text.contains(r#""status":"completed""#));
}

#[test]
fn finish_job_writes_the_completed_file() {
    let (audio, out) = temp_pair("ok");
    finish_job(
        &OkApi {
            transcript: completed(),
            uploads: AtomicU32::new(0),
        },
        &audio,
        &out,
        None,
    );
    let got: ChapterTranscript =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("json");
    assert_eq!(got, completed());
}

#[test]
fn a_completed_file_is_not_uploaded_again() {
    let (audio, out) = temp_pair("skip");
    write_transcript(&out, &completed()).expect("seed");
    let api = OkApi {
        transcript: ChapterTranscript {
            status: TranscriptStatus::Completed,
            text: "should not overwrite".into(),
            words: Vec::new(),
            error: None,
        },
        uploads: AtomicU32::new(0),
    };
    finish_job(&api, &audio, &out, None);
    assert_eq!(api.uploads.load(Ordering::SeqCst), 0);
    let got: ChapterTranscript =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("json");
    assert_eq!(got.text, "hello there");
}

#[test]
fn an_upload_failure_is_written_not_raised() {
    let (audio, out) = temp_pair("fail");
    finish_job(&FailUpload, &audio, &out, None);
    let got: ChapterTranscript =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("json");
    assert_eq!(got.status, TranscriptStatus::Error);
    assert!(got
        .error
        .as_deref()
        .is_some_and(|e| e.contains("upload refused")));
}

fn silent_api() -> OkApi {
    OkApi {
        transcript: completed(),
        uploads: AtomicU32::new(0),
    }
}

/// The bug this closes: three days of takes recorded from a loopback device
/// came back "completed" with an empty string, and notes could only report
/// "none have text". A silent chapter is skipped with the reason, and never
/// uploaded.
#[test]
fn a_silent_chapter_is_skipped_with_the_reason_and_never_uploaded() {
    let (audio, out) = temp_pair("silent");
    let api = silent_api();
    finish_job(&api, &audio, &out, Some(-91.0));
    assert_eq!(api.uploads.load(Ordering::SeqCst), 0);
    let got: ChapterTranscript =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read")).expect("json");
    assert_eq!(got.status, TranscriptStatus::Skipped);
    assert!(
        got.error
            .as_deref()
            .is_some_and(|e| e.contains("silent audio") && e.contains("-91.0 dBFS")),
        "{:?}",
        got.error
    );
}

/// Quiet is not silent: a peak above the meter floor is uploaded like any other.
#[test]
fn a_quiet_chapter_still_goes_to_assemblyai() {
    let (audio, out) = temp_pair("quiet");
    let api = silent_api();
    finish_job(&api, &audio, &out, Some(-45.0));
    assert_eq!(api.uploads.load(Ordering::SeqCst), 1);
}

/// A measurement that failed says nothing about the audio.
#[test]
fn an_unmeasured_chapter_is_uploaded() {
    let (audio, out) = temp_pair("unmeasured");
    let api = silent_api();
    finish_job(&api, &audio, &out, None);
    assert_eq!(api.uploads.load(Ordering::SeqCst), 1);
}
