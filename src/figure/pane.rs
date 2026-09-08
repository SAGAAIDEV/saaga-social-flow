//! What the Blog tab shows about figures, decided in Rust.
//!
//! Not a tab of its own. A figure is only ever *for* something — the article it
//! illustrates — and a list of captioned screenshots with no article beside it
//! is a folder, which the operator already has. So this builds a view that
//! [`crate::blog::pane`] embeds, right above the body the figures will be
//! placed into.
//!
//! `file://` URLs rather than inlined base64, for the reason
//! [`crate::thumbnail::pane`] gives: the JPEGs are already on disk and, at a
//! 2400-pixel long edge, large — so re-encoding every one into the HTML on each
//! repaint would cost megabytes per redraw.

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FiguresView {
    pub rows: Vec<Row>,
    /// Whether the Write Blurbs button does anything. False when every figure
    /// already has one, which is different from there being no figures.
    pub can_write: bool,
    /// One line under the button: what has been captured, and what is left.
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub n: u32,
    /// `figure 03` — formatted here rather than in the template, which is where
    /// every other decision in this crate's panes is made.
    pub label: String,
    pub url: String,
    /// `ch 03 · 1:24` — where in the recording this came from.
    pub moment: String,
    pub caption: String,
    pub alt: String,
    pub written: bool,
    /// `1280 × 720`, in pixels. Shown because a figure snipped too small
    /// publishes blurry and there is no way to tell from a thumbnail.
    pub size: String,
    /// What the author said over the figure during the break, transcribed.
    /// Empty until the words land, and empty for good with no aside.
    pub said: String,
    /// One line for the pane when `said` is empty: the words are still coming,
    /// or there was no break to record them on. Empty once `said` is not.
    pub said_note: String,
}

/// `scale` is the backing scale of the display the figures came from, for rows
/// that did not record their own size — see [`super::Figure::pixel_size`] — so
/// those sizes read in pixels rather than points. `None` — no display selected —
/// falls back to 1x rather than guessing: a size that is honest about being
/// points is better than one that claims to be pixels and is half.
pub fn build(root: &Path, scale: Option<f64>) -> FiguresView {
    let figures = super::load(root);
    let scale = scale.unwrap_or(1.0);
    let unwritten = figures.iter().filter(|figure| !figure.has_blurb()).count();
    let rows = figures
        .iter()
        .rev()
        .map(|figure| Row {
            n: figure.n,
            label: format!("figure {:02}", figure.n),
            url: file_url(&figure.file),
            moment: figure.moment(),
            caption: figure.caption.clone(),
            alt: figure.alt.clone(),
            written: figure.has_blurb(),
            said: figure.said.trim().to_string(),
            said_note: said_note(figure),
            size: match figure.pixel_size() {
                // The file's own size, measured when it was written.
                Some((w, h)) => format!("{w} × {h}"),
                // A row from before that was recorded: the snip rect at the
                // display's scale is the best estimate there is.
                None => format!(
                    "{} × {}",
                    (figure.rect.w * scale).round() as i64,
                    (figure.rect.h * scale).round() as i64,
                ),
            },
        })
        .collect();

    FiguresView {
        rows,
        can_write: unwritten > 0,
        hint: hint(figures.len(), unwritten),
    }
}

/// Why there are no words yet, when there are none. The three states read
/// differently on purpose: "still transcribing" is a wait, "no break" is a fact
/// about how the figure was taken, and an empty transcript is a mic problem.
fn said_note(figure: &super::Figure) -> String {
    if figure.explained() {
        return String::new();
    }
    match (&figure.audio, figure.transcribing) {
        (_, true) => "Transcribing what you said…".to_string(),
        (None, false) => "No explanation — snipped without a break.".to_string(),
        (Some(_), false) => "Nothing was heard over this figure.".to_string(),
    }
}

