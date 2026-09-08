//! Take/chapter markers: track chapter/retake events and log them to JSONL.
//!
//! `Take` mirrors the existing OBS-era semantics already validated in
//! `screencast/src/screencast/workers/analyze/chapters.py` (`KNOWN_TITLES =
//! {"chapter", "take"}`): a retake, latest wins within the current chapter.
//! Resolving that discard logic stays a downstream/editing-tool concern —
//! this crate only emits the two classes faithfully with correct times.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Which kind of event a line in `chapters.jsonl` records.
///
/// `#[serde(rename_all = "lowercase")]` is what keeps this compatible with the
/// files already on disk: it serializes to exactly the `"chapter"` and
/// `"take"` string literals the two event structs used to carry by hand, so
/// every log written before this typing existed still parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkerClass {
    Chapter,
    Take,
}

/// Log entry for a chapter close event.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChapterClosedEvent {
    pub n: u32,
    pub class: MarkerClass,
    pub closed_at: String,
    pub path: PathBuf,
}

/// Log entry for a take discard (retake) event.
#[derive(Debug, Serialize, Deserialize)]
pub struct TakeDiscardedEvent {
    pub n: u32,
    pub class: MarkerClass,
    pub discarded_at: String,
    pub moved_to: PathBuf,
}

/// Appends chapter and retake events to `{session_dir}/chapters.jsonl`.
#[derive(Clone)]
pub struct MarkerLog {
    path: PathBuf,
}

impl MarkerLog {
    /// Create a new JSONL logger at the given session directory.
    pub fn new(session_dir: &Path) -> Result<Self> {
        let path = session_dir.join("chapters.jsonl");
        Ok(MarkerLog { path })
    }

    /// Log a chapter close event.
    pub fn log_chapter_closed(&self, n: u32, chapter_path: &Path) -> Result<()> {
        let event = ChapterClosedEvent {
            n,
            class: MarkerClass::Chapter,
            closed_at: iso8601_now(),
            path: chapter_path.to_path_buf(),
        };
        self.append_line(&event)
    }

    /// Log a take discard (retake) event.
    pub fn log_take_discarded(&self, n: u32, moved_to: &Path) -> Result<()> {
        let event = TakeDiscardedEvent {
            n,
            class: MarkerClass::Take,
            discarded_at: iso8601_now(),
            moved_to: moved_to.to_path_buf(),
        };
        self.append_line(&event)
    }

    fn append_line<T: Serialize>(&self, event: &T) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        let line = serde_json::to_string(event).context("serializing marker event")?;
        writeln!(file, "{}", line).context("writing marker event")?;
        Ok(())
    }
}

/// Now, as `2026-08-29T21:24:33.706060Z`.
fn iso8601_now() -> String {
    iso8601(chrono::Utc::now())
}

/// The moment `at`, in the shape every `chapters.jsonl` line has always had.
///
/// Through chrono, because the arithmetic that used to be here counted 30-day
/// months and 365-day years: a chapter closed on 29 August was logged as 25
/// September, and the error grew by a day or so every month.
fn iso8601(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S.%6fZ").to_string()
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    /// `MarkerClass` replaced two hand-written `&'static str` fields. The whole
    /// point is that nothing downstream notices: `chapters.jsonl` files already
    /// on disk must still parse, and new lines must be byte-identical to old
    /// ones.
    #[test]
    fn the_class_field_still_serializes_to_the_same_json() {
        let event = ChapterClosedEvent {
            n: 1,
            class: MarkerClass::Chapter,
            closed_at: "2026-08-09T22:58:43.421182Z".to_string(),
            path: PathBuf::from("/tmp/chapter-01.mp4"),
        };
        let line = serde_json::to_string(&event).expect("serializes");
        assert!(
            line.contains(r#""class":"chapter""#),
            "the wire format changed: {line}",
        );

        let take = TakeDiscardedEvent {
            n: 2,
            class: MarkerClass::Take,
            discarded_at: "2026-08-09T22:58:46.510504Z".to_string(),
            moved_to: PathBuf::from("/tmp/.discarded/chapter-02-1.mp4"),
        };
        assert!(serde_json::to_string(&take)
            .expect("serializes")
            .contains(r#""class":"take""#));
    }

    /// A real line from a session recorded before this field was typed.
    #[test]
    fn a_log_line_written_before_the_enum_existed_still_parses() {
        let existing = r#"{"n":1,"class":"chapter","closed_at":"2026-08-09T22:58:43.421182Z","path":"/Users/andrew/.stream-recorder/sessions/2026-08-13_22-58-28/chapter-01.mp4"}"#;
        let parsed: ChapterClosedEvent =
            serde_json::from_str(existing).expect("an existing log line still parses");
        assert_eq!(parsed.class, MarkerClass::Chapter);
        assert_eq!(parsed.n, 1);
    }

    /// The date on a chapter is the date it was recorded. The hand-rolled
    /// calendar this replaces put the Aug 29 chapters in late September.
    #[test]
    fn closed_at_is_the_real_calendar_date() {
        use chrono::TimeZone;
        let at = chrono::Utc
            .with_ymd_and_hms(2026, 8, 29, 21, 24, 33)
            .unwrap()
            + chrono::Duration::microseconds(706_060);
        assert_eq!(iso8601(at), "2026-08-29T21:24:33.706060Z");
        let now = iso8601_now();
        assert!(chrono::DateTime::parse_from_rfc3339(&now).is_ok(), "{now}");
    }
}
