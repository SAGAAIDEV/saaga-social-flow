//! How the camera is aimed inside its layout slot: a punch-in, and where the
//! subject is right now.
//!
//! Every layout drops the camera into a fixed hole with `object-fit: cover`,
//! and [`super::cover`] answers *which part* of the camera that hole shows for
//! a given `object-position`. Until now that position was the literal constant
//! `(0.5, 0.5)`, written twice — once in `ops::composite` and once in
//! `ops::crop`. This module is that constant grown up: the same answer when
//! nothing is tracking, and a live one when something is.
//!
//! ## Why the shared cell holds a *point*, not an offset
//!
//! An offset means nothing without a slot. The horizontal composite and the
//! vertical one crop the same camera through holes of wildly different aspect —
//! 522×1080 against 1080×1920 — so one offset cannot serve both, and a detector
//! that published offsets would have to know every slot it feeds.
//!
//! A point in the camera's own normalized space has no such problem: it is
//! where the subject *is*, which is a fact about the frame and not about any
//! output. Each consumer converts it to its own offset through
//! [`offset_for_point`](super::cover::offset_for_point), using its own slot and
//! its own zoom. That is what lets one detector run once per frame and frame
//! two orientations correctly, and it is why smoothing happens on the point,
//! upstream of the split, rather than separately per output where the two
//! would drift out of step.
//!
//! ## Why the cell is a `Mutex` when only one thread writes it
//!
//! Today the op that writes it and the composites that read it are all on the
//! camera capture queue, in one graph walk, so the lock is uncontended and a
//! `Cell` would do. It is a `Mutex` anyway because the cell is the documented
//! seam for moving detection onto its own thread if the inline cost ever stops
//! being affordable — see `face::FaceTracker` for the measurement that says it
//! currently is. Choosing the cheaper primitive would put a thread-safety
//! change in the way of a performance change that should not need one.

use std::sync::{Arc, Mutex};

use super::cover::offset_for_point;

/// Where the subject is in the camera frame, normalized against the frame:
/// `(0.5, 0.5)` is dead centre, `(0.0, 0.0)` the top-left corner.
///
/// Top-left origin with y growing **down**, like every other rect in this
/// recorder and unlike Core Image — the flip into Core Image's space happens
/// once, in `ops::crop::flip_rect`, and never here.
pub type Anchor = (f64, f64);

/// The latest smoothed anchor, shared between whatever is tracking and whatever
/// is framing.
///
/// `None` means "nothing has been tracked yet", which is a different state from
/// "the subject is centred" and must stay so: a composite that read a missing
/// anchor as `(0.5, 0.5)` would be indistinguishable from one reading a
/// genuinely centred subject, and the first frames of a session — before the
/// first detection lands — would silently claim a face that has not been found.
/// Callers get `Option` and decide.
#[derive(Debug, Default)]
pub struct AnchorCell {
    anchor: Mutex<Option<Anchor>>,
}

impl AnchorCell {
    pub fn new() -> AnchorCell {
        AnchorCell {
            anchor: Mutex::new(None),
        }
    }

    /// The latest anchor, or `None` before the first one lands.
    ///
    /// A poisoned lock reads as `None` rather than panicking. This is called
    /// per frame per output on a capture queue, and the house rule there is
    /// "return, never unwrap" — the cost of a poisoned cell should be a
    /// centred frame, not a dead recording.
    pub fn get(&self) -> Option<Anchor> {
        self.anchor.lock().ok().and_then(|a| *a)
    }

    pub fn set(&self, anchor: Option<Anchor>) {
        if let Ok(mut slot) = self.anchor.lock() {
            *slot = anchor;
        }
    }
}

