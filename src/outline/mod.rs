//! The outline layout's talking points: written from the transcript, placed
//! against the cut, handed to the render.
//!
//! An outline chapter — see [`crate::layouts`] — records the camera in the
//! split layout's column and nothing else. What fills the rest of the frame is
//! this: the chapter's points, one line each, appearing beside the speaker
//! the moment they reach each one. The words exist only after the chapter
//! closes and the time each one is said only after the cut, so this stage
//! sits between the cut and the compose, and runs only for chapters recorded
//! as outline.
//!
//! Three files meet here. `chapter-NN.transcript.json` has the words and when
//! they were said; `edit/vN/chapter-NN/edits.json` has which moments survived
//! the cut; `outline/vN/outline.json` has the points and their anchors, written
//! once by the model and kept across renders so an edit to them survives. The
//! placed time is recomputed every render from the first two — see [`place`].

pub mod place;
pub mod schema;

use std::path::Path;

use anyhow::{Context, Result};

pub use schema::{load, save, ChapterOutline};

use crate::layouts::Pair;
use crate::session::Session;

/// The most points the panel can hold at the block's line height.
pub const MAX_POINTS: usize = 8;
/// The most characters a point may run to before it wraps into the next line.
pub const MAX_TEXT_CHARS: usize = 56;
/// The least time between two points appearing; closer and the second lands
/// on top of the first's arrival.
pub const MIN_GAP_SECONDS: f64 = 1.0;
/// The earliest a point may appear.
///
/// The chapter opens as a talking head, and the card slides in 1.2 seconds
/// before the first point and takes 0.8 to land — those beats live in the
/// `outline-*` compositions. A point earlier than this would have the card
/// arriving over the chapter's first frame, with no talking head to open on.
pub const FIRST_POINT_SECONDS: f64 = 2.6;
/// Where the face sits when nothing recorded it: a little above the middle,
/// where a seated speaker's is. A fraction of the frame's height.
pub const DEFAULT_FACE_Y: f64 = 0.4;

/// Which of `numbers` were recorded as outline chapters, in order.
pub fn recorded_as_outline(session_dir: &Path, numbers: &[u32]) -> Vec<u32> {
    numbers
        .iter()
        .copied()
        .filter(|&n| crate::layouts::chapter_pair(session_dir, n) == Some(Pair::Outline))
        .collect()
}

