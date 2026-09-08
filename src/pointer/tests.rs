//! What holding the combo does, one behaviour per test.
//!
//! All of it is arithmetic over an anchor and a boolean, so none of it needs a
//! display, a pointer, or a window server — the sensor is the op's problem and
//! is spiked separately in [`super::sample`]. What is worth pinning here is the
//! *latch*, because every one of its failures is a frame that moves when it
//! should be still, and that is the difference between a punch-in you can
//! watch and one that makes a viewer seasick.

use super::*;
use crate::config::MouseTracking;

const FRAME: f64 = 1.0 / 30.0;

fn tracker() -> PointerTracker {
    PointerTracker::new(&MouseTracking::default())
}

/// Run `frames` frames at `anchor` with the combo `held`, starting at `t`.
/// Returns the clock it ended on.
fn run(tracker: &PointerTracker, anchor: Anchor, held: bool, frames: u32, t: f64) -> f64 {
    let mut t = t;
    for _ in 0..frames {
        tracker.track(Some(anchor), held, t);
        t += FRAME;
    }
    t
}

fn published(tracker: &PointerTracker) -> Track {
    tracker.cell().get().expect("nothing published")
}

/// Before the pointer has ever been seen there is no framing to publish, and
/// `None` is not the same as centred — a composite reading a missing anchor as
/// the middle of the region would claim a pointer that has not been found.
#[test]
fn nothing_is_published_before_the_first_reading() {
    let tracker = tracker();
    tracker.track(None, false, 0.0);
    assert!(
        tracker.cell().get().is_none(),
        "published a framing before the pointer was ever read",
    );
}

/// Untouched, the frame stays all the way out — switching tracking on must not
/// punch in on its own.
#[test]
fn an_unheld_frame_stays_wide() {
    let tracker = tracker();
    run(&tracker, (0.5, 0.5), false, 60, 0.0);
    assert!(
        published(&tracker).punch < 1e-3,
        "the frame punched in with nothing held: {}",
        published(&tracker).punch,
    );
}

/// The headline behaviour, and the one the whole latch exists for.
///
/// You punch in on something and then point at parts of it. If the frame
/// chased the pointer while you did, the punch-in would be unusable for the
/// thing it is for. So the press captures a position and the frame stays
/// there, however far the pointer then roams.
#[test]
fn holding_pins_the_frame_where_it_was_pressed() {
    let tracker = tracker();
    // Settle on the left, then press there.
    let t = run(&tracker, (0.2, 0.5), false, 60, 0.0);
    let t = run(&tracker, (0.2, 0.5), true, 2, t);
    let pinned = published(&tracker).anchor;

    // Now walk the pointer all the way across, still holding.
    let t = run(&tracker, (0.9, 0.5), true, 60, t);
    let after = published(&tracker);

    assert!(
        (after.anchor.0 - pinned.0).abs() < 1e-9 && (after.anchor.1 - pinned.1).abs() < 1e-9,
        "the frame followed the pointer while held: pinned at {pinned:?}, \
         ended at {:?}",
        after.anchor,
    );
    // 0.98 rather than 0.99, and not a rounding fudge: the glide closes a
    // fixed proportion of the remaining distance each frame, so it approaches
    // 1.0 asymptotically and never lands on it. Two seconds at the default
    // 0.45s ramp reaches ~0.99, which crops to the same whole pixel as fully
    // in. `Glide::is_at` is the settled test; this one only cares that the
    // punch actually happened.
    assert!(
        after.punch > 0.98,
        "two seconds of holding only reached punch {}",
        after.punch,
    );
    let _ = t;
}

/// Releasing eases the frame back out rather than cutting to wide.
#[test]
fn releasing_eases_the_frame_back_out() {
    let tracker = tracker();
    let t = run(&tracker, (0.3, 0.5), false, 30, 0.0);
    let t = run(&tracker, (0.3, 0.5), true, 60, t);
    assert!(published(&tracker).punch > 0.98, "did not punch in");

    // One frame after the release it must have *started* back, not arrived.
    tracker.track(Some((0.3, 0.5)), false, t);
    let one_frame = published(&tracker).punch;
    assert!(
        one_frame < 0.97 && one_frame > 0.5,
        "one frame after release the punch was {one_frame} — that is a cut, \
         not an ease",
    );

    // Five seconds, not three. The ease-out is asymptotic like the ease-in,
    // and `Glide`'s settled threshold is a thousandth — which at the default
    // 0.45s ramp is a shade over three seconds away. This is also what
    // releases the latch, so the number has to clear that bar and not merely
    // look small.
    run(&tracker, (0.3, 0.5), false, 150, t + FRAME);
    assert!(
        published(&tracker).punch < 1e-3,
        "five seconds after release the frame had not returned: {}",
        published(&tracker).punch,
    );
}

/// The latch must outlive the key, or the frame widens *and* slides at once.
///
/// Two motions where the operator asked for one. Held until the punch is
/// spent, the release is a straight zoom out from where the frame already was.
#[test]
fn the_latch_outlives_the_release_until_the_frame_is_all_the_way_out() {
    let tracker = tracker();
    let t = run(&tracker, (0.2, 0.5), false, 30, 0.0);
    let t = run(&tracker, (0.2, 0.5), true, 60, t);
    let pinned = published(&tracker).anchor;

    // Release, and move the pointer far away while it eases out.
    let mut t = t;
    for _ in 0..3 {
        tracker.track(Some((0.9, 0.5)), false, t);
        t += FRAME;
        let now = published(&tracker);
        assert!(
            (now.anchor.0 - pinned.0).abs() < 1e-9,
            "the frame slid toward the pointer while easing out: {:?} vs \
             {pinned:?} at punch {}",
            now.anchor,
            now.punch,
        );
    }
}

/// A fresh press picks up the pointer's new position, so punching in twice in
/// two places works without switching anything off.
#[test]
fn a_fresh_press_latches_the_new_position() {
    let tracker = tracker();
    let t = run(&tracker, (0.2, 0.5), false, 30, 0.0);
    let t = run(&tracker, (0.2, 0.5), true, 30, t);
    let first = published(&tracker).anchor;

    // Release fully, settle somewhere else, press again.
    let t = run(&tracker, (0.8, 0.5), false, 150, t);
    let t = run(&tracker, (0.8, 0.5), true, 2, t);
    let second = published(&tracker).anchor;

    assert!(
        second.0 > first.0 + 0.4,
        "the second press latched {second:?}, barely moved from {first:?} — \
         the latch was never let go",
    );
    let _ = t;
}

/// The pointer leaving the captured display holds the framing rather than
/// re-centring it: reaching across to a second monitor is not a request to
/// reframe the take.
#[test]
fn losing_the_pointer_holds_the_framing() {
    let tracker = tracker();
    let t = run(&tracker, (0.25, 0.5), false, 60, 0.0);
    let before = published(&tracker).anchor;

    let mut t = t;
    for _ in 0..90 {
        tracker.track(None, false, t);
        t += FRAME;
    }
    let after = published(&tracker).anchor;
    assert!(
        (after.0 - before.0).abs() < 1e-6 && (after.1 - before.1).abs() < 1e-6,
        "three seconds with the pointer off the display moved the framing \
         from {before:?} to {after:?}",
    );
}