/// Where the pointer is in the captured screen, and how far the frame is
/// punched in.
///
/// `anchor` is normalized to the **captured buffer** — `(0.5, 0.5)` is the
/// middle of what `SCStream` is delivering — and not to either crop. That is
/// the same argument [`AnchorCell`] makes for the camera, and it matters for
/// the same reason: the horizontal and vertical composites crop the one
/// screen buffer through slots of wildly different shape, so an anchor
/// expressed against one of them means nothing to the other. A position in
/// the capture's own space is a fact about the screen rather than about any
/// output, and each composite converts it into its own region on the way in.
///
/// It also survives the operator dragging a region. The preview graph is
/// rebuilt on every drag release and both crops move underneath, but where
/// the pointer is on the captured screen is unchanged by any of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub anchor: Anchor,
    /// How far punched in, normalized to the room the region has: `0.0` is
    /// all of it, `1.0` is as tight as the 1:1 floor allows. See
    /// [`crate::region::track::tracked_crop`] — travel and punch-in are the
    /// same quantity, so this is also "how far the frame is free to move".
    pub punch: f64,
}

/// The latest tracked framing, shared between the pointer tracker and the
/// composite that crops for it.
///
/// One cell holding both numbers rather than two cells holding one each, and
/// that is the point of the type: a frame that read this tick's anchor beside
/// last tick's punch would crop a rect neither of them asked for, and it would
/// do it intermittently, under load, which is the worst way to find out.
///
/// ## Why this recovers from a poisoned lock and [`AnchorCell`] does not
///
/// [`AnchorCell::get`] answers a poisoned lock with `None` — the framing falls
/// back to centre and stays there. That is survivable for a face because
/// centre is a defensible place to point a camera. Here it is not: `None`
/// means "hold the authored framing", so a single poisoned lock would switch
/// tracking off silently for the rest of the session with nothing logged, and
/// the operator would be left recording a feature they believe is on.
///
/// So this takes the recovering convention `Tap::publish_buffer` and
/// `PreviewPort::publish` use instead — `unwrap_or_else(|e| e.into_inner())`.
/// The data behind this lock is two `f64`s written as a unit; there is no
/// invariant a panicking writer could have left half-applied, so there is
/// nothing for the poison flag to protect.
#[derive(Debug, Default)]
pub struct TrackCell {
    track: Mutex<Option<Track>>,
}

impl TrackCell {
    pub fn new() -> TrackCell {
        TrackCell {
            track: Mutex::new(None),
        }
    }

    /// The latest tracked framing, or `None` before the first one lands —
    /// which the composite reads as "show the authored region", the same thing
    /// it did before tracking existed.
    pub fn get(&self) -> Option<Track> {
        *self.track.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, track: Option<Track>) {
        *self.track.lock().unwrap_or_else(|e| e.into_inner()) = track;
    }
}

/// One output's framing rule: how far to punch in, and what to aim at.
#[derive(Clone)]
pub struct Framing {
    /// Multiplies `cover`'s scale. `1.0` keeps every pixel the slot's aspect
    /// allows; above that the slot is filled from fewer source pixels and gains
    /// somewhere to slide. See [`super::cover::cover_zoom`].
    zoom: f64,
    /// `None` when nothing is tracking, which is the default and the whole of
    /// what this type did before face tracking existed.
    anchor: Option<Arc<AnchorCell>>,
}

impl Framing {
    /// Dead centre, no punch-in: byte-for-byte what every composite did before
    /// this type existed.
    pub fn fixed() -> Framing {
        Framing {
            zoom: 1.0,
            anchor: None,
        }
    }

    /// Punched in by `zoom`, aimed at whatever `anchor` last reported.
    ///
    /// `anchor` is `None` for a layout that should punch in but not track,
    /// which is a legitimate combination — it frames tighter and stays put.
    pub fn tracked(zoom: f64, anchor: Option<Arc<AnchorCell>>) -> Framing {
        Framing { zoom, anchor }
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Whether this framing can move at all. Read by the ops' sidecar
    /// reporting, so a chapter records whether it was tracked rather than
    /// leaving it to be inferred from the pixels.
    pub fn is_tracking(&self) -> bool {
        self.anchor.is_some()
    }

    /// Where this framing is currently aimed, in the camera's own normalized
    /// space. `None` when nothing is tracking or nothing has been found.
    ///
    /// For reporting only — the ops go through
    /// [`offset_into`](Framing::offset_into), which is the same reading turned
    /// into the answer a particular slot needs.
    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor.as_ref().and_then(|cell| cell.get())
    }