fn hint(captured: usize, unwritten: usize) -> String {
    match (captured, unwritten) {
        // Says the gesture rather than "no figures": the whole feature is
        // invisible until someone knows the chord exists.
        (0, _) => "⌃⇧S over the screen, then drag, to capture a figure.".to_string(),
        (captured, 0) => format!(
            "{captured} figure{} · every one has a blurb.",
            plural(captured)
        ),
        (captured, left) => format!(
            "{captured} figure{} · {left} still to write about.",
            plural(captured)
        ),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut out = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            // Unreserved per RFC 3986, plus the separator itself.
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            byte if byte.is_ascii_alphanumeric() => out.push(*byte as char),
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::figure::{append_blurb, append_capture, path_for, Capture, Snipped};
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-figpane-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn capture(root: &Path, n: u32) -> Capture {
        let file = path_for(root, n);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"jpeg").unwrap();
        Capture {
            n,
            file,
            chapter: Some(2),
            offset: Some(30.0 * n as f64),
            rect: Snipped {
                x: 0.0,
                y: 0.0,
                w: 640.0,
                h: 360.0,
            },
            // Unmeasured, so the size tests below exercise the estimate.
            width: 0,
            height: 0,
        }
    }

    /// A row that recorded its size reports that, whatever the display says
    /// now — the file is the fact, the display is a guess.
    #[test]
    fn a_recorded_size_wins_over_the_display_scale() {
        let root = scratch("recorded");
        let mut one = capture(&root, 1);
        one.width = 1600;
        one.height = 900;
        append_capture(&root, &one).unwrap();
        assert_eq!(build(&root, Some(2.0)).rows[0].size, "1600 × 900");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Newest first, because the figure you just took is the one you are
    /// looking for.
    #[test]
    fn the_newest_figure_is_listed_first() {
        let root = scratch("order");
        append_capture(&root, &capture(&root, 1)).unwrap();
        append_capture(&root, &capture(&root, 2)).unwrap();
        let view = build(&root, Some(2.0));
        assert_eq!(view.rows[0].n, 2);
        assert_eq!(view.rows[1].n, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Sizes are in pixels, which on a Retina display is twice the points the
    /// selection was dragged in.
    #[test]
    fn sizes_are_reported_in_pixels() {
        let root = scratch("size");
        append_capture(&root, &capture(&root, 1)).unwrap();
        assert_eq!(build(&root, Some(2.0)).rows[0].size, "1280 × 720");
        assert_eq!(build(&root, Some(1.0)).rows[0].size, "640 × 360");
        // No display selected: 1x rather than a guess.
        assert_eq!(build(&root, None).rows[0].size, "640 × 360");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The words are the other half of a figure, so each row carries them —
    /// and, until they arrive, says why not rather than showing a blank.
    #[test]
    fn a_row_carries_what_was_said_or_why_nothing_was() {
        let root = scratch("said");
        let one = capture(&root, 1);
        append_capture(&root, &one).unwrap();
        let quiet = build(&root, Some(2.0));
        assert_eq!(quiet.rows[0].said, "");
        assert!(
            quiet.rows[0].said_note.contains("without a break"),
            "{}",
            quiet.rows[0].said_note
        );

        // The aside is on disk and its transcript has been written.
        let audio = crate::figure::audio_path_for(&root, 1);
        std::fs::write(&audio, b"m4a").unwrap();
        crate::figure::append_aside(&root, &one.file, &audio).unwrap();
        std::fs::write(
            audio.with_extension("transcript.json"),
            r#"{"status":"completed","text":"  This is the dashboard nobody read.  "}"#,
        )
        .unwrap();
        let spoken = build(&root, Some(2.0));
        assert_eq!(spoken.rows[0].said, "This is the dashboard nobody read.");
        assert_eq!(spoken.rows[0].said_note, "");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// "Nothing to write" and "nothing captured" are different states and the
    /// button has to be off for both, for different reasons.
    #[test]
    fn the_write_button_is_off_when_there_is_nothing_to_write() {
        let root = scratch("gate");
        let empty = build(&root, Some(2.0));
        assert!(!empty.can_write);
        assert!(empty.hint.contains("⌃⇧S"), "{}", empty.hint);

        let one = capture(&root, 1);
        append_capture(&root, &one).unwrap();
        let pending = build(&root, Some(2.0));
        assert!(pending.can_write);
        assert!(
            pending.hint.contains("1 still to write"),
            "{}",
            pending.hint
        );

        append_blurb(&root, &one.file, "Done.", "alt").unwrap();
        let done = build(&root, Some(2.0));
        assert!(!done.can_write);
        assert!(done.hint.contains("every one has a blurb"), "{}", done.hint);
        assert!(done.rows[0].written);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A space in the project name would otherwise make an unloadable URL, and
    /// the pane would show a broken image with no clue why.
    #[test]
    fn a_path_with_a_space_is_percent_encoded() {
        let url = file_url(Path::new("/tmp/my project/figures/figure-01.jpg"));
        assert_eq!(url, "file:///tmp/my%20project/figures/figure-01.jpg");
    }
}
