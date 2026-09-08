//! What the smoother is *for*, one failure per test.
//!
//! Every case here is a thing that visibly ruins a take, written as the
//! detector input that causes it. They run on synthetic observations rather
//! than on real detections deliberately: the tremor these filters exist to
//! absorb is a property of the numbers, so it can be reproduced exactly, and a
//! regression in the damping should fail here rather than in a review of the
//! footage a week later.

use super::{Damping, Smoother};
use crate::region::framing::Anchor;

/// 30fps, so a "frame" is a real duration and the exponential glide is being
/// asked the question it will actually be asked.
const FRAME: f64 = 1.0 / 30.0;

fn settled(damping: Damping, at: Anchor) -> (Smoother, f64) {
    let mut smoother = Smoother::new(damping);
    let mut t = 0.0;
    smoother.advance(t);
    smoother.observe(at, t);
    // The first observation snaps, so this is already settled.
    t += FRAME;
    smoother.advance(t);
    (smoother, t)
}

/// Run `frames` frames, feeding `observe` a point from `feed` every third one,
/// and return where the framing ended up plus how far it wandered on the way.
fn run(
    smoother: &mut Smoother,
    mut t: f64,
    frames: usize,
    mut feed: impl FnMut(usize) -> Option<Anchor>,
) -> (f64, Anchor, f64) {
    let mut previous = smoother.anchor().unwrap_or((0.5, 0.5));
    let mut largest_step: f64 = 0.0;
    for frame in 0..frames {
        t += FRAME;
        if frame % 3 == 0 {
            match feed(frame) {
                Some(point) => smoother.observe(point, t),
                None => smoother.lost(t),
            }
        }
        if let Some(now) = smoother.advance(t) {
            let step = ((now.0 - previous.0).powi(2) + (now.1 - previous.1).powi(2)).sqrt();
            largest_step = largest_step.max(step);
            previous = now;
        }
    }
    (t, previous, largest_step)
}

/// The headline behaviour: a subject sitting still, with a detector wandering
/// by a percent and a half every sample, produces a framing that does not move.
#[test]
fn a_still_subject_with_a_jittery_detector_holds_a_still_frame() {
    let (mut smoother, t) = settled(Damping::default(), (0.5, 0.5));
    let start = smoother.anchor().expect("acquired");

    // A deterministic wobble of ±0.015 — squarely inside real BlazeFace noise
    // and squarely inside the 0.02 deadband.
    let (_, ended, largest_step) = run(&mut smoother, t, 300, |frame| {
        let phase = frame as f64 * 0.7;
        Some((0.5 + phase.sin() * 0.015, 0.5 + phase.cos() * 0.015))
    });

    let drift = ((ended.0 - start.0).powi(2) + (ended.1 - start.1).powi(2)).sqrt();
    assert!(
        drift < 1e-6,
        "ten seconds of detector tremor moved the framing by {drift}"
    );
    assert!(
        largest_step < 1e-6,
        "largest single-frame step was {largest_step}"
    );
    assert!(
        smoother.rejected_as_noise > 90,
        "the deadband should have absorbed nearly every sample, rejected {}",
        smoother.rejected_as_noise
    );
}

/// The deadband must not become a dead zone: a real move, even a slow one,
/// still gets there.
#[test]
fn a_real_move_still_arrives_despite_the_deadband() {
    let (mut smoother, t) = settled(Damping::default(), (0.5, 0.5));
    // Three seconds of holding still at a genuinely different position.
    let (_, ended, _) = run(&mut smoother, t, 90, |_| Some((0.30, 0.5)));
    assert!(
        (ended.0 - 0.30).abs() < 0.01,
        "the framing should have arrived, sitting at {ended:?}"
    );
}

/// No single frame may lurch. This is the property that separates "tracking"
/// from "the frame keeps snapping", and it holds even though the target moved
/// a third of the frame in one sample.
#[test]
fn the_framing_never_lurches_however_far_the_target_jumps() {
    let damping = Damping {
        // Confirmation off, so the jump is accepted immediately and the speed
        // clamp is the only thing standing between it and a whip pan.
        confirm: 1,
        ..Damping::default()
    };
    let (mut smoother, t) = settled(damping, (0.5, 0.5));
    let (_, _, largest_step) = run(&mut smoother, t, 120, |_| Some((0.05, 0.95)));

    let ceiling = damping.max_speed * FRAME;
    assert!(
        largest_step <= ceiling + 1e-9,
        "a single frame moved {largest_step}, above the {ceiling} clamp"
    );
}

/// A one-sample false positive — a face-like pattern on a bookshelf, or
/// someone crossing behind — must not move the camera at all.
#[test]
fn a_single_stray_detection_is_not_believed() {
    let (mut smoother, t) = settled(Damping::default(), (0.5, 0.5));
    let before = smoother.anchor().expect("acquired");

    let (_, ended, _) = run(&mut smoother, t, 60, |frame| {
        // One sample, once, at the far corner.
        Some(if frame == 9 { (0.05, 0.05) } else { (0.5, 0.5) })
    });

    assert_eq!(
        ended, before,
        "a stray detection moved the framing to {ended:?}"
    );
    assert_eq!(smoother.rejected_as_jump, 1);
}

/// But a subject who genuinely moves across the frame and stays there is
/// followed — the confirmation is a delay, not a veto.
#[test]
fn a_confirmed_jump_is_followed() {
    let (mut smoother, t) = settled(Damping::default(), (0.5, 0.5));
    let (_, ended, _) = run(&mut smoother, t, 150, |_| Some((0.15, 0.5)));
    assert!(
        (ended.0 - 0.15).abs() < 0.02,
        "a sustained move should be followed, ended at {ended:?}"
    );
}

