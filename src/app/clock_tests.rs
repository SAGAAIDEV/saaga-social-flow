//! Tests for the take bookkeeping: which chapter a second of speech belongs to,
//! and what happens to it when the take is retaken or the mic is swapped.
//!
//! Wall clock is not asserted on — it comes from `Instant::now()` and a test
//! runs in microseconds. What matters here is the speech accounting, which is
//! all differences taken off the meter.

use std::time::Duration;

use super::*;

fn snapshot(voiced_secs: f64, buffers: u64) -> Snapshot {
    Snapshot {
        level_dbfs: -20.0,
        peak_dbfs: -8.0,
        clipped: false,
        floor_dbfs: -52.0,
        speaking: true,
        voiced: Duration::from_secs_f64(voiced_secs),
        buffers,
        readable: true,
    }
}

#[test]
fn speech_lands_in_the_chapter_it_was_spoken_in() {
    let mut clock = RecordClock::default();
    // The meter runs before recording does — that time belongs to nobody.
    clock.tick(Some(snapshot(4.0, 1)));

    clock.open(1);
    clock.tick(Some(snapshot(14.0, 2)));
    let first = clock.open(2).expect("chapter 1 banked");
    assert_eq!(first.number, 1);
    assert_eq!(first.voiced, Duration::from_secs(10));

    clock.tick(Some(snapshot(20.0, 3)));
    let second = clock.close().expect("chapter 2 banked");
    assert_eq!(second.number, 2);
    assert_eq!(second.voiced, Duration::from_secs(6));

    // Both chapters, in the headline.
    assert!(
        clock.readout().headline.contains("~0:16"),
        "{}",
        clock.readout().headline
    );
}

/// Speech over a figure is the aside's, not the chapter's — see
/// `capture::pause` — so the meter's total over a break is left out.
#[test]
fn speech_during_a_break_is_not_the_chapters() {
    let mut clock = RecordClock::default();
    clock.open(1);
    clock.tick(Some(snapshot(10.0, 1)));
    clock.pause();
    assert!(clock.is_paused());
    assert!(
        !clock.readout().recording,
        "a paused chapter reads as recording"
    );
    assert!(
        clock.readout().detail.contains("on a break"),
        "{}",
        clock.readout().detail
    );
    // Four seconds spoken over the figure.
    clock.tick(Some(snapshot(14.0, 2)));
    clock.resume();
    assert!(!clock.is_paused());
    clock.tick(Some(snapshot(16.0, 3)));

    let closed = clock.close().expect("banked");
    assert_eq!(closed.voiced, Duration::from_secs(2));
}

/// The chapter's clock holds through a break, so the offset a figure records
/// is where the moment sits in the *file* — the only place it can be looked up.
#[test]
fn a_break_takes_no_time_off_the_chapter() {
    let mut clock = RecordClock::default();
    clock.open(2);
    clock.backdate(Duration::from_secs(5));
    let before = clock.position().expect("open").1;
    assert!((before - 5.0).abs() < 0.2, "{before}");

    clock.pause();
    clock.backdate(Duration::from_secs(3));
    let during = clock.position().expect("open").1;
    assert!(
        (during - 5.0).abs() < 0.2,
        "the clock ran during the break: {during}"
    );

    clock.resume();
    let after = clock.position().expect("open").1;
    assert!(
        (after - 5.0).abs() < 0.2,
        "the break was added back: {after}"
    );

    let closed = clock.close().expect("banked");
    assert!(
        (closed.recorded.as_secs_f64() - 5.0).abs() < 0.2,
        "banked {:?}",
        closed.recorded
    );
}

#[test]
fn a_retaken_chapter_takes_its_time_with_it() {
    let mut clock = RecordClock::default();
    clock.open(3);
    clock.tick(Some(snapshot(30.0, 2)));
    clock.discard();
    clock.tick(Some(snapshot(35.0, 3)));

    let closed = clock.close().expect("banked");
    assert_eq!(closed.number, 3, "a retake keeps the chapter number");
    assert_eq!(
        closed.voiced,
        Duration::from_secs(5),
        "the discarded take was counted anyway"
    );
}

#[test]
fn nothing_accumulates_between_takes() {
    let mut clock = RecordClock::default();
    clock.open(1);
    clock.tick(Some(snapshot(10.0, 2)));
    clock.close();

    clock.tick(Some(snapshot(45.0, 3)));
    clock.open(2);
    clock.tick(Some(snapshot(48.0, 4)));
    let closed = clock.close().expect("banked");
    assert_eq!(
        closed.voiced,
        Duration::from_secs(3),
        "talking between takes was counted into the next one"
    );
}

/// A device switch builds a new capture session with a new meter, whose counters
/// start at zero again.
#[test]
fn a_meter_that_restarts_does_not_run_the_clock_backwards() {
    let mut clock = RecordClock::default();
    clock.open(1);
    clock.tick(Some(snapshot(120.0, 9)));
    clock.close();

    clock.tick(None);
    clock.open(2);
    clock.tick(Some(snapshot(2.0, 1)));
    clock.tick(Some(snapshot(7.0, 2)));
    let closed = clock.close().expect("banked");
    assert_eq!(closed.voiced, Duration::from_secs(5));
}

