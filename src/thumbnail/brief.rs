//! What the image model is told, written by hand.
//!
//! No language model in front of this. A prompt written for you is one you then
//! have to read, disagree with, and edit — three steps to arrive where typing it
//! starts. The pictures do the describing anyway: the camera still, the screen
//! grab and the style references all travel with these words, so what is left to
//! say is short.
//!
//! Two fields, because they are two different kinds of instruction. `title` is
//! text that must appear in the image verbatim; `description` is how to edit the
//! picture. Mixing them into one box meant the words to render and the direction
//! for rendering them competed in the same sentence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    /// The words to put on the image, verbatim.
    #[serde(default)]
    pub title: String,
    /// How to edit the picture: the presenter, the screen, the light, the mood.
    #[serde(default)]
    pub description: String,
}

impl Brief {
    /// Exactly what the image model is sent.
    ///
    /// The title is quoted and told to be set as given, because an unquoted
    /// phrase reads as a description of the subject rather than as text to draw.
    pub fn render(&self) -> String {
        self.render_for(super::format::Format::Horizontal)
    }

    pub fn render_for(&self, format: super::format::Format) -> String {
        let mut out = String::new();
        if !self.description.trim().is_empty() {
            out.push_str(self.description.trim());
        }
        if !self.title.trim().is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!(
                "Set this text on the image, exactly as written: \"{}\"",
                self.title.trim()
            ));
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("{} thumbnail, the text legible at small size.", format.aspect_ratio()));
        out
    }

    /// Nothing to draw from: both boxes empty.
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.description.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> Brief {
        Brief {
            title: "SHIP IT ANYWAY".into(),
            description: "Presenter grinning, the editor dimmed behind them".into(),
        }
    }

    #[test]
    fn both_fields_reach_the_prompt() {
        let rendered = brief().render();
        assert!(rendered.starts_with("Presenter grinning"));
        assert!(rendered.contains("exactly as written: \"SHIP IT ANYWAY\""));
        assert!(rendered.contains("16:9"));
    }

    /// A title on its own is a legitimate brief — the pictures carry the rest.
    #[test]
    fn either_field_alone_still_renders() {
        let title_only = Brief { title: "SHIP IT".into(), ..Brief::default() };
        assert!(title_only.render().contains("\"SHIP IT\""));

        let description_only = Brief {
            description: "make it warmer".into(),
            ..Brief::default()
        };
        let rendered = description_only.render();
        assert!(rendered.starts_with("make it warmer"));
        assert!(!rendered.contains("exactly as written"), "no empty quotes");
    }

    /// The candidate id is keyed on this, so an edit has to move it and
    /// whitespace has to not — saving a stray space must not redraw everything.
    #[test]
    fn editing_changes_the_render_and_whitespace_does_not() {
        let mut edited = brief();
        edited.description = "Presenter scowling".into();
        assert_ne!(edited.render(), brief().render());

        let mut padded = brief();
        padded.title = format!("  {}  ", brief().title);
        padded.description = format!("\n{}\n", brief().description);
        assert_eq!(padded.render(), brief().render());
    }

    #[test]
    fn empty_is_recognised() {
        assert!(Brief::default().is_empty());
        assert!(Brief { title: "  ".into(), description: "\n".into() }.is_empty());
        assert!(!brief().is_empty());
    }
}