/// Turning your head or reaching off-camera loses the face for a moment. The
/// framing must not drift home and back.
///
/// "Must not drift home" rather than "must not move": a face lost mid-glide
/// leaves the framing partway to its last target, and *finishing* that move is
/// right — freezing halfway would be its own artefact. What must never happen
/// is travel back toward centre.
#[test]
fn losing_the_face_holds_the_framing_rather_than_recentring() {
    let (mut smoother, t) = settled(Damping::default(), (0.5, 0.5));
    let (t, parked, _) = run(&mut smoother, t, 90, |_| Some((0.28, 0.5)));
    assert!(
        parked.0 < 0.29,
        "should have followed the face to 0.28, at {parked:?}"
    );

    // Two seconds with nothing found at all.
    let (_, after, _) = run(&mut smoother, t, 60, |_| None);
    assert!(
        after.0 <= parked.0 + 1e-9,
        "the framing drifted back toward centre, {parked:?} -> {after:?}"
    );
    assert!(
        (after.0 - 0.28).abs() < 1e-3,
        "it should have settled on the last known face, at {after:?}"
    );
}

/// Opting in to recentring does recentre — and only after the configured wait.
#[test]
fn recentring_is_available_for_anyone_who_wants_it() {
    let damping = Damping {
        recenter_after_s: Some(1.0),
        ..Damping::default()
    };
    let (mut smoother, t) = settled(damping, (0.5, 0.5));
    let (t, parked, _) = run(&mut smoother, t, 90, |_| Some((0.25, 0.5)));
    assert!((parked.0 - 0.25).abs() < 0.02, "parked at {parked:?}");

    // Half a second gone: still converging on the face, not on centre.
    let (t, held, _) = run(&mut smoother, t, 15, |_| None);
    assert!(held.0 <= parked.0 + 1e-9, "recentred early, at {held:?}");

    // Four seconds gone: home.
    let (_, home, _) = run(&mut smoother, t, 120, |_| None);
    assert!(
        (home.0 - 0.5).abs() < 0.01,
        "should be centred, at {home:?}"
    );
}

/// The glide is defined in seconds, so the same configuration produces the same
/// motion at any capture rate. A fixed-fraction smoother fails this.
#[test]
fn the_glide_is_the_same_curve_at_any_frame_rate() {
    let damping = Damping {
        deadband: 0.0,
        max_speed: f64::MAX,
        ..Damping::default()
    };
    let travel = |step: f64, steps: usize| {
        let mut smoother = Smoother::new(damping);
        let mut t = 0.0;
        smoother.advance(t);
        smoother.observe((0.5, 0.5), t);
        // Inside `jump`, so this is about the glide and not about confirmation.
        smoother.observe((0.25, 0.5), t);
        for _ in 0..steps {
            t += step;
            smoother.advance(t);
        }
        smoother.anchor().expect("acquired").0
    };
    // One second of glide, walked at 30fps and at 120.
    let slow = travel(1.0 / 30.0, 30);
    let fast = travel(1.0 / 120.0, 120);
    assert!(
        (slow - fast).abs() < 1e-6,
        "30fps landed at {slow}, 120fps at {fast}"
    );
    // And it is most of the way there after one time constant plus a bit:
    // 0.25 + 0.25 * exp(-1 / 0.6) = 0.297.
    assert!(
        (slow - 0.297).abs() < 0.005,
        "a 0.6s constant should be 81% of the way in 1s, got {slow}"
    );
}

/// Before any face is found the smoother reports nothing, which is what lets
/// the framing fall back to exactly its untracked behaviour rather than to a
/// guess that happens to look the same.
#[test]
fn nothing_is_reported_until_a_face_is_actually_found() {
    let mut smoother = Smoother::new(Damping::default());
    for frame in 0..30 {
        let t = frame as f64 * FRAME;
        smoother.lost(t);
        assert_eq!(smoother.advance(t), None);
    }
    smoother.observe((0.4, 0.4), 1.0);
    assert_eq!(smoother.anchor(), Some((0.4, 0.4)), "the first face snaps");
}

/// A stalled capture queue delivers a frame with a much later timestamp. That
/// must cost a bounded amount of motion, not teleport the framing.
#[test]
fn a_stalled_queue_does_not_teleport_the_framing() {
    let damping = Damping {
        deadband: 0.0,
        ..Damping::default()
    };
    let (mut smoother, t) = settled(damping, (0.5, 0.5));
    smoother.observe((0.05, 0.05), t);

    let before = smoother.anchor().expect("acquired");
    // Ten seconds later, in one step.
    let after = smoother.advance(t + 10.0).expect("still tracking");
    let step = ((after.0 - before.0).powi(2) + (after.1 - before.1).powi(2)).sqrt();
    assert!(
        step <= damping.max_speed * 0.5 + 1e-9,
        "a stalled queue moved the framing {step} in one frame"
    );
}

/// Nonsense in, last good framing out. A NaN reaching a crop rect is a black
/// frame, so it stops here.
#[test]
fn a_nonsense_observation_is_ignored_rather_than_propagated() {
    let (mut smoother, t) = settled(Damping::default(), (0.4, 0.4));
    let before = smoother.anchor();
    smoother.observe((f64::NAN, 0.5), t);
    smoother.observe((0.5, f64::INFINITY), t);
    assert_eq!(smoother.anchor(), before);
    assert!(smoother.advance(t + FRAME).expect("tracking").0.is_finite());
}
