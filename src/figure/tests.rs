//! The ledger's fold, which is the whole reason a blurb is a second row.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "stream-recorder-figure-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn capture(root: &Path, n: u32, chapter: Option<u32>, offset: Option<f64>) -> Capture {
    let file = path_for(root, n);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"pretend jpeg").unwrap();
    Capture {
        n,
        file,
        chapter,
        offset,
        rect: Snipped {
            x: 10.0,
            y: 20.0,
            w: 640.0,
            h: 360.0,
        },
        width: 1280,
        height: 720,
    }
}

/// The audio arrives after the picture, so it is its own row — and what was
/// said over it is read from the transcript file, not copied into the ledger.
#[test]
fn an_aside_row_brings_the_audio_and_what_was_said() {
    let root = scratch("aside");
    let one = capture(&root, 1, Some(2), Some(61.0));
    append_capture(&root, &one).unwrap();
    let audio = audio_path_for(&root, 1);
    std::fs::write(&audio, b"pretend m4a").unwrap();
    append_aside(&root, &one.file, &audio).unwrap();

    // Not transcribed yet: the audio is known, the words are not — and the
    // figure says it is still waiting, which is what holds the blurb back.
    let figures = load(&root);
    assert_eq!(figures[0].audio.as_deref(), Some(audio.as_path()));
    assert!(!figures[0].explained());
    assert!(figures[0].transcribing);

    std::fs::write(
        audio.with_extension("transcript.json"),
        r#"{"status":"completed","text":"  This is the retry storm.  "}"#,
    )
    .unwrap();
    let figures = load(&root);
    assert_eq!(figures[0].said, "This is the retry storm.");
    assert!(figures[0].explained());
    assert!(!figures[0].transcribing);

    let text = std::fs::read_to_string(log_path(&root)).unwrap();
    assert!(
        text.contains("\"audio\":\"figures/figure-01.m4a\""),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The shutter answers on its own queue, so a short aside can be on disk before
/// the picture it was recorded over. The fold must not depend on the order.
#[test]
fn an_aside_row_may_land_before_its_capture() {
    let root = scratch("aside-first");
    let one = capture(&root, 1, Some(2), Some(61.0));
    let audio = audio_path_for(&root, 1);
    std::fs::write(&audio, b"pretend m4a").unwrap();
    append_aside(&root, &one.file, &audio).unwrap();
    append_capture(&root, &one).unwrap();
    let figures = load(&root);
    assert_eq!(figures.len(), 1);
    assert_eq!(figures[0].audio.as_deref(), Some(audio.as_path()));
    let _ = std::fs::remove_dir_all(&root);
}

/// A transcript that failed or was skipped has no words to offer, and must not
/// leak its reason into the figure as though the author had said it.
#[test]
fn a_failed_transcript_leaves_the_figure_unexplained() {
    let root = scratch("aside-failed");
    let one = capture(&root, 1, Some(1), Some(5.0));
    append_capture(&root, &one).unwrap();
    let audio = audio_path_for(&root, 1);
    std::fs::write(&audio, b"pretend m4a").unwrap();
    std::fs::write(
        audio.with_extension("transcript.json"),
        r#"{"status":"skipped","error":"silent audio"}"#,
    )
    .unwrap();
    append_aside(&root, &one.file, &audio).unwrap();
    let figure = &load(&root)[0];
    assert!(figure.audio.is_some());
    assert!(!figure.explained());
    assert!(
        !figure.transcribing,
        "a skipped transcript is over, not pending"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The size is a fact about the file, recorded when it was written, and it has
/// to survive the round trip — the CMS lays the figure out from it.
#[test]
fn the_pixel_size_is_recorded_with_the_capture() {
    let root = scratch("size");
    append_capture(&root, &capture(&root, 1, Some(1), Some(1.0))).unwrap();
    assert_eq!(load(&root)[0].pixel_size(), Some((1280, 720)));
    let _ = std::fs::remove_dir_all(&root);
}

/// A row from before the size was measured has none, and says so rather than
/// claiming a zero-by-zero image.
#[test]
fn a_row_written_before_sizes_existed_has_no_pixel_size() {
    let root = scratch("legacy-size");
    let file = path_for(&root, 1);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"pretend jpeg").unwrap();
    std::fs::write(
        log_path(&root),
        concat!(
            "{\"kind\":\"capture\",\"n\":1,\"file\":\"figures/figure-01.jpg\",",
            "\"at\":\"2026-08-29T21:24:33.706060Z\",\"chapter\":2,\"offset\":10.0,",
            "\"rect\":{\"x\":0.0,\"y\":0.0,\"w\":640.0,\"h\":360.0}}\n"
        ),
    )
    .unwrap();
    let figure = &load(&root)[0];
    assert_eq!(figure.pixel_size(), None);
    assert!(figure.audio.is_none());
    assert!(!figure.explained());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_blurb_row_lands_on_its_capture() {
    let root = scratch("fold");
    let first = capture(&root, 1, Some(3), Some(84.0));
    let second = capture(&root, 2, Some(3), Some(140.0));
    append_capture(&root, &first).unwrap();
    append_capture(&root, &second).unwrap();
    append_blurb(
        &root,
        &second.file,
        "The retry storm.",
        "A log full of 429s",
    )
    .unwrap();

    let figures = load(&root);
    assert_eq!(figures.len(), 2);
    assert!(!figures[0].has_blurb(), "only the second was written about");
    assert_eq!(figures[1].caption, "The retry storm.");
    assert_eq!(figures[1].alt, "A log full of 429s");
    let _ = std::fs::remove_dir_all(&root);
}

/// Rewriting a blurb is an append, so the newest row has to win. If the first
/// one won instead, a regenerated blurb would look like it had no effect.
#[test]
fn the_last_blurb_wins() {
    let root = scratch("last-wins");
    let one = capture(&root, 1, Some(1), Some(2.0));
    append_capture(&root, &one).unwrap();
    append_blurb(&root, &one.file, "First attempt.", "alt one").unwrap();
    append_blurb(&root, &one.file, "Second attempt.", "alt two").unwrap();

    let figures = load(&root);
    assert_eq!(figures[0].caption, "Second attempt.");
    assert_eq!(figures[0].alt, "alt two");
    let _ = std::fs::remove_dir_all(&root);
}

/// The ledger is appended to while recording, so a session killed mid-write
/// leaves a partial last line. Losing every earlier figure to it would be the
/// one unrecoverable failure in this module.
#[test]
fn a_truncated_last_line_does_not_cost_the_figures_before_it() {
    let root = scratch("truncated");
    append_capture(&root, &capture(&root, 1, Some(1), Some(1.0))).unwrap();
    append_capture(&root, &capture(&root, 2, Some(1), Some(9.0))).unwrap();
    let mut ledger = OpenOptions::new()
        .append(true)
        .open(log_path(&root))
        .unwrap();
    write!(ledger, "{{\"kind\":\"capture\",\"n\":3,\"fi").unwrap();
    drop(ledger);

    assert_eq!(load(&root).len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

/// Numbering is read off disk so a restart mid-session cannot put two captures
/// on one filename.
#[test]
fn numbering_continues_from_the_ledger() {
    let root = scratch("numbering");
    assert_eq!(next_n(&root), 1, "an empty session starts at one");
    append_capture(&root, &capture(&root, 1, None, None)).unwrap();
    append_capture(&root, &capture(&root, 2, None, None)).unwrap();
    assert_eq!(next_n(&root), 3);
    let _ = std::fs::remove_dir_all(&root);
}

/// Paths are stored relative, so a project folder copied or moved — which is
/// exactly what a new version is — still resolves its own figures.
#[test]
fn the_ledger_stores_paths_relative_to_the_project() {
    let root = scratch("relative");
    let one = capture(&root, 1, Some(2), Some(5.0));
    append_capture(&root, &one).unwrap();
    let text = std::fs::read_to_string(log_path(&root)).unwrap();
    assert!(
        text.contains("\"file\":\"figures/figure-01.webp\""),
        "the path travelled absolute: {text}"
    );
    assert_eq!(load(&root)[0].file, one.file, "and still resolves absolute");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_figure_names_the_moment_it_came_from() {
    let root = scratch("moment");
    append_capture(&root, &capture(&root, 1, Some(3), Some(84.0))).unwrap();
    append_capture(&root, &capture(&root, 2, None, None)).unwrap();
    let figures = load(&root);
    assert_eq!(figures[0].moment(), "ch 03 · 1:24");
    // Nothing was recording for the second, so the clock time is all there is.
    assert_eq!(figures[1].moment().len(), 8, "{}", figures[1].moment());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unwritten_is_the_work_left_to_do() {
    let root = scratch("unwritten");
    let one = capture(&root, 1, Some(1), Some(1.0));
    append_capture(&root, &one).unwrap();
    append_capture(&root, &capture(&root, 2, Some(1), Some(2.0))).unwrap();
    append_blurb(&root, &one.file, "Written.", "alt").unwrap();
    let left = unwritten(&root);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].n, 2);
    let _ = std::fs::remove_dir_all(&root);
}

/// A blurb whose caption is only whitespace is not a blurb: it would publish as
/// an empty figure caption and never be regenerated, because the row exists.
#[test]
fn a_blank_caption_leaves_the_figure_unwritten() {
    let root = scratch("blank");
    let one = capture(&root, 1, Some(1), Some(1.0));
    append_capture(&root, &one).unwrap();
    append_blurb(&root, &one.file, "   ", "  ").unwrap();
    assert_eq!(unwritten(&root).len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// A new version starts from the last one's figures, JPEGs and ledger together —
/// half of either would be a caption with no picture or the reverse.
#[test]
fn figures_travel_into_a_new_version() {
    let from = scratch("copy-from");
    let to = scratch("copy-to");
    let one = capture(&from, 1, Some(1), Some(3.0));
    append_capture(&from, &one).unwrap();
    append_blurb(&from, &one.file, "Carried.", "alt").unwrap();

    copy_into(&from, &to).unwrap();
    let figures = load(&to);
    assert_eq!(figures.len(), 1);
    assert_eq!(figures[0].caption, "Carried.");
    assert!(figures[0].file.exists(), "the jpeg came too");
    let _ = std::fs::remove_dir_all(&from);
    let _ = std::fs::remove_dir_all(&to);
}

#[test]
fn a_project_with_no_figures_reads_as_empty() {
    let root = scratch("empty");
    assert!(load(&root).is_empty());
    assert!(unwritten(&root).is_empty());
    // And copying from it is not an error — most projects have no figures.
    let to = scratch("empty-to");
    copy_into(&root, &to).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&to);
}
