//! Figures: a piece of screen you snipped, the moment you snipped it, and the
//! blurb written about it afterwards.
//!
//! The gesture is the one everybody already has in their hands — ⌃⇧S, then drag
//! a rectangle, like Zoom's screenshot or macOS's own ⌘⇧4. What makes it worth
//! a module rather than a keyboard shortcut is the *moment*: a figure records
//! which chapter was open and how far into it, so the blurb can be written from
//! what was being said over that picture, and the article can put the figure
//! next to the section it belongs to.
//!
//! ## Not a thumbnail still
//!
//! [`crate::thumbnail::still`] captures frames too, and deliberately names them
//! by the hash of their pixels: a thumbnail generated from a still is identified
//! partly by *which* still, so two identical frames have to be one file.
//!
//! A figure is the opposite. Its identity is *when* — the same slide snipped at
//! the top and the bottom of an explanation is two figures with two blurbs, and
//! content-addressing would silently collapse them into one. So figures are
//! numbered in capture order and the ledger, not the filename, carries the
//! meaning.
//!
//! ## Why the ledger has three kinds of row
//!
//! [`FIGURES_JSONL`] is append-only, like `chapters.jsonl`, because a capture is
//! the one irreplaceable thing here: the screen has moved on by the time anyone
//! notices a write failed. Everything else about a figure arrives later. The
//! audio the author records over it finishes seconds after the shutter, and a
//! blurb arrives minutes or hours after that, once something has transcribed —
//! so rather than rewriting rows in place, each is *another* appended row keyed
//! by the image file, and [`load`] folds them, last blurb winning. A crash can
//! therefore cost the newest blurb, which can be regenerated, and never a
//! capture, which cannot.
//!
//! What the author *said* is deliberately not a row. It lives in the transcript
//! file beside the audio — the same `{status, text}` every chapter writes — and
//! [`load`] reads it from there: one source of truth rather than a copy that
//! could disagree with it.
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | the record and its append-only ledger — the only part with no AppKit or network in it |
//! | [`snip`] | the overlay you drag the rectangle on |
//! | [`shot`] | that rectangle to a picture, through ScreenCaptureKit |
//! | [`encode`] | that picture to a lossless file at its own size, under a ceiling |
//! | [`aside`] | the microphone alone, recorded while the author explains the figure |
//! | [`blurb`] | the vision call that says what the picture shows |
//! | [`pane`] | what the Blog tab draws about all of it |
//!
//! The gesture is driven from `app::figures`, which is the only place the four
//! meet.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub mod aside;
pub mod blurb;
pub mod encode;
pub mod pane;
pub mod shot;
pub mod snip;

use crate::region::PointRect;

/// Append-only, beside `chapters.jsonl` and for the same reason.
pub const FIGURES_JSONL: &str = "figures.jsonl";

/// Where the images land, under the session root.
pub const FIGURES_DIR: &str = "figures";

/// Where on a display a figure was taken from, in that display's own points.
///
/// Its own type rather than [`PointRect`] because this one is written to disk:
/// a serde derive on the region primitive would make every coordinate change in
/// that module a wire-format change here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Snipped {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl From<PointRect> for Snipped {
    fn from(rect: PointRect) -> Snipped {
        Snipped {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        }
    }
}

/// One figure, as the app and the panes see it: the capture row with whatever
/// blurb has been written over it.
#[derive(Debug, Clone, PartialEq)]
pub struct Figure {
    /// Capture order within the session, 1-based, and the number in the
    /// filename.
    pub n: u32,
    /// The image, absolute. WebP — see [`encode`] — or JPEG in a project from
    /// before the format changed.
    pub file: PathBuf,
    /// Wall clock, in the shape every `chapters.jsonl` line has.
    pub at: String,
    /// Which chapter was open. `None` when nothing was recording — a figure
    /// snipped while setting up, which is still worth keeping and cannot be
    /// placed against a transcript.
    pub chapter: Option<u32>,
    /// Seconds into that chapter, which is what locates it in the transcript.
    pub offset: Option<f64>,
    pub rect: Snipped,
    /// The image's pixel size, measured from the bytes that were written. Zero
    /// on rows from before this was recorded — see [`Figure::pixel_size`].
    pub width: u32,
    pub height: u32,
    /// The audio the author recorded over this figure, once it has finished.
    /// `None` for a figure snipped without a break.
    pub audio: Option<PathBuf>,
    /// The audio is on disk but its transcript has not finished — the words
    /// may still be coming. False once the job ended, with words or without.
    pub transcribing: bool,
    /// What the author said over the figure, once that audio has transcribed.
    /// Empty until then, and empty for good when there was no audio.
    pub said: String,
    /// What the article prints under the picture. Empty until a blurb is written.
    pub caption: String,
    /// The `alt` text. Empty until a blurb is written.
    pub alt: String,
}

