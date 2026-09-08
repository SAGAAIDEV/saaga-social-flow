//! How long you have been recording, and roughly how much of it was talking.
//!
//! Wall clock is easy — a chapter opens, an `Instant` starts. The second figure
//! comes from [`crate::capture::level`], which counts speech on the audio queue
//! into a monotonic total. This scopes that total to the take: a baseline when a
//! chapter opens, banked when it closes, dropped when it is retaken.
//!
//! Deltas rather than absolutes, deliberately: a device switch builds a whole
//! new capture session with a whole new meter whose counters restart at zero, and
//! a clock that read absolutes would jump backwards by however long the session
//! had run.

use std::time::{Duration, Instant};

use crate::capture::level::{Snapshot, METER_FLOOR_DBFS, SILENT_DBFS};

/// Nothing arriving for this long while a chapter is open means the mic is gone.
///
/// Worth saying out loud on screen: an unplugged interface or a device that
/// stopped delivering looks exactly like a silent room, and the difference is a
/// take you have to record again.
const MIC_SILENT_AFTER: Duration = Duration::from_secs(2);

/// How often the labels are allowed to repaint. They show whole seconds and a
/// meter, and the event loop ticks at ~60Hz.
const PAINT_EVERY: Duration = Duration::from_millis(100);

/// The chapter currently being recorded.
struct Open {
    number: u32,
    started: Instant,
    voiced: Duration,
    /// Breaks already taken in this chapter, summed — time the files do not
    /// contain, see `capture::pause`.
    paused: Duration,
    /// When the current break began, while one is on.
    paused_since: Option<Instant>,
}

impl Open {
    /// How much of this chapter is in its files: wall clock less every break.
    fn recorded(&self) -> Duration {
        let on_break = self
            .paused_since
            .map(|since| since.elapsed())
            .unwrap_or_default();
        self.started
            .elapsed()
            .saturating_sub(self.paused)
            .saturating_sub(on_break)
    }

    /// How long the current break has run, while one is on.
    fn on_break(&self) -> Option<Duration> {
        self.paused_since.map(|since| since.elapsed())
    }
}

/// What has been recorded and kept.
#[derive(Default, Clone, Copy)]
struct Banked {
    recorded: Duration,
    voiced: Duration,
}

/// The two strings and the level the Draft tab draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Readout {
    pub headline: String,
    pub detail: String,
    /// RMS dBFS — how loud it sounds, and what the label reads.
    pub level_dbfs: f32,
    /// Peak dBFS since the last paint, which is what the bar draws. Amplitude,
    /// not loudness: this is the number that moves when you speak and the one
    /// that reaches the top before anything distorts.
    pub peak_dbfs: f32,
    /// Latched once anything has hit full scale this session.
    pub clipped: bool,
    /// Whether a chapter is open, so the labels can be dimmed when it is not.
    pub recording: bool,
}

/// What a closed chapter ran to, for the line printed when it finishes.
pub struct Closed {
    pub number: u32,
    pub recorded: Duration,
    pub voiced: Duration,
}

pub struct RecordClock {
    open: Option<Open>,
    banked: Banked,
    /// The meter's speech total as of the last tick, so the next one can take a
    /// difference.
    last_voiced: Duration,
    /// Set when the meter being read is not the one the baseline came from — a
    /// device switch builds a new capture session with a new meter whose counters
    /// start at zero. The next tick re-baselines instead of taking a difference,
    /// because a new meter's total was measured before this clock saw any of it.
    resync: bool,
    last_buffers: u64,
    /// When a buffer last arrived, as seen from here. `None` before the first.
    heard_at: Option<Instant>,
    level_dbfs: f32,
    /// The noise floor the gate is working against, shown while idle: it is the
    /// number that explains a speech clock which will not move.
    floor_dbfs: f32,
    /// Peak since the last paint, and whether anything has clipped. Held here so
    /// a paint skipped by the dedupe does not drop a transient on the floor.
    peak_dbfs: f32,
    clipped: bool,
    /// Whether the gate is open right now — the speech clock's own state, shown
    /// so it is visible that it tracks talking rather than noise.
    speaking: bool,
    /// False once the meter meets audio in a layout it cannot read: the speech
    /// figure would be an undercount, so it is shown as unknown instead.
    readable: bool,
    /// What the labels currently show, so a 60Hz tick does not repaint a label
    /// to the same text sixty times a second.
    painted: Option<Readout>,
    painted_at: Option<Instant>,
}

impl Default for RecordClock {
    fn default() -> Self {
        RecordClock {
            open: None,
            banked: Banked::default(),
            last_voiced: Duration::ZERO,
            resync: true,
            last_buffers: 0,
            heard_at: None,
            level_dbfs: SILENT_DBFS,
            floor_dbfs: SILENT_DBFS,
            peak_dbfs: SILENT_DBFS,
            clipped: false,
            speaking: false,
            readable: true,
            painted: None,
            painted_at: None,
        }
    }
}

