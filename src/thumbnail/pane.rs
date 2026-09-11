//! What the thumbnail pane shows, decided in Rust.
//!
//! Candidates and references are `file://` URLs rather than inlined base64: the
//! images are already on disk, and re-encoding a dozen of them into the HTML on
//! every repaint would cost megabytes per redraw. The pane is loaded with read
//! access scoped to the project and the library, so nothing else is reachable.

use std::path::Path;

use serde::Serialize;

use crate::thumbnail::brief::Brief;
use crate::thumbnail::{references, schema, still, SavedBrief};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pane {
    pub artwork: Vec<Shot>,
    pub artwork_notice: Option<String>,
    pub still: Option<Shot>,
    /// What was on screen when the still was taken. `None` for a talking-head
    /// layout, which has no screen to catch.
    pub screen: Option<Shot>,
    /// Never absent, so the boxes are always there to type into. When nothing has
    /// written one they are simply empty — with no step that drafts a brief,
    /// hiding the form until one existed left no way to make one.
    pub brief: Brief,
    pub candidates: Vec<Shot>,
    /// The image models on offer, with the one that will draw marked.
    pub models: Vec<ModelChoice>,
    pub references: Vec<Ref>,
    /// The procedural card's boxes and where it stands.
    ///
    /// On this tab rather than one of its own because a card and a generated
    /// picture are two ways to make the same artifact, and they land in the same
    /// candidate strip below — see [`crate::card`].
    pub card: CardView,
    pub active_id: Option<String>,
    pub can_generate: bool,
    /// Why generation is unavailable, when it is.
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shot {
    pub id: String,
    pub url: String,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardView {
    pub format: crate::thumbnail::format::Format,
    pub title: String,
    pub description: String,
    pub kicker: String,
    /// The two themes, with the chosen one marked.
    pub themes: Vec<ModelChoice>,
    /// `0.50`, as the number boxes show it: across, then down.
    pub focus: String,
    pub focus_y: String,
    /// Whether Redraw artwork does anything, and why not when it does not.
    pub can_draw: bool,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelChoice {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ref {
    pub name: String,
    pub url: String,
    pub active: bool,
}

/// `default_brief` is the house style to start a brief from when this project
/// has not written one — the caller's business, because it comes from global
/// config and a pane that read it directly could not be tested.
pub fn build(root: &Path, library_root: &Path, default_brief: Brief) -> Pane {
    let rows = schema::load(root);
    let active_id = schema::active(&rows).map(|candidate| candidate.id.clone());

    let still = still::list(root).first().map(|path| Shot {
        id: "still".into(),
        url: file_url(path),
        label: "camera still".into(),
        selected: false,
    });

    let screen = still::list_screens(root).first().map(|path| Shot {
        id: "screen".into(),
        url: file_url(path),
        label: "screen".into(),
        selected: false,
    });

    let brief = crate::thumbnail::load_brief_or_default(&fake_session(root), default_brief)
        .unwrap_or_default()
        .brief;
    let candidates = schema::candidates(&rows)
        .into_iter()
        .rev()
        .map(|candidate| Shot {
            selected: active_id.as_deref() == Some(candidate.id.as_str()),
            id: candidate.id.clone(),
            url: file_url(&candidate.path(root)),
            label: candidate.model.clone(),
        })
        .collect();

    let library = references::load(library_root);
    let blocked = match (still.is_none(), brief.is_empty()) {
        (true, _) => Some("Capture a frame first — the thumbnail is built around you.".to_string()),
        (false, true) => Some("Write a title or a description, then Generate.".to_string()),
        (false, false) => None,
    };

    let config = crate::config::load();
    let models = config
        .thumbnail
        .models
        .iter()
        .map(|model| ModelChoice {
            selected: model.id == config.thumbnail.model,
            id: model.id.clone(),
            label: model.label.clone(),
        })
        .collect();

    // Read once. This pane is rebuilt on every action, and `ready` is `load`
    // plus a staleness check — calling both meant reading and hashing all three
    // pictures twice per repaint, off a Drive-backed folder.
    let artwork = crate::card::assets::load(root).ok();
    // Only for a set that is on disk and no longer right. A project with no set
    // yet is not in a bad state, and the empty case below already says so.
    let artwork_notice = artwork
        .as_ref()
        .and_then(|set| crate::card::assets::current(root, set).err())
        .map(|err| err.to_string());

    Pane {
        artwork_notice,
        artwork: artwork
            .map(|set| {
                set.assets
                    .iter()
                    .map(|asset| Shot {
                        id: asset.kind.name().into(),
                        url: file_url(&root.join(&asset.file)),
                        label: format!("{} · {}×{}", asset.kind.name(), asset.width, asset.height),
                        selected: true,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        models,
        card: card_view(root, still.is_some()),
        // Both halves: a still to draw from, and something to draw.
        can_generate: still.is_some() && !brief.is_empty(),
        blocked,
        still,
        screen,
        brief,
        candidates,
        references: library
            .items
            .iter()
            .map(|item| Ref {
                name: item.name.clone(),
                url: file_url(&item.path),
                active: item.active,
            })
            .collect(),
        active_id,
    }
}

/// `load_brief` wants a session; the pane only has a root.
fn fake_session(root: &Path) -> crate::session::Session {
    crate::session::Session {
        root: root.to_path_buf(),
        dir: root.join("drafts"),
        version: None,
    }
}

/// A `file://` URL for a path on disk, percent-encoded.
///
/// These are real filenames chosen by whoever made them. A screenshot dragged
/// into the style library arrives called `Frame 2085667299.jpg`, and the space
/// alone makes `file:///…/Frame 2085667299.jpg` an invalid URL — the pane drew a
/// broken-image glyph and the reference looked like it had failed to upload.
/// The card's form, read off `card.json`.
///
/// `has_still` is passed in rather than re-derived so the hint agrees with the
/// Still section above it on the same repaint.
fn card_view(root: &Path, has_still: bool) -> CardView {
    let card = crate::card::load(root);
    CardView {
        format: card.format,
        themes: crate::card::THEMES
            .iter()
            .map(|name| ModelChoice {
                id: name.to_string(),
                label: name.to_string(),
                selected: card.theme_or_default() == *name,
            })
            .collect(),
        // Two decimals, because the useful range of a nudge is about a twentieth
        // of the frame and a whole number would round every one of them away.
        focus: format!("{:.2}", card.focus_clamped()),
        focus_y: format!("{:.2}", card.focus_y_clamped()),
        can_draw: !card.is_empty() && has_still,
        hint: card_hint(&card, has_still),
        title: card.title,
        description: card.description,
        kicker: card.kicker,
    }
}

fn card_hint(card: &crate::card::Card, has_still: bool) -> String {
    match (card.is_empty(), has_still) {
        (true, _) => {
            "No title yet — the render writes one, or type one under Artwork design.".to_string()
        }
        // A set is a photograph with words beside it, so there is nothing to
        // draw without one. The button is disabled to match, rather than taking
        // the press and failing on it.
        (false, false) => "No photo yet — Retake photo, or choose one.".to_string(),
        (false, true) => "The render draws these. Redraw only after retaking the photo or \
             changing the design: the YouTube thumbnail, the portrait poster, and the \
             link preview — which is the thumbnail at 1200×630. The AI format picker \
             steers only the image models."
            .to_string(),
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

/// The brief as it now stands, stamped with when it was written.
pub fn edited(brief: Brief, at: String) -> SavedBrief {
    SavedBrief {
        brief,
        written_at: at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-tpane-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The exact filename that exposed this: a screenshot dragged in from Finder.
    #[test]
    fn a_filename_with_a_space_makes_a_valid_url() {
        let url = file_url(Path::new(
            "/Users/andrew/.stream-recorder/references/Frame 2085667299.jpg",
        ));
        assert_eq!(
            url,
            "file:///Users/andrew/.stream-recorder/references/Frame%202085667299.jpg"
        );
        assert!(
            !url.contains(' '),
            "a space ends the URL and breaks the image"
        );
    }

    #[test]
    fn separators_and_ordinary_characters_survive_encoding() {
        assert_eq!(
            file_url(Path::new("/tmp/a-b_c.d~e/still-01.jpg")),
            "file:///tmp/a-b_c.d~e/still-01.jpg"
        );
        // Anything a URL would read as syntax is escaped, not passed through.
        assert_eq!(
            file_url(Path::new("/tmp/a#b?c%d.jpg")),
            "file:///tmp/a%23b%3Fc%25d.jpg"
        );
    }

    fn brief() -> Brief {
        Brief {
            title: "SHIP IT".into(),
            description: "Presenter in a studio, editor behind".into(),
        }
    }

    fn write_brief(root: &Path, brief: &Brief) {
        crate::thumbnail::save_brief(&fake_session(root), &edited(brief.clone(), "now".into()))
            .unwrap();
    }

    #[test]
    fn with_no_still_generation_is_blocked_and_says_why() {
        let root = temp("blocked");
        let library = temp("blocked-lib");
        let pane = build(&root, &library, Brief::default());
        assert!(!pane.can_generate);
        assert!(pane.blocked.unwrap().contains("Capture a frame"));
        assert!(pane.still.is_none());
        assert!(pane.candidates.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A still alone is not enough now that nothing writes the words for you.
    #[test]
    fn a_still_with_no_words_is_blocked_on_the_words() {
        let root = temp("wordless");
        let library = temp("wordless-lib");
        still::write_bytes(&root, b"frame").unwrap();
        let pane = build(&root, &library, Brief::default());
        assert!(!pane.can_generate);
        assert!(pane.blocked.unwrap().contains("Write a title"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The boxes are always rendered, empty or not — there is no step that would
    /// create the brief, so hiding the form until one existed left no way in.
    #[test]
    fn the_brief_boxes_exist_before_anything_is_written() {
        let root = temp("empty-brief");
        let library = temp("empty-brief-lib");
        let pane = build(&root, &library, Brief::default());
        assert_eq!(pane.brief, Brief::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_still_and_a_brief_unblock_generation() {
        let root = temp("still");
        let library = temp("still-lib");
        still::write_bytes(&root, b"frame").unwrap();
        write_brief(&root, &brief());
        let pane = build(&root, &library, Brief::default());
        assert!(pane.can_generate);
        assert!(pane.blocked.is_none());
        assert_eq!(pane.brief.title, "SHIP IT");
        assert!(pane.still.unwrap().url.starts_with("file:///"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn candidates_are_newest_first_and_mark_the_live_one() {
        let root = temp("candidates");
        let library = temp("candidates-lib");
        still::write_bytes(&root, b"frame").unwrap();
        for id in ["thumb-a", "thumb-b"] {
            schema::append(
                &root,
                &schema::Row::Candidate(schema::Candidate {
                    id: id.into(),
                    model: "m".into(),
                    file: format!("thumbnails/candidates/{id}.jpg"),
                    created_at: "now".into(),
                    brief_hash: "b".into(),
                    still: "s".into(),
                    screen: None,
                    refs: "r".into(),
                }),
            )
            .unwrap();
        }
        crate::thumbnail::activate(&fake_session(&root), "thumb-a").unwrap();

        let pane = build(&root, &library, Brief::default());
        let ids: Vec<&str> = pane.candidates.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["thumb-b", "thumb-a"], "newest first");
        assert_eq!(pane.active_id.as_deref(), Some("thumb-a"));
        assert!(
            pane.candidates
                .iter()
                .find(|c| c.id == "thumb-a")
                .unwrap()
                .selected
        );
        assert!(
            !pane
                .candidates
                .iter()
                .find(|c| c.id == "thumb-b")
                .unwrap()
                .selected
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn references_carry_their_switch_state() {
        let root = temp("refs");
        let library = temp("refs-lib");
        std::fs::write(library.join("a.jpg"), b"x").unwrap();
        std::fs::write(library.join("b.jpg"), b"x").unwrap();
        references::set_active(&library, "a.jpg", true).unwrap();
        let pane = build(&root, &library, Brief::default());
        assert_eq!(pane.references.len(), 2);
        assert!(
            pane.references
                .iter()
                .find(|r| r.name == "a.jpg")
                .unwrap()
                .active
        );
        assert!(
            !pane
                .references
                .iter()
                .find(|r| r.name == "b.jpg")
                .unwrap()
                .active
        );
        let _ = std::fs::remove_dir_all(&library);
    }

    /// Saving stamps when, and carries the words through untouched.
    #[test]
    fn editing_stamps_the_time_and_keeps_the_words() {
        let mut changed = brief();
        changed.description = "Something else entirely".into();
        let saved = edited(changed, "now".into());
        assert_eq!(saved.written_at, "now");
        assert_eq!(saved.brief.description, "Something else entirely");
        assert_eq!(
            saved.brief.title, "SHIP IT",
            "the other box is not disturbed"
        );
    }
}
