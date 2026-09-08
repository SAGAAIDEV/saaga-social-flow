//! The procedural thumbnail: presenter on the left, words on the right — or,
//! on the portrait poster, words on top and the presenter beneath — drawn the
//! same way every time.
//!
//! A second way to make the artifact [`crate::thumbnail`] makes, and deliberately
//! nothing like it. That one describes a picture to an image model and gets back
//! something nobody can predict or reproduce; this one lays out a photograph and
//! two strings and gets back exactly what the last one looked like. Which is the
//! right tool depends on the video, so both write into the *same* candidate
//! ledger and both appear in the same list on the Video details pane — choosing
//! between them is one decision, made by looking at them side by side, not a
//! choice of pipeline made before either exists.
//!
//! ## Why the layout is TSX
//!
//! The composition is the whole product here, and a composition is something you
//! iterate on by looking at it. `card/Card.tsx` can be rendered and opened in a
//! browser on its own — `bun run card/render.tsx --open` — with no recorder
//! running and no camera attached, which is the loop that makes a layout good.
//! Expressing the same thing as Rust string concatenation would work and would
//! make that loop impossible.
//!
//! It costs one runtime dependency, `bun`, and nothing else: the package has no
//! `node_modules` and never will. See `card/jsx.ts` for why that is worth
//! forty lines of JSX factory.
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | the two strings, where they are stored, and the job |
//! | [`render`] | the strings to HTML, through `bun` |
//! | [`raster`] | that HTML to a JPEG, through an offscreen web view |

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub mod assets;
#[cfg(test)]
pub mod candidate;
pub mod cli;
pub mod raster;
pub mod render;

/// Beside `article.json` and the rest: one small document per stage, edited by
/// hand and read by everything.
pub const CARD_JSON: &str = "card.json";

/// What the card says, and how it is set.
///
/// Its own document rather than fields on [`crate::thumbnail::brief::Brief`],
/// even though both have a `title` and a `description`, because the two words
/// mean different things in each. A brief's `description` is *direction for a
/// model* — "presenter grinning, the editor dimmed behind them" — and it is
/// never rendered. A card's is *text on the picture*. Sharing one box would put
/// stage directions on the thumbnail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// Output format, shared with AI generation in this project.
    #[serde(default)]
    pub format: crate::thumbnail::format::Format,
    /// The headline. Newlines are honoured as deliberate line breaks — the only
    /// control anyone has over where it wraps.
    #[serde(default)]
    pub title: String,
    /// The line under it.
    #[serde(default)]
    pub description: String,
    /// A small word above the title, in brand orange. Empty draws nothing.
    #[serde(default)]
    pub kicker: String,
    /// `dark` or `light`.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Where across the camera still the presenter is, 0–1.
    ///
    /// The still is a wide frame and the photo column is nearly portrait, so
    /// most of the width is cropped and this decides which part survives. It is
    /// the one control that cannot be judged from the text boxes — you move it
    /// and redraw.
    #[serde(default = "default_focus")]
    pub focus: f64,
}

fn default_theme() -> String {
    // Dark, because a thumbnail is seen in a grid of other thumbnails and the
    // light one is the polite neighbour.
    "dark".to_string()
}

fn default_focus() -> f64 {
    0.5
}

impl Default for Card {
    fn default() -> Card {
        Card {
            format: Default::default(),
            title: String::new(),
            description: String::new(),
            kicker: String::new(),
            theme: default_theme(),
            focus: default_focus(),
        }
    }
}

/// The two themes the TSX knows about, in the order the picker shows them.
pub const THEMES: [&str; 2] = ["dark", "light"];

impl Card {
    /// Nothing to draw. The title alone is enough — a card is a headline with a
    /// picture, and the description is the optional half.
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty()
    }

    /// Everything that decides what the picture looks like, as one string.
    ///
    /// Hashed into the artwork set's identity, so a set drawn before an edit is
    /// refused at publish time and one drawn after it is not. `focus` is in here
    /// because moving it is the whole reason someone redraws a card whose words
    /// did not change.
    ///
    /// The version prefix is bumped whenever the layout itself changes — the
    /// words moving to the other side of the photo, say — because a set drawn
    /// under the old layout is then the wrong picture for the same words, and
    /// without the bump it would pass every freshness check and never be redrawn.
    ///
    /// `format` is deliberately *not*. The set is always all three destinations
    /// and each one composes on its own artboard — see `assets::Kind` — so the
    /// field changes nothing about these pixels. Including it made the format
    /// picker retire finished artwork and block publishing, over a setting whose
    /// only real effect is on the prompt sent to an image model.
    pub fn fingerprint(&self) -> String {
        format!(
            "card-v4\n{}\n{}\n{}\n{}\n{:.4}",
            self.title.trim(),
            self.description.trim(),
            self.kicker.trim(),
            self.theme.trim(),
            self.focus,
        )
    }

    /// The theme, forced to one the TSX will accept.
    ///
    /// A `card.json` edited by hand can say anything, and an unknown theme would
    /// reach the renderer as `THEMES[undefined]` and draw a card with no colours
    /// at all rather than an error.
    pub fn theme_or_default(&self) -> &str {
        match THEMES.contains(&self.theme.trim()) {
            true => self.theme.trim(),
            false => "dark",
        }
    }

    /// The focus, clamped. Outside 0–1 `object-position` clamps anyway, but the
    /// stored value would keep drifting further every time it was nudged.
    pub fn focus_clamped(&self) -> f64 {
        if self.focus.is_finite() {
            self.focus.clamp(0.0, 1.0)
        } else {
            default_focus()
        }
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join(CARD_JSON)
}

/// The card as stored, or an empty one. Never an error: an unreadable or
/// unparseable `card.json` should leave the boxes blank to be typed into, not
/// take the tab down.
pub fn load(root: &Path) -> Card {
    std::fs::read_to_string(path(root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(root: &Path, card: &Card) -> Result<PathBuf> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let file = path(root);
    let json = serde_json::to_string_pretty(card).context("serializing the card")?;
    std::fs::write(&file, json).with_context(|| format!("writing {}", file.display()))?;
    Ok(file)
}

/// Where the page the web view loads is written.
///
/// Inside the project rather than in a temp directory, because the web view is
/// given read access to the project root and nothing else — the photo it
/// references lives there, and a page loaded from `/tmp` could not reach it.
#[cfg(test)]
pub fn page_path(root: &Path) -> PathBuf {
    root.join(crate::thumbnail::still::STILLS_DIR)
        .parent()
        .unwrap_or(root)
        .join("card.html")
}

/// The still a card is drawn over: the newest one captured.
///
/// The same one the Video details pane shows as your photo, so what is on screen
/// is what gets drawn. `None` is not an error — a card with no photograph is a
/// title card, which is a legitimate thumbnail.
pub fn photo(root: &Path) -> Option<PathBuf> {
    crate::thumbnail::still::list(root).into_iter().next()
}

/// Checks everything that can be refused before `bun` is started.
pub fn ready(root: &Path, card: &Card) -> Result<()> {
    if card.is_empty() {
        bail!("type a title first — a card is a headline with a picture");
    }
    if !render::script(root).is_file() {
        bail!(
            "the card renderer is missing from {}",
            render::script(root).display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