impl Figure {
    /// `ch 03 · 1:24` — where this came from, for a pane and for the prompt.
    pub fn moment(&self) -> String {
        match (self.chapter, self.offset) {
            (Some(chapter), Some(offset)) => {
                let secs = offset.max(0.0) as u64;
                format!("ch {chapter:02} · {}:{:02}", secs / 60, secs % 60)
            }
            (Some(chapter), None) => format!("ch {chapter:02}"),
            // The calendar time is all there is when nothing was recording, and
            // it is better than "unknown": it still orders the figures.
            _ => self.at.chars().skip(11).take(8).collect(),
        }
    }

    pub fn has_blurb(&self) -> bool {
        !self.caption.trim().is_empty()
    }

    /// Whether the author's explanation has landed.
    pub fn explained(&self) -> bool {
        !self.said.trim().is_empty()
    }

    /// The size to lay the figure out at, when the row recorded one.
    ///
    /// `None` for a row written before the size was measured at capture, which
    /// is a caller's cue to fall back to the snip rect and a display scale —
    /// the estimate this replaces, kept only for those rows.
    pub fn pixel_size(&self) -> Option<(u32, u32)> {
        (self.width > 0 && self.height > 0).then_some((self.width, self.height))
    }
}

/// What the App hears about while a figure is being taken or written about.
///
/// One channel for both the shutter and the blurbs, because they are the same
/// pane's business and neither can run twice at once — see the two guards on
/// the App side.
pub enum FigureEvent {
    /// The image is on disk. The ledger row is appended by the App, not by the
    /// capture: the completion block runs on one of ScreenCaptureKit's queues,
    /// and two figures snipped in quick succession would race for the file.
    Captured(Capture),
    /// The shutter failed, already formatted for a status line.
    Failed(String),
    /// Progress through a blurb pass.
    Status(String),
    /// A blurb pass finished. `written` counts blurbs on disk; `failures`
    /// names the figures that got none, one line each.
    Blurbs {
        written: usize,
        failures: Vec<String>,
    },
    /// One figure's blurb was written on its own once its explanation had
    /// transcribed — see [`blurb::auto`]. `explained` says whether the words
    /// landed or the caption came from the picture alone.
    Explained {
        n: u32,
        explained: bool,
        failure: Option<String>,
    },
}

/// A line in the ledger. See the module docs for why a blurb is its own row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Row {
    Capture {
        n: u32,
        /// Relative to the session root, so a project folder stays movable.
        file: PathBuf,
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chapter: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<f64>,
        rect: Snipped,
        /// Absent on rows from before the size was measured; zero reads the same.
        #[serde(default)]
        width: u32,
        #[serde(default)]
        height: u32,
    },
    /// The audio recorded over a figure, once it is on disk. Keyed by the image
    /// like a blurb, and for the same reason.
    Aside {
        file: PathBuf,
        /// Relative to the session root, like `file`.
        audio: PathBuf,
    },
    Blurb {
        /// Names its capture by file rather than by `n`, so a blurb row survives
        /// being copied into a new version of the project alongside the image
        /// it describes.
        file: PathBuf,
        caption: String,
        #[serde(default)]
        alt: String,
    },
}

/// Everything needed to record a capture, gathered by the caller because only
/// the App knows the clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    pub n: u32,
    pub file: PathBuf,
    pub chapter: Option<u32>,
    pub offset: Option<f64>,
    pub rect: Snipped,
    /// Measured by the shutter from the bytes it wrote; zero until then.
    pub width: u32,
    pub height: u32,
}

pub fn log_path(root: &Path) -> PathBuf {
    root.join(FIGURES_JSONL)
}

pub fn dir(root: &Path) -> PathBuf {
    root.join(FIGURES_DIR)
}

/// Where capture number `n`'s image goes.
pub fn path_for(root: &Path, n: u32) -> PathBuf {
    dir(root).join(format!("figure-{n:02}.{}", encode::EXTENSION))
}

/// Where capture number `n`'s audio goes, when the figure is taken on a break.
pub fn audio_path_for(root: &Path, n: u32) -> PathBuf {
    dir(root).join(format!("figure-{n:02}.m4a"))
}

/// The number the next capture takes.
///
/// Read from the ledger rather than counted in memory, so numbering survives a
/// restart mid-session and two figures never land on one filename.
pub fn next_n(root: &Path) -> u32 {
    load(root).iter().map(|figure| figure.n).max().unwrap_or(0) + 1
}

/// Record a capture. The image is already on disk by the time this is called.
pub fn append_capture(root: &Path, capture: &Capture) -> Result<()> {
    let relative = capture
        .file
        .strip_prefix(root)
        .unwrap_or(&capture.file)
        .to_path_buf();
    append(
        root,
        &Row::Capture {
            n: capture.n,
            file: relative,
            at: iso8601_now(),
            chapter: capture.chapter,
            offset: capture.offset,
            rect: capture.rect,
            width: capture.width,
            height: capture.height,
        },
    )
}