/// The points for every outline chapter in `numbers`, placed against the
/// current cut and saved.
///
/// Chapters already in the manifest keep their text and anchors — that is
/// what makes an edit to `outline.json` stick. The ones missing are written in
/// one model call. A chapter with no words to write from gets an empty
/// outline and a line in the log: the block renders its heading alone, which
/// is the designed fallback for a chapter whose transcript never landed. A
/// chapter *with* words the model cannot be asked about — no key, no network —
/// is an error, because rendering it without points would ship a layout that
/// exists to show them.
pub fn prepare(
    session: &Session,
    edit_root: &Path,
    numbers: &[u32],
    titles: &[(u32, String)],
    status: &dyn Fn(&str),
) -> Result<Vec<ChapterOutline>> {
    if numbers.is_empty() {
        return Ok(Vec::new());
    }
    let dir = session.outline_dir();
    let mut manifest = load(&dir).unwrap_or_default();
    manifest.version = session.version;

    let transcripts: Vec<(u32, Option<crate::notes::ChapterTranscript>)> = numbers
        .iter()
        .map(|&n| (n, crate::notes::load_transcript(&session.dir, n)))
        .collect();

    // What still has to be written: chapters not in the manifest that have
    // words to write from.
    let to_write: Vec<(u32, String, Option<String>)> = transcripts
        .iter()
        .filter(|(n, _)| manifest.chapter(*n).is_none())
        .filter_map(|(n, transcript)| {
            let text = transcript
                .as_ref()
                .filter(|t| t.status == crate::notes::TranscriptStatus::Completed)
                .map(|t| t.text.trim().to_string())
                .filter(|t| !t.is_empty());
            match text {
                Some(text) => {
                    let title = titles
                        .iter()
                        .find(|(m, _)| m == n)
                        .map(|(_, t)| t.clone())
                        .filter(|t| !t.trim().is_empty());
                    Some((*n, text, title))
                }
                None => {
                    eprintln!(
                        "stream-recorder: chapter {n:02} is an outline chapter with no words — \
                         its heading renders alone"
                    );
                    None
                }
            }
        })
        .collect();

    if !to_write.is_empty() {
        status(&format!(
            "Writing outline points for {} chapter(s)…",
            to_write.len()
        ));
        let (model, provider) = model_choice();
        let project = session.name().unwrap_or_else(|| "Video".into());
        let (written, step) = crate::agent::outline::extract_outline(
            &to_write,
            &project,
            &model,
            provider.as_deref(),
            Some(&session.root),
        )
        .with_context(|| {
            format!(
                "writing outline points for chapter {} — an outline chapter renders its \
                 points, so it cannot render without them",
                to_write
                    .iter()
                    .map(|(n, _, _)| format!("{n:02}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        crate::agent::trace::write_step(&dir, &session.root, &step)?;
        for outline in written {
            manifest.put(outline);
        }
    }

    // Placement, for every requested chapter, against the cut as it is now.
    let mut placed = Vec::with_capacity(numbers.len());
    for (n, transcript) in &transcripts {
        let outline = manifest.chapter(*n).cloned().unwrap_or(ChapterOutline {
            n: *n,
            points: Vec::new(),
            approved: false,
            face_y: None,
        });
        let words = transcript
            .as_ref()
            .map(|t| t.words.as_slice())
            .unwrap_or(&[]);
        let edits = load_edits(edit_root, *n);
        let points = place::place(&outline.points, words, &edits);
        // Kept once set, so a value corrected by hand in the manifest stands.
        let face_y = outline.face_y.or_else(|| face_y_of(&session.dir, *n));
        let outline = ChapterOutline {
            points,
            face_y,
            ..outline
        };
        manifest.put(outline.clone());
        placed.push(outline);
    }
    save(&dir, &manifest)?;
    Ok(placed)
}

/// Where the face sat in the vertical master, from the tracker's own record.
///
/// The vertical cover op writes `chapter-NN.cover-vertical.json` beside the
/// take with the framing's `final_anchor` — the subject's last known position,
/// normalised to the camera frame. The vertical crop keeps every row of the
/// camera, so the anchor's `y` is the face's height in the master too, and it
/// is what the composition centres the bottom band on when the card is in.
/// `None` for an untracked take or an older one, which the composition frames
/// at [`DEFAULT_FACE_Y`].
fn face_y_of(session_dir: &Path, n: u32) -> Option<f64> {
    let path = session_dir.join(format!("chapter-{n:02}.cover-vertical.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let y = value.get("final_anchor")?.as_array()?.get(1)?.as_f64()?;
    (0.0..=1.0)
        .contains(&y)
        .then_some((y * 1000.0).round() / 1000.0)
}

/// The keep-list the chapter was cut to, or nothing — in which case the raw
/// times stand, which is right for a chapter that was never cut.
fn load_edits(edit_root: &Path, n: u32) -> Vec<crate::edit::compute::Edit> {
    let path = crate::edit::pane::chapter_dir(edit_root, n).join("edits.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The model the Speaking notes picker is set to, which is what every other
/// writing step in the render chain uses. Read from the saved config rather
/// than passed down, because the render thread has no picker.
fn model_choice() -> (String, Option<String>) {
    let cfg = crate::config::load();
    let model = cfg
        .notes_model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(crate::notes::default_model);
    let provider = cfg
        .notes_provider
        .filter(|p| !p.trim().is_empty() && p != crate::notes::AUTO_PROVIDER);
    (model, provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("outline-mod-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn session(root: &Path) -> Session {
        Session {
            root: root.to_path_buf(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    /// Only chapters whose layout record says outline, in the order asked.
    #[test]
    fn recorded_as_outline_reads_the_layout_records() {
        let dir = temp("recorded");
        crate::layouts::record_chapter_layout(&dir, 1, Pair::Split).unwrap();
        crate::layouts::record_chapter_layout(&dir, 2, Pair::Outline).unwrap();
        crate::layouts::record_chapter_layout(&dir, 4, Pair::Outline).unwrap();
        // 3 has no record at all: an older take, never outline.
        assert_eq!(recorded_as_outline(&dir, &[1, 2, 3, 4]), vec![2, 4]);
        assert!(recorded_as_outline(&dir, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The manifest is the source of truth for text and anchors: a chapter
    /// already there is placed, not rewritten — so no model is called, which
    /// is also what lets this test run without a key.
    #[test]
    fn a_chapter_already_in_the_manifest_is_placed_without_the_model() {
        let root = temp("placed");
        let session = session(&root);
        std::fs::create_dir_all(&session.dir).unwrap();
        std::fs::write(
            session.dir.join("chapter-02.transcript.json"),
            r#"{"status":"completed","text":"okay so the first thing broke",
                "words":[{"text":"okay","start":0,"end":400},{"text":"so","start":500,"end":900},
                         {"text":"the","start":1000,"end":1400},{"text":"first","start":1500,"end":1900},
                         {"text":"thing","start":2000,"end":2400},{"text":"broke","start":2500,"end":2900}]}"#,
        )
        .unwrap();
        let edit_root = session.edit_dir();
        let chapter = crate::edit::pane::chapter_dir(&edit_root, 2);
        std::fs::create_dir_all(&chapter).unwrap();
        // Cut the first half second out: everything after moves 0.5s earlier.
        std::fs::write(
            chapter.join("edits.json"),
            r#"[{"index":0,"start":500,"end":2900,"duration":2400,"text":"","start_word_idx":1,"end_word_idx":5,"disfluency_group":0,"extended_silence":0}]"#,
        )
        .unwrap();
        let mut manifest = schema::OutlineManifest::default();
        manifest.put(ChapterOutline {
            n: 2,
            points: vec![schema::OutlinePoint {
                text: "What broke first".into(),
                anchor: "the first thing".into(),
                at: None,
            }],
            approved: true,
            face_y: None,
        });
        save(&session.outline_dir(), &manifest).unwrap();

        // The tracker's record of where the face sat in the vertical take.
        std::fs::write(
            session.dir.join("chapter-02.cover-vertical.json"),
            r#"{"slot":[1080.0,1920.0],"zoom":1.0,"tracking":true,"final_anchor":[0.51,0.3684],"output":[1080,1920]}"#,
        )
        .unwrap();

        let placed = prepare(&session, &edit_root, &[2], &[], &|_| {}).unwrap();
        assert_eq!(placed.len(), 1);
        // "the" at 1.0s raw, 0.5s into the cut — held back to where the card
        // has landed.
        assert_eq!(placed[0].points[0].at, Some(FIRST_POINT_SECONDS));
        assert!(placed[0].approved, "a person's tick survives placement");
        assert_eq!(
            placed[0].face_y,
            Some(0.368),
            "the band centres on the face"
        );
        // And the placed time is written back for anyone reading the file.
        let back = load(&session.outline_dir()).unwrap();
        assert_eq!(
            back.chapter(2).unwrap().points[0].at,
            Some(FIRST_POINT_SECONDS)
        );

        // A value someone corrected by hand is not overwritten by the sidecar.
        let mut manifest = back;
        let mut chapter = manifest.chapter(2).unwrap().clone();
        chapter.face_y = Some(0.25);
        manifest.put(chapter);
        save(&session.outline_dir(), &manifest).unwrap();
        let placed = prepare(&session, &edit_root, &[2], &[], &|_| {}).unwrap();
        assert_eq!(placed[0].face_y, Some(0.25));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No tracker record, no guess written: the composition's own default
    /// applies, and the manifest says nothing it does not know.
    #[test]
    fn an_untracked_take_records_no_face_position() {
        let root = temp("untracked");
        let session = session(&root);
        std::fs::create_dir_all(&session.dir).unwrap();
        std::fs::write(
            session.dir.join("chapter-01.transcript.json"),
            r#"{"status":"skipped","text":"","error":"silent audio"}"#,
        )
        .unwrap();
        std::fs::write(
            session.dir.join("chapter-01.cover-vertical.json"),
            r#"{"slot":[1080.0,1920.0],"zoom":1.0,"tracking":false,"final_anchor":null}"#,
        )
        .unwrap();
        let placed = prepare(&session, &session.edit_dir(), &[1], &[], &|_| {}).unwrap();
        assert_eq!(placed[0].face_y, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A chapter with no words is the designed fallback — empty, logged, no
    /// model asked — rather than an error or a call about nothing.
    #[test]
    fn a_wordless_outline_chapter_gets_an_empty_outline_without_the_model() {
        let root = temp("wordless");
        let session = session(&root);
        std::fs::create_dir_all(&session.dir).unwrap();
        std::fs::write(
            session.dir.join("chapter-01.transcript.json"),
            r#"{"status":"skipped","text":"","error":"silent audio"}"#,
        )
        .unwrap();
        let placed = prepare(&session, &session.edit_dir(), &[1], &[], &|_| {}).unwrap();
        assert_eq!(placed.len(), 1);
        assert!(placed[0].points.is_empty());
        assert_eq!(placed[0].variable_json(), r#"{"points":[]}"#);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_asked_is_nothing_written() {
        let root = temp("nothing");
        let session = session(&root);
        assert!(prepare(&session, &session.edit_dir(), &[], &[], &|_| {})
            .unwrap()
            .is_empty());
        assert!(
            !session.outline_dir().exists(),
            "no manifest for no chapters"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