impl RecordClock {
    /// A chapter just opened. Banks whatever it displaced.
    pub fn open(&mut self, number: u32) -> Option<Closed> {
        let closed = self.bank();
        self.open = Some(Open {
            number,
            started: Instant::now(),
            voiced: Duration::ZERO,
            paused: Duration::ZERO,
            paused_since: None,
        });
        closed
    }

    /// The open chapter is being retaken: its time goes nowhere, and the same
    /// chapter number starts again from zero.
    pub fn discard(&mut self) {
        if let Some(open) = self.open.as_mut() {
            open.started = Instant::now();
            open.voiced = Duration::ZERO;
            open.paused = Duration::ZERO;
            open.paused_since = None;
        }
    }

    /// A break began: the chapter's clock holds, and speech from here is the
    /// author explaining a figure, not the take's.
    pub fn pause(&mut self) {
        if let Some(open) = self.open.as_mut() {
            if open.paused_since.is_none() {
                open.paused_since = Some(Instant::now());
            }
        }
    }

    /// The break ended and the take picked back up.
    pub fn resume(&mut self) {
        if let Some(open) = self.open.as_mut() {
            if let Some(since) = open.paused_since.take() {
                open.paused += since.elapsed();
            }
        }
    }

    pub fn is_paused(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.paused_since.is_some())
    }

    /// Recording stopped. Banks the open chapter and reports what it ran to.
    pub fn close(&mut self) -> Option<Closed> {
        self.bank()
    }

    fn bank(&mut self) -> Option<Closed> {
        let open = self.open.take()?;
        let recorded = open.recorded();
        self.banked.recorded += recorded;
        self.banked.voiced += open.voiced;
        Some(Closed {
            number: open.number,
            recorded,
            voiced: open.voiced,
        })
    }

    /// Fold one reading of the meter into the open chapter. `None` when there is
    /// no capture session at all — between device switches, or before startup
    /// finished.
    pub fn tick(&mut self, snapshot: Option<Snapshot>) {
        let Some(snap) = snapshot else {
            self.level_dbfs = SILENT_DBFS;
            self.speaking = false;
            self.resync = true;
            return;
        };
        self.level_dbfs = snap.level_dbfs;
        self.floor_dbfs = snap.floor_dbfs;
        // Held at the maximum rather than overwritten. `take_paint` drops a tick
        // whose readout is unchanged and rate-limits the rest, so a peak that
        // arrived on a skipped tick would otherwise never reach the bar.
        self.peak_dbfs = self.peak_dbfs.max(snap.peak_dbfs);
        self.clipped = snap.clipped;
        self.speaking = snap.speaking;
        self.readable = snap.readable;

        if snap.buffers != self.last_buffers {
            self.last_buffers = snap.buffers;
            self.heard_at = Some(Instant::now());
        }
        // A total below the last one is a fresh meter behind a fresh capture
        // session, not time going backwards.
        if snap.voiced < self.last_voiced {
            self.resync = true;
        }
        let voiced = if std::mem::take(&mut self.resync) {
            Duration::ZERO
        } else {
            snap.voiced - self.last_voiced
        };
        self.last_voiced = snap.voiced;
        // Not while on a break: that speech is the aside's, and the aside is
        // not in the chapter.
        if let Some(open) = self
            .open
            .as_mut()
            .filter(|open| open.paused_since.is_none())
        {
            open.voiced += voiced;
        }
    }

    /// True while a chapter is open but nothing is arriving from the mic.
    ///
    /// A chapter younger than the grace period never warns: at the moment one
    /// opens there may be no reading yet, and a warning that flashes on every
    /// New Chapter press is a warning nobody reads.
    fn mic_gone(&self) -> bool {
        let Some(open) = self.open.as_ref() else {
            return false;
        };
        if open.started.elapsed() <= MIC_SILENT_AFTER {
            return false;
        }
        self.heard_at
            .is_none_or(|at| at.elapsed() > MIC_SILENT_AFTER)
    }

    /// Moves the open chapter and the last-heard reading `by` into the past, so
    /// a test can reach a state that otherwise takes seconds of waiting.
    #[cfg(test)]
    fn backdate(&mut self, by: Duration) {
        let back = |at: Instant| at.checked_sub(by).unwrap_or(at);
        if let Some(open) = self.open.as_mut() {
            open.started = back(open.started);
            // A break that is on grows by the same amount, so backdating during
            // one is how a test makes a break of a known length.
            open.paused_since = open.paused_since.map(back);
        }
        self.heard_at = self.heard_at.map(back);
    }

    /// Which chapter is open and how many seconds into it we are.
    ///
    /// `None` when nothing is recording. The one number here that is written to
    /// disk rather than drawn — [`crate::figure`] stores it so a screenshot can
    /// be lined up against the chapter transcript later — which is why it is a
    /// pair of numbers and not one of [`Readout`]'s formatted strings.
    pub fn position(&self) -> Option<(u32, f64)> {
        self.open
            .as_ref()
            .map(|open| (open.number, open.recorded().as_secs_f64()))
    }

    pub fn readout(&self) -> Readout {
        // Paused is not recording: the numbers hold and the line dims, so it is
        // visible at a glance that the take is not rolling.
        let recording = self
            .open
            .as_ref()
            .is_some_and(|open| open.paused_since.is_none());
        let chapter = self
            .open
            .as_ref()
            .map(|open| (open.number, open.recorded(), open.voiced));
        let recorded = self.banked.recorded + chapter.map(|c| c.1).unwrap_or_default();
        let voiced = self.banked.voiced + chapter.map(|c| c.2).unwrap_or_default();

        let headline = if recorded.is_zero() {
            "⏸ 0:00 — ready to record".to_string()
        } else {
            format!(
                "{} {}   {} speech{}",
                if recording { "⏺" } else { "⏸" },
                clock(recorded),
                speech(voiced, self.readable),
                share(voiced, recorded, self.readable),
            )
        };

        let on_break = self.open.as_ref().and_then(|open| {
            open.on_break()
                .map(|since| (open.number, open.recorded(), since))
        });
        let detail = if self.mic_gone() {
            "⚠ no mic input — check the microphone before this take goes further".to_string()
        } else if let Some((number, recorded, since)) = on_break {
            // The chapter's clock holds while the author explains a figure; the
            // break's own runs, so it is visible that recording has stopped and
            // how long ago.
            format!(
                "ch {number:02}  {} · on a break {}   ⌃⇧S picks the take back up",
                clock(recorded),
                clock(since),
            )
        } else if let Some((number, elapsed, spoken)) = chapter {
            format!(
                "ch {number:02}  {} · {}      {}{}{}",
                clock(elapsed),
                speech(spoken, self.readable),
                mic(self.level_dbfs, self.peak_dbfs),
                if self.speaking { "  ●" } else { "" },
                clip(self.clipped),
            )
        } else {
            // Idle is when the gain gets set, so this is where the floor the gate
            // measures against is worth showing.
            format!(
                "listening      {}  (room {}){}",
                mic(self.level_dbfs, self.peak_dbfs),
                level(self.floor_dbfs),
                clip(self.clipped),
            )
        };

        Readout {
            headline,
            detail,
            level_dbfs: self.level_dbfs,
            peak_dbfs: self.peak_dbfs,
            clipped: self.clipped,
            recording,
        }
    }

    /// The readout, but only when it is worth painting: something changed, and
    /// the last paint was long enough ago.
    pub fn take_paint(&mut self) -> Option<Readout> {
        let now = Instant::now();
        if self
            .painted_at
            .is_some_and(|at| now.duration_since(at) < PAINT_EVERY)
        {
            return None;
        }
        let readout = self.readout();
        if self.painted.as_ref() == Some(&readout) {
            return None;
        }
        self.painted_at = Some(now);
        self.painted = Some(readout.clone());
        // The peak has been drawn, so the next window starts from silence. Reset
        // here rather than in `tick` — a tick whose paint was skipped has not
        // shown its peak to anyone yet.
        self.peak_dbfs = SILENT_DBFS;
        Some(readout)
    }
}