/// Record the audio recorded over the figure stored at `file`. The `.m4a` is
/// already finished by the time this is called.
pub fn append_aside(root: &Path, file: &Path, audio: &Path) -> Result<()> {
    let relative = |path: &Path| path.strip_prefix(root).unwrap_or(path).to_path_buf();
    append(
        root,
        &Row::Aside {
            file: relative(file),
            audio: relative(audio),
        },
    )
}

/// Record a blurb over the figure stored at `file`.
pub fn append_blurb(root: &Path, file: &Path, caption: &str, alt: &str) -> Result<()> {
    let relative = file.strip_prefix(root).unwrap_or(file).to_path_buf();
    append(
        root,
        &Row::Blurb {
            file: relative,
            caption: caption.trim().to_string(),
            alt: alt.trim().to_string(),
        },
    )
}

fn append(root: &Path, row: &Row) -> Result<()> {
    let path = log_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let line = serde_json::to_string(row).context("serializing a figure row")?;
    writeln!(file, "{line}").context("writing a figure row")?;
    Ok(())
}

/// Every figure this session has captured, in capture order, with the newest
/// blurb for each folded in.
///
/// A row that will not parse is skipped rather than fatal: the ledger is
/// appended to during a live recording, and one truncated line at the end of a
/// crashed session must not cost every figure before it.
pub fn load(root: &Path) -> Vec<Figure> {
    let Ok(file) = std::fs::File::open(log_path(root)) else {
        return Vec::new();
    };
    let rows = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Row>(&line).ok());
    // Captures first, then everything keyed on them, so the order rows landed
    // in does not matter. It cannot be relied on: the shutter answers on its
    // own queue, and an aside finished quickly is recorded before the picture
    // it was recorded over has been.
    let (captures, others): (Vec<Row>, Vec<Row>) =
        rows.partition(|row| matches!(row, Row::Capture { .. }));
    let mut figures: Vec<Figure> = Vec::new();
    for row in captures.into_iter().chain(others) {
        match row {
            Row::Capture {
                n,
                file,
                at,
                chapter,
                offset,
                rect,
                width,
                height,
            } => figures.push(Figure {
                n,
                file: root.join(file),
                at,
                chapter,
                offset,
                rect,
                width,
                height,
                audio: None,
                transcribing: false,
                said: String::new(),
                caption: String::new(),
                alt: String::new(),
            }),
            Row::Aside { file, audio } => {
                let path = root.join(file);
                if let Some(figure) = figures.iter_mut().find(|figure| figure.file == path) {
                    let audio = root.join(audio);
                    // Read here rather than copied into a row — see the module
                    // docs. Only a completed transcript has words; an error or a
                    // skip has a reason, and that is not something the author said.
                    let transcript = crate::notes::load_transcript_at(&audio);
                    figure.transcribing = transcript
                        .as_ref()
                        .is_none_or(|t| !crate::notes::is_terminal(t));
                    figure.said = transcript
                        .filter(|t| t.status == crate::notes::TranscriptStatus::Completed)
                        .map(|t| t.text.trim().to_string())
                        .unwrap_or_default();
                    figure.audio = Some(audio);
                }
            }
            Row::Blurb { file, caption, alt } => {
                let path = root.join(file);
                // Last blurb wins, which is what makes rewriting one a matter of
                // appending rather than editing.
                if let Some(figure) = figures.iter_mut().find(|figure| figure.file == path) {
                    figure.caption = caption;
                    figure.alt = alt;
                }
            }
        }
    }
    figures.sort_by_key(|figure| figure.n);
    figures
}

/// The figures with no blurb yet — what the Write Blurbs action works on.
pub fn unwritten(root: &Path) -> Vec<Figure> {
    load(root)
        .into_iter()
        .filter(|figure| !figure.has_blurb())
        .collect()
}

/// Copies the figures and their ledger into a new version of the project.
///
/// Same contract as [`crate::notes::copy_notes_into`]: a new version starts from
/// the last one's material rather than from nothing, and figures are recorded
/// moments that cannot be re-taken once the screen has moved on.
pub fn copy_into(from: &Path, to: &Path) -> Result<()> {
    if from == to {
        return Ok(());
    }
    let ledger = log_path(from);
    if !ledger.exists() {
        return Ok(());
    }
    let dest_dir = dir(to);
    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("creating {}", dest_dir.display()))?;
    if let Ok(entries) = std::fs::read_dir(dir(from)) {
        for entry in entries.filter_map(|entry| entry.ok()) {
            let name = entry.file_name();
            let _ = std::fs::copy(entry.path(), dest_dir.join(name));
        }
    }
    std::fs::copy(&ledger, log_path(to))
        .with_context(|| format!("copying {}", ledger.display()))?;
    Ok(())
}

/// Now, as `2026-08-29T21:24:33.706060Z` — the shape [`crate::markers`] writes.
fn iso8601_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.%6fZ")
        .to_string()
}

#[cfg(test)]
mod tests;