    /// The `object-position` offset to hand [`super::cover::cover_zoom`] for a
    /// `src`-sized camera in a `slot`-sized hole.
    ///
    /// Falls back to centred whenever there is nothing to aim at — no cell, or
    /// a cell that has not seen a face yet. That fallback is the reason
    /// enabling tracking cannot make a recording worse than leaving it off: the
    /// failure mode of a detector that never finds anything is exactly the
    /// framing that was shipping before it existed.
    pub fn offset_into(&self, src: (f64, f64), slot: (f64, f64)) -> (f64, f64) {
        match self.anchor.as_ref().and_then(|cell| cell.get()) {
            Some(point) => offset_for_point(src, slot, self.zoom, point),
            None => (0.5, 0.5),
        }
    }
}

impl Default for Framing {
    fn default() -> Self {
        Framing::fixed()
    }
}

impl std::fmt::Debug for Framing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framing")
            .field("zoom", &self.zoom)
            .field("anchor", &self.anchor.as_ref().and_then(|c| c.get()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAMERA: (f64, f64) = (1920.0, 1080.0);
    const SPLIT_H_SLOT: (f64, f64) = (522.0, 1080.0);
    const TALKING_H_SLOT: (f64, f64) = (1920.0, 1080.0);

    /// The property that makes the feature safe to ship switched on: with
    /// nothing tracked, every framing answers exactly what the hardcoded
    /// constant used to.
    #[test]
    fn framing_with_nothing_tracked_is_the_old_hardcoded_centre() {
        let fixed = Framing::fixed();
        assert_eq!(fixed.offset_into(CAMERA, SPLIT_H_SLOT), (0.5, 0.5));

        // A tracked framing whose detector has not found a face yet, too.
        let cell = Arc::new(AnchorCell::new());
        let waiting = Framing::tracked(1.14, Some(Arc::clone(&cell)));
        assert_eq!(waiting.offset_into(CAMERA, SPLIT_H_SLOT), (0.5, 0.5));
        assert_eq!(cell.get(), None, "an unread cell stays empty");
    }

    #[test]
    fn an_anchor_left_of_centre_slides_the_split_column_left() {
        let cell = Arc::new(AnchorCell::new());
        let framing = Framing::tracked(1.0, Some(Arc::clone(&cell)));
        cell.set(Some((0.30, 0.5)));
        let offset = framing.offset_into(CAMERA, SPLIT_H_SLOT);
        assert!(offset.0 < 0.5, "got {offset:?}");
        // 0.30 * 1920 = 576, minus half of the 522px window = 315, over 1398
        // of travel.
        assert!((offset.0 - (576.0 - 261.0) / 1398.0).abs() < 1e-9, "{offset:?}");
    }

    /// Tracking the full-bleed longform is a no-op without a punch-in, and
    /// works with one. Both halves matter: the first is why `zoom` exists, the
    /// second is that it does its job.
    #[test]
    fn the_full_bleed_longform_only_moves_once_it_is_punched_in() {
        let cell = Arc::new(AnchorCell::new());
        cell.set(Some((0.35, 0.40)));

        let flat = Framing::tracked(1.0, Some(Arc::clone(&cell)));
        assert_eq!(
            flat.offset_into(CAMERA, TALKING_H_SLOT),
            (0.5, 0.5),
            "at zoom 1.0 a 16:9 camera in a 16:9 slot cannot move"
        );

        let punched = Framing::tracked(1.14, Some(Arc::clone(&cell)));
        let offset = punched.offset_into(CAMERA, TALKING_H_SLOT);
        assert!(offset.0 < 0.5 && offset.1 < 0.5, "got {offset:?}");
    }

    #[test]
    fn clearing_the_cell_returns_the_framing_to_centre() {
        let cell = Arc::new(AnchorCell::new());
        let framing = Framing::tracked(1.0, Some(Arc::clone(&cell)));
        cell.set(Some((0.2, 0.5)));
        assert!(framing.offset_into(CAMERA, SPLIT_H_SLOT).0 < 0.5);
        cell.set(None);
        assert_eq!(framing.offset_into(CAMERA, SPLIT_H_SLOT), (0.5, 0.5));
    }
}