/// `m:ss`, or `h:mm:ss` once a session runs past the hour.
pub fn clock(duration: Duration) -> String {
    let secs = duration.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The speech figure, always marked as the estimate it is — or as unknown, when
/// the meter met audio it could not read.
fn speech(voiced: Duration, readable: bool) -> String {
    if readable {
        format!("~{}", clock(voiced))
    } else {
        "~ --:--".to_string()
    }
}

fn share(voiced: Duration, recorded: Duration, readable: bool) -> String {
    if !readable || recorded.is_zero() {
        return String::new();
    }
    let percent = (voiced.as_secs_f64() / recorded.as_secs_f64() * 100.0).round();
    format!("  ({percent:.0}%)")
}

/// Both mic numbers: the peak drives the bar, so it is named first, with the
/// RMS after it as the quieter loudness figure people actually recognise.
fn mic(rms_dbfs: f32, peak_dbfs: f32) -> String {
    format!("{} pk · {} rms", level(peak_dbfs), level(rms_dbfs))
}

/// Sticky, and deliberately loud in the line: by the time anyone looks, the
/// samples that clipped are minutes back in the file.
fn clip(clipped: bool) -> &'static str {
    if clipped {
        "   ⚠ CLIPPED"
    } else {
        ""
    }
}

fn level(dbfs: f32) -> String {
    if dbfs <= METER_FLOOR_DBFS {
        "-∞ dB".to_string()
    } else {
        format!("{dbfs:.0} dB")
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
