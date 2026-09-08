//! What the blurb job assembles, and what it refuses to publish.
//!
//! The transcript window is the assertion that matters here: it is the whole
//! reason a figure records a moment, and a window that drifted would caption
//! every figure with the argument two minutes later.

use super::*;
use crate::figure::Snipped;
use serde_json::json;
use std::path::PathBuf;

fn figure(chapter: Option<u32>, offset: Option<f64>) -> Figure {
    Figure {
        n: 1,
        file: PathBuf::from("/tmp/figure-01.jpg"),
        at: "2026-09-04T10:22:31.000000Z".into(),
        chapter,
        offset,
        rect: Snipped {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        },
        width: 1600,
        height: 1200,
        audio: None,
        transcribing: false,
        said: String::new(),
        caption: String::new(),
        alt: String::new(),
    }
}

/// A figure snipped while nothing was recording has no transcript to sit
/// against, and the prompt has to say so rather than leave a gap the model
/// fills with an apology.
#[test]
fn a_figure_with_no_chapter_says_the_transcript_is_missing() {
    let context = context_for(
        Path::new("/nonexistent"),
        &figure(None, None),
        "Retry storms".into(),
    );
    assert!(context.contains("Retry storms"));
    assert!(
        context.contains("no explanation or transcript for this moment"),
        "{context}"
    );
}

/// The explanation is the point of the break, so it leads — and once there is
/// one, the prompt does not also claim there is nothing to go on.
#[test]
fn the_authors_explanation_leads_and_silences_the_missing_note() {
    let mut explained = figure(None, None);
    explained.said = "  This is the retry storm that took the queue down.  ".into();
    let context = context_for(Path::new("/nonexistent"), &explained, "Retry storms".into());
    let explanation = context
        .find("They said:")
        .expect("the explanation is offered");
    assert!(
        context[explanation..].contains("retry storm that took the queue down"),
        "{context}"
    );
    assert!(!context.contains("describe the picture alone"), "{context}");
}

/// The same when the chapter exists but never transcribed — the caller
/// cannot tell the two apart and should not have to.
#[test]
fn a_chapter_with_no_transcript_file_reads_the_same() {
    let context = context_for(
        Path::new("/nonexistent"),
        &figure(Some(3), Some(84.0)),
        "Retry storms".into(),
    );
    assert!(context.contains("ch 03 · 1:24"), "{context}");
    assert!(
        context.contains("no explanation or transcript for this moment"),
        "{context}"
    );
}

/// The window is what keeps a caption about the picture instead of about
/// the argument two minutes later.
#[test]
fn only_the_words_around_the_moment_travel() {
    let root = std::env::temp_dir().join(format!(
        "stream-recorder-blurb-{}-window",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let transcript = json!({
        "status": "completed",
        "text": "long before inside window long after",
        "words": [
            { "text": "long",   "start": 1_000,   "end": 1_400, "confidence": 0.9 },
            { "text": "before", "start": 2_000,   "end": 2_400, "confidence": 0.9 },
            { "text": "inside", "start": 80_000,  "end": 80_400, "confidence": 0.9 },
            { "text": "window", "start": 90_000,  "end": 90_400, "confidence": 0.9 },
            { "text": "after",  "start": 300_000, "end": 300_400, "confidence": 0.9 },
        ],
    });
    std::fs::write(
        root.join("chapter-03.transcript.json"),
        serde_json::to_string(&transcript).unwrap(),
    )
    .unwrap();

    let spoken = spoken_around(&root, &figure(Some(3), Some(84.0))).unwrap();
    assert_eq!(spoken, "inside window");
    let _ = std::fs::remove_dir_all(&root);
}

/// A transcript with a paragraph but no timings is what some providers
/// leave behind. The whole chapter is poor context; none at all is worse.
#[test]
fn a_transcript_with_no_timings_falls_back_to_the_chapter_text() {
    let root = std::env::temp_dir().join(format!(
        "stream-recorder-blurb-{}-notimings",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("chapter-01.transcript.json"),
        r#"{"status":"completed","text":"Everything that was said."}"#,
    )
    .unwrap();
    assert_eq!(
        spoken_around(&root, &figure(Some(1), Some(5.0))).as_deref(),
        Some("Everything that was said."),
    );
    let _ = std::fs::remove_dir_all(&root);
}