#[test]
fn audio_it_cannot_read_is_shown_as_unknown_rather_than_as_zero() {
    let mut clock = RecordClock::default();
    clock.open(1);
    let mut snap = snapshot(10.0, 2);
    snap.readable = false;
    clock.tick(Some(snap));

    let readout = clock.readout();
    assert!(
        readout.headline.contains("~ --:--"),
        "headline was {}",
        readout.headline
    );
    assert!(
        !readout.headline.contains('%'),
        "a share of an unknown number is not a number: {}",
        readout.headline
    );
}

#[test]
fn idle_says_so_and_recording_says_which_chapter() {
    let mut clock = RecordClock::default();
    assert!(clock.readout().headline.contains("ready to record"));
    assert!(!clock.readout().recording);

    clock.open(4);
    let readout = clock.readout();
    assert!(readout.recording);
    assert!(
        readout.detail.contains("ch 04"),
        "detail was {}",
        readout.detail
    );
}

/// The mic-gone warning is what stops a whole take going into a dead interface.
#[test]
fn silence_from_the_device_is_called_out_while_recording() {
    let mut clock = RecordClock::default();
    clock.open(1);
    clock.tick(Some(snapshot(1.0, 1)));
    assert!(
        !clock.readout().detail.contains("no mic input"),
        "a mic that just delivered was called dead"
    );

    // Two seconds on, with the buffer count stuck where it was.
    clock.backdate(Duration::from_secs(3));
    clock.tick(Some(snapshot(1.0, 1)));
    assert!(
        clock.readout().detail.contains("no mic input"),
        "detail was {}",
        clock.readout().detail
    );

    // And it clears as soon as buffers arrive again.
    clock.tick(Some(snapshot(1.0, 2)));
    assert!(!clock.readout().detail.contains("no mic input"));
}

#[test]
fn the_clock_reads_in_minutes_until_it_reads_in_hours() {
    assert_eq!(clock(Duration::from_secs(9)), "0:09");
    assert_eq!(clock(Duration::from_secs(72)), "1:12");
    assert_eq!(clock(Duration::from_secs(3599)), "59:59");
    assert_eq!(clock(Duration::from_secs(3600)), "1:00:00");
    assert_eq!(clock(Duration::from_secs(3661)), "1:01:01");
}

#[test]
fn repainting_waits_for_something_to_change() {
    let mut clock = RecordClock::default();
    assert!(clock.take_paint().is_some(), "the first readout must paint");
    assert!(
        clock.take_paint().is_none(),
        "nothing changed and no time passed, so nothing should repaint"
    );
}

/// The bar draws peak, and peaks arrive faster than paints.
///
/// Audio lands every ~20ms while `take_paint` rate-limits to 100ms and skips
/// unchanged readouts entirely, so four ticks in five never reach the screen.
/// Holding the maximum across them is what makes a plosive visible; taking the
/// latest tick's value would show whichever 20ms the paint happened to land on.
#[test]
fn the_loudest_peak_between_paints_is_the_one_shown() {
    let mut clock = RecordClock::default();
    clock.open(1);

    let mut loud = snapshot(1.0, 1);
    loud.peak_dbfs = -6.0;
    clock.tick(Some(loud));

    let mut quiet = snapshot(1.0, 2);
    quiet.peak_dbfs = -48.0;
    clock.tick(Some(quiet));

    let readout = clock.readout();
    assert_eq!(
        readout.peak_dbfs, -6.0,
        "the transient survived the quiet tick"
    );
    assert!(readout.detail.contains("-6 dB pk"), "{}", readout.detail);
    // RMS is the latest, not the maximum: it is a loudness reading, not a catch.
    assert_eq!(readout.level_dbfs, -20.0);
}

/// Clipping latches, because the samples that clipped are gone by the time
/// anyone reads the line.
#[test]
fn a_clip_is_reported_and_stays_reported() {
    let mut clock = RecordClock::default();
    clock.open(1);

    let mut clipped = snapshot(1.0, 1);
    clipped.clipped = true;
    clock.tick(Some(clipped));
    assert!(clock.readout().detail.contains("CLIPPED"));
}

/// While idle the line is about setting gain, so it carries the same peak and
/// the room's floor rather than a chapter's clock.
#[test]
fn the_idle_line_shows_the_peak_and_the_room() {
    let mut clock = RecordClock::default();
    let mut snap = snapshot(0.0, 1);
    snap.peak_dbfs = -12.0;
    clock.tick(Some(snap));

    let detail = clock.readout().detail;
    assert!(detail.contains("listening"), "{detail}");
    assert!(detail.contains("-12 dB pk"), "{detail}");
    assert!(detail.contains("room -52 dB"), "{detail}");
}
