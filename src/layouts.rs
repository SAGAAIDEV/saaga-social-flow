//! The 2x2 of hyperframes layouts a chapter can be recorded for, and where each
//! one's screen slot sits.
//!
//! A chapter is recorded *for* a layout. The layout decides the aspect the
//! screen capture has to arrive in, because the composition drops that capture
//! into a fixed hole with `object-fit: cover` — hand it the wrong aspect and the
//! edges are silently trimmed, and the first anyone sees of it is in the edit.
//! Recording at the slot's aspect is what makes the framing you set the framing
//! you get.
//!
//! ```text
//!                 Horizontal                    Vertical
//!  Talking head   talking-head-horizontal       talking-head-vertical
//!                 1920x1080, camera only        1080x1920, camera only
//!  Split          screen-camera-split           screen-camera-vertical
//!                 screen 0,0,1402.562,1080      screen 0,0,1080,1280
//! ```
//!
//! Two things fall out of that table rather than out of special cases at the
//! call sites. Only the Split pair has a [`Layout::screen_slot`], so a talking
//! head chapter runs no `SCStream` at all — the `None` is the whole
//! implementation of "no screen capture for this layout". And the three screen
//! aspects a recording can need are 1.299:1 and 0.844:1 and nothing else, so
//! "horizontal vs vertical" is not the axis that matters; the *block* is.
//!
//! ## Hand-transcribed, with a test that notices drift
//!
//! These rects live in the compositions' CSS, not in any manifest —
//! `library.json` indexes the blocks and declares which media each consumes,
//! but carries no geometry. Four numbers transcribed once beat a CSS parser in
//! the recording hot path, and `slots_match_the_composition_css` below re-reads
//! the sibling project and fails if a re-export moved them. It skips with a
//! printed note when `screencast/` is not checked out beside this crate, so the
//! recorder still builds standalone.
//!
//! ## The camera slot is here too, and it is not symmetric with the screen's
//!
//! [`Layout::camera_slot`] is never `None` — every one of these layouts has a
//! camera, which is the one thing a chapter always records. Unlike the screen
//! slot it does **not** decide what gets captured: the camera master is written
//! full-frame with its audio, and this rect only says where the camera lands
//! inside a composite. Framing within it stays a placement decision
//! ([`crate::region::cover`]), so `cameraPosition` and `face_track.json` can
//! still revise it downstream — which is where it is decided today.
//!
//! [`Layout::topmost`] is carried for the same reason the rects are: the two
//! Split layouts paint their two media layers in *opposite* orders, so it
//! cannot be inferred.

/// Which of the two layout pairs a chapter is recorded for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pair {
    TalkingHead,
    Split,
}

/// Which half of a pair — i.e. which output the chapter is framed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Pair {
    pub const ALL: [Pair; 2] = [Pair::TalkingHead, Pair::Split];

    pub fn as_str(self) -> &'static str {
        match self {
            Pair::TalkingHead => "Talking Head",
            Pair::Split => "Split",
        }
    }
}

impl Orientation {
    pub const ALL: [Orientation; 2] = [Orientation::Horizontal, Orientation::Vertical];

    pub fn as_str(self) -> &'static str {
        match self {
            Orientation::Horizontal => "Horizontal",
            Orientation::Vertical => "Vertical",
        }
    }
}

/// Which of the two media layers a layout paints **last**, and therefore draws
/// on top where they overlap.
///
/// Not a constant, and not guessable: the two Split layouts disagree.
/// `screen-camera-split` paints background -> camera -> screen, so the screen
/// covers the camera's left edge — its screen slot ends at 1402.562 while the
/// camera column starts at 1398, and that 4.5px is deliberate, described in its
/// own CSS as "the visible join". `screen-camera-vertical` paints background ->
/// screen -> camera panel, the other way round. A composite hardcoded either
/// way draws one of them wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topmost {
    Camera,
    Screen,
}

/// One cell of the 2x2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub pair: Pair,
    pub orientation: Orientation,
    /// The hyperframes block id, matching `library.json` and the filename under
    /// `screencast/components/compositions/`.
    pub block: &'static str,
    /// The composition's canvas, in its own pixels.
    pub canvas: (f64, f64),
    /// `(x, y, w, h)` of the hole the screen capture lands in, in canvas
    /// pixels — `None` when the layout has no screen at all.
    pub screen_slot: Option<(f64, f64, f64, f64)>,
    /// `(x, y, w, h)` of the hole the camera lands in, in canvas pixels.
    ///
    /// Never `None`: every one of these four layouts has a camera, which is
    /// what `library.json`'s `media` says too. The camera is the one thing a
    /// chapter always records.
    pub camera_slot: (f64, f64, f64, f64),
    /// Which layer wins where the two overlap. See [`Topmost`].
    pub topmost: Topmost,
}

