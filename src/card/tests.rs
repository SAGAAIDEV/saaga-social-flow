//! The card's stored state: what survives a round trip, and what a hand-edited
//! file cannot break.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "stream-recorder-card-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn card() -> Card {
    Card {
        title: "Ship it anyway".into(),
        description: "Why the queue fell over.".into(),
        kicker: "SAAGA".into(),
        theme: "light".into(),
        focus: 0.34,
            format: Default::default(),
    }
}

#[test]
fn a_saved_card_comes_back_whole() {
    let root = scratch("roundtrip");
    save(&root, &card()).unwrap();
    assert_eq!(load(&root), card());
    let _ = std::fs::remove_dir_all(&root);
}

/// A project that has never drawn one shows empty boxes to type into, not an
/// error and not a tab that will not paint.
#[test]
fn a_project_with_no_card_reads_as_empty() {
    let root = scratch("missing");
    let blank = load(&root);
    assert!(blank.is_empty());
    assert_eq!(blank.theme, "dark", "the default theme is the grid-proof one");
    assert_eq!(blank.focus, 0.5);
    let _ = std::fs::remove_dir_all(&root);
}

/// Same reason: a truncated or hand-mangled `card.json` must not take the
/// Thumbnail tab down with it.
#[test]
fn an_unreadable_card_reads_as_empty() {
    let root = scratch("garbage");
    std::fs::write(path(&root), "{ this is not json").unwrap();
    assert!(load(&root).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// An older `card.json` written before a field existed still loads, with the
/// default rather than a parse failure.
#[test]
fn a_card_with_only_a_title_still_loads() {
    let root = scratch("partial");
    std::fs::write(path(&root), r#"{"title":"Ship it"}"#).unwrap();
    let loaded = load(&root);
    assert_eq!(loaded.title, "Ship it");
    assert_eq!(loaded.theme, "dark");
    assert_eq!(loaded.focus, 0.5);
    let _ = std::fs::remove_dir_all(&root);
}

/// A title is the whole requirement — the description is the optional half.
#[test]
fn a_title_alone_is_a_card() {
    assert!(Card { title: "  ".into(), ..card() }.is_empty());
    assert!(!Card { description: String::new(), ..card() }.is_empty());
}

/// A hand-edited theme would otherwise reach the renderer as an index into a
/// map that has no such key, and draw a card with no colours at all.
#[test]
fn an_unknown_theme_falls_back_to_dark() {
    assert_eq!(Card { theme: "neon".into(), ..card() }.theme_or_default(), "dark");
    assert_eq!(Card { theme: " light ".into(), ..card() }.theme_or_default(), "light");
}

#[test]
fn the_focus_is_held_inside_the_frame() {
    assert_eq!(Card { focus: 4.0, ..card() }.focus_clamped(), 1.0);
    assert_eq!(Card { focus: -1.0, ..card() }.focus_clamped(), 0.0);
    assert_eq!(Card { focus: 0.34, ..card() }.focus_clamped(), 0.34);
}

/// The fingerprint is what the candidate id is built from, so an edit has to
/// move it and stray whitespace must not — saving a trailing space would
/// otherwise redraw the same picture under a new name.
#[test]
fn the_fingerprint_moves_on_an_edit_and_not_on_whitespace() {
    let padded = Card {
        title: format!("  {}  ", card().title),
        description: format!("\n{}\n", card().description),
        kicker: format!(" {} ", card().kicker),
        ..card()
    };
    assert_eq!(padded.fingerprint(), card().fingerprint());

    for edited in [
        Card { title: "Ship it".into(), ..card() },
        Card { description: "Else.".into(), ..card() },
        Card { kicker: String::new(), ..card() },
        Card { theme: "dark".into(), ..card() },
        Card { focus: 0.35, ..card() },
    ] {
        assert_ne!(edited.fingerprint(), card().fingerprint(), "{edited:?}");
    }
}

/// A card with nothing typed in it is refused before `bun` is started, so the
/// message is about the empty box rather than about a subprocess.
#[test]
fn an_empty_card_is_refused_before_anything_runs() {
    let root = scratch("ready");
    let err = ready(&root, &Card::default()).unwrap_err().to_string();
    assert!(err.contains("title"), "{err}");
    assert!(ready(&root, &card()).is_ok(), "the renderer is missing from the crate");
    let _ = std::fs::remove_dir_all(&root);
}

/// The page has to be written where the web view is allowed to read from, or
/// the photograph in it resolves to nothing.
#[test]
fn the_page_is_written_inside_the_project() {
    let root = Path::new("/tmp/a-project");
    let page = page_path(root);
    assert!(page.starts_with(root), "{}", page.display());
    assert_eq!(page.file_name().unwrap(), "card.html");
}

/// The picker is remembered and reaches the renderer — and costs nothing.
///
/// An artwork set is always all three destinations, so the field decides none of
/// their pixels. It must therefore stay out of the fingerprint: while it was in,
/// changing the dropdown retired finished artwork and blocked the blog publish
/// behind "Design changed", over a setting that only steers an image model.
#[test]
fn the_format_picker_is_persisted_without_retiring_the_artwork() {
    let root = scratch("portrait");
    let mut portrait = card();
    portrait.format = crate::thumbnail::format::Format::Vertical;
    save(&root, &portrait).unwrap();
    assert_eq!(load(&root).format.size(), (720, 1280));
    assert_eq!(render::payload(&portrait, None, 720, 1280)["format"], "vertical");
    assert_eq!(portrait.fingerprint(), card().fingerprint());
    let legacy: Card = serde_json::from_str(r#"{"title":"Legacy"}"#).unwrap();
    assert_eq!(legacy.format.size(), (1280, 720));
    let _ = std::fs::remove_dir_all(root);
}
