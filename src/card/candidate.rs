//! Filing a drawn card in the thumbnail ledger.
//!
//! The card is a *second producer* for [`crate::thumbnail`]'s candidate list,
//! not a parallel one. Same directory, same `thumbnails.jsonl`, same Select
//! button, same activation event — so choosing between a drawn card and a
//! generated picture is one decision made by looking at both, rather than a
//! choice of pipeline made before either exists.
//!
//! The only field that distinguishes them is `model`, which reads `card` here
//! where the other path writes an OpenRouter model id. That is exactly what it
//! is for: the row records what made the picture.

use std::path::Path;

use anyhow::{Context, Result};

use super::Card;
use crate::thumbnail::schema;

/// What the `model` field says for a procedurally drawn card.
pub const MODEL: &str = "card";

/// The id a card with these inputs would have.
///
/// Through [`schema::candidate_id`] like everything else in the ledger, so the
/// same-inputs-same-id property holds across both producers. The card's whole
/// state goes into the `brief_hash` slot: it is the same idea — everything that
/// decides what the picture looks like, hashed — and reusing the field keeps the
/// row shape identical for both.
pub fn id(card: &Card, still: &str) -> String {
    schema::candidate_id(
        MODEL,
        &crate::agent::prompt::hash_of(&card.fingerprint()),
        still,
        // No screen grab and no style references: a card is laid out, not
        // imagined, so neither would change a pixel of it.
        "",
        "",
        0,
    )
}

/// The camera still's content hash, or an empty string when there is none.
///
/// Empty rather than absent so a card drawn with no photograph still has a
/// stable id — and a different one from the same words drawn over a face.
pub fn still_hash(photo: Option<&Path>) -> String {
    photo
        .and_then(|path| std::fs::read(path).ok())
        .map(|bytes| crate::agent::prompt::hash_of_bytes(&bytes))
        .unwrap_or_default()
}

/// Writes the JPEG into the candidates directory and appends its row.
pub fn write(
    root: &Path,
    id: &str,
    jpeg: &[u8],
    card: &Card,
    still: &str,
) -> Result<schema::Candidate> {
    let dir = root.join(schema::CANDIDATES_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = format!("{}/{id}.jpg", schema::CANDIDATES_DIR);
    let path = root.join(&file);
    std::fs::write(&path, jpeg).with_context(|| format!("writing {}", path.display()))?;

    let row = schema::Candidate {
        id: id.to_string(),
        model: MODEL.to_string(),
        file,
        created_at: crate::schedule::ledger::now_rfc3339(),
        brief_hash: crate::agent::prompt::hash_of(&card.fingerprint()),
        still: still.to_string(),
        screen: None,
        refs: String::new(),
    };
    schema::append(root, &schema::Row::Candidate(row.clone()))?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Card {
        Card {
            title: "Ship it anyway".into(),
            description: "Why the queue fell over.".into(),
            kicker: "SAAGA".into(),
            theme: "dark".into(),
            focus: 0.5,
            format: Default::default(),
        }
    }

    /// Same inputs, same id — which is what lets a redraw of unchanged words
    /// recognise the picture it already made.
    #[test]
    fn the_id_is_stable_for_the_same_card_and_still() {
        assert_eq!(id(&card(), "abc"), id(&card(), "abc"));
    }

    /// Every field that reaches the layout has to move the id, or an edit would
    /// silently return the previous picture.
    #[test]
    fn every_edit_moves_the_id() {
        let base = id(&card(), "abc");
        for edited in [
            Card {
                title: "Ship it".into(),
                ..card()
            },
            Card {
                description: "Something else.".into(),
                ..card()
            },
            Card {
                kicker: String::new(),
                ..card()
            },
            Card {
                theme: "light".into(),
                ..card()
            },
            Card {
                focus: 0.31,
                ..card()
            },
        ] {
            assert_ne!(id(&edited, "abc"), base, "{edited:?} drew the old card");
        }
    }

    /// Moving the focus slider is the one edit whose whole point is that the
    /// words did not change, so it is the one most likely to be forgotten.
    #[test]
    fn a_nudged_focus_is_a_different_picture() {
        let mut nudged = card();
        nudged.focus = 0.5001;
        assert_ne!(id(&nudged, "abc"), id(&card(), "abc"));
    }

    /// A new still is a new picture even with the same words.
    #[test]
    fn a_different_still_is_a_different_card() {
        assert_ne!(id(&card(), "abc"), id(&card(), "def"));
    }

    /// A card with no photograph still needs a stable id, and a different one
    /// from the same words over a face.
    #[test]
    fn a_card_with_no_photo_has_its_own_id() {
        let none = still_hash(None);
        assert_eq!(none, "");
        assert_ne!(id(&card(), &none), id(&card(), "abc"));
    }

    /// The row is what puts a card in the same list as a generated picture, and
    /// `model` is the only thing that says which is which.
    #[test]
    fn the_row_files_under_the_card_model() {
        let root =
            std::env::temp_dir().join(format!("stream-recorder-card-{}-row", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let id = id(&card(), "abc");
        let row = write(&root, &id, b"pretend jpeg", &card(), "abc").unwrap();
        assert_eq!(row.model, MODEL);
        assert_eq!(row.file, format!("thumbnails/candidates/{id}.jpg"));
        assert!(root.join(&row.file).is_file());

        let rows = schema::load(&root);
        assert_eq!(
            schema::candidates(&rows).len(),
            1,
            "it is in the shared list"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