/// The 2x2, ordered pair-major so [`Layout::get`] can index rather than search.
/// `layouts_are_indexed_pair_major` pins that ordering.
pub const LAYOUTS: [Layout; 4] = [
    Layout {
        pair: Pair::TalkingHead,
        orientation: Orientation::Horizontal,
        block: "talking-head-horizontal",
        canvas: (1920.0, 1080.0),
        screen_slot: None,
        // Full bleed, and the only slot that matches a 16:9 camera exactly —
        // so `cover` crops nothing and the framing offset is inert here.
        camera_slot: (0.0, 0.0, 1920.0, 1080.0),
        topmost: Topmost::Camera,
    },
    Layout {
        pair: Pair::TalkingHead,
        orientation: Orientation::Vertical,
        block: "talking-head-vertical",
        canvas: (1080.0, 1920.0),
        screen_slot: None,
        // Full bleed of a 9:16 canvas, which crops a 16:9 camera down to 607
        // of its 1920 columns — the hardest crop of the four.
        camera_slot: (0.0, 0.0, 1080.0, 1920.0),
        topmost: Topmost::Camera,
    },
    Layout {
        pair: Pair::Split,
        orientation: Orientation::Horizontal,
        block: "screen-camera-split",
        canvas: (1920.0, 1080.0),
        // Not 1920: the camera column takes the right of the frame, and the
        // screen's right edge at 1402.562 is what its CSS calls "the visible
        // join". A 16:9 capture here loses its sides to `object-fit: cover`.
        screen_slot: Some((0.0, 0.0, 1402.562, 1080.0)),
        // The camera column, starting 4.5px *before* the screen slot ends.
        // That overlap is the point: the screen is painted over it so the two
        // never leave a sub-pixel seam.
        camera_slot: (1398.0, 0.0, 522.0, 1080.0),
        topmost: Topmost::Screen,
    },
    Layout {
        pair: Pair::Split,
        orientation: Orientation::Vertical,
        block: "screen-camera-vertical",
        canvas: (1080.0, 1920.0),
        // The top two thirds; the camera panel has the bottom third.
        screen_slot: Some((0.0, 0.0, 1080.0, 1280.0)),
        // The camera fills its peach panel rather than sitting as an inset
        // card on it — the composition's own comment records that the Figma
        // card was deliberately dropped.
        camera_slot: (0.0, 1280.0, 1080.0, 640.0),
        topmost: Topmost::Camera,
    },
];

impl Layout {
    /// The cell for one pair and orientation. Total, because the 2x2 is
    /// complete — every combination names a real block.
    pub fn get(pair: Pair, orientation: Orientation) -> &'static Layout {
        &LAYOUTS[pair as usize * 2 + orientation as usize]
    }

    /// The screen hole's size in canvas pixels, which is what a region is sized
    /// from. `None` for a layout with no screen.
    pub fn slot_size(&self) -> Option<(f64, f64)> {
        self.screen_slot.map(|(_, _, w, h)| (w, h))
    }

    /// The camera hole's size in canvas pixels — what
    /// [`crate::region::cover`] fits the camera into.
    ///
    /// Read by the composite, which does not exist yet; the slot table and its
    /// drift test are worth landing ahead of it because they are the half that
    /// can be proved against the real CSS.
    #[allow(dead_code)]
    pub fn camera_slot_size(&self) -> (f64, f64) {
        (self.camera_slot.2, self.camera_slot.3)
    }

    /// Whether recording this layout needs the screen captured at all.
    pub fn needs_screen(&self) -> bool {
        self.screen_slot.is_some()
    }

    /// The layout this one's region is parented to, if any.
    ///
    /// Vertical follows Horizontal within a pair. They are two crops of the
    /// same demonstration, so framing them independently means aiming at the
    /// same content twice and holding both in your head; parenting makes the
    /// horizontal region the thing you aim and the vertical a relationship that
    /// travels with it.
    ///
    /// Horizontal is the parent rather than the other way round because it is
    /// the longform master — the one a chapter is usually framed for — and
    /// because a root has to be the one whose offset is absolute.
    ///
    /// A single optional link, not a graph: [`crate::region::placement::resolve`]
    /// needs the parent resolved first, and one link makes that ordering
    /// obvious instead of requiring a topological sort.
    pub fn parent(&self) -> Option<&'static Layout> {
        match (self.pair, self.orientation) {
            (Pair::Split, Orientation::Vertical) => {
                Some(Layout::get(Pair::Split, Orientation::Horizontal))
            }
            _ => None,
        }
    }

    /// `"Split · Horizontal"` — for dropdowns, labels and log lines.
    pub fn label(&self) -> String {
        format!("{} · {}", self.pair.as_str(), self.orientation.as_str())
    }

    /// Resolve a hyperframes block id, for `--layout`.
    ///
    /// Keyed on the block id rather than on a `Pair`/`Orientation` spelling
    /// because the block id is the name that already exists everywhere else —
    /// `library.json`, the composition filename, the edit stage's template
    /// assignment — so there is one vocabulary rather than two.
    pub fn from_block(block: &str) -> Option<&'static Layout> {
        LAYOUTS.iter().find(|layout| layout.block == block)
    }

    /// Every valid `--layout` value, for help text and error messages.
    pub fn block_ids() -> [&'static str; LAYOUTS.len()] {
        [
            LAYOUTS[0].block,
            LAYOUTS[1].block,
            LAYOUTS[2].block,
            LAYOUTS[3].block,
        ]
    }
}

#[cfg(test)]
#[path = "layouts_tests.rs"]
mod tests;
