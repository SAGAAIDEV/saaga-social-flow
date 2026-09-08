//! Live input level, and how much of a take was speech rather than silence.
//!
//! The Draft tab has two questions to answer while recording: how long has this
//! been running, and roughly how long is it once the silence comes out. The
//! first is a clock. The second has to listen — so every captured audio buffer,
//! which already passes through [`crate::capture::av_delegate`], gets one RMS
//! reading (~20ms of audio) fed through a gate, into counters the main thread
//! polls on its tick.
//!
//! The speech figure is a ballpark and is labelled as one everywhere it is
//! shown. It is measured on the same criterion a silence cutter uses — level
//! against a tracked noise floor, with a hangover long enough to keep the gaps
//! between words inside one phrase — so the two should land close. The cut is
//! the authority; this is the number that stops you recording 40 minutes for a
//! 12-minute video without knowing it.
//!
//! ## Layout of this module
//!
//! | piece | responsibility |
//! |---|---|
//! | [`rms_dbfs`] / [`Layout`] | reading one channel of PCM out of a buffer's bytes, whatever integer or float layout the device chose |
//! | [`Gate`] | noise floor, hysteresis, attack and hangover — the whole "is this speech" decision, in pure arithmetic |
//! | [`SpeechMeter`] | the shared handle: what the capture queue writes and the main thread reads |

use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use objc2_core_audio_types::{
    kAudioFormatFlagIsBigEndian, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved,
    kAudioFormatFlagIsSignedInteger, AudioStreamBasicDescription,
};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMSampleBuffer};

/// What a buffer carrying no signal at all reports, rather than `-inf`.
///
/// Digital silence is a real input — a muted interface sends it — and an
/// infinity would poison the noise floor follower and the label alike.
pub const SILENT_DBFS: f32 = -120.0;

/// The range the input meter draws, and the clamp on the tracked noise floor.
pub const METER_FLOOR_DBFS: f32 = -60.0;

/// Where a peak counts as clipped.
///
/// Not 0.0: a converter that has run out of headroom returns its maximum code,
/// which lands a hair under full scale after normalising, and a threshold of
/// exactly zero would never fire.
pub const CLIP_DBFS: f32 = -0.1;

/// How each PCM sample is stored, and how far apart consecutive frames sit.
///
/// Resolved from the stream's `AudioStreamBasicDescription` once per buffer.
/// Only channel 0 is read: this measures whether someone is talking, and the
/// mic that matters here is mono.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub sample: Sample,
    /// Bytes from one frame's channel 0 to the next.
    pub stride: usize,
    pub big_endian: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    F32,
    I16,
    /// 24-bit packed, three bytes per sample. Not hypothetical: the XLR dock
    /// this rig records through sends exactly this until it renegotiates.
    I24,
    I32,
}

impl Sample {
    fn width(self) -> usize {
        match self {
            Sample::F32 | Sample::I32 => 4,
            Sample::I24 => 3,
            Sample::I16 => 2,
        }
    }

    /// One sample's bytes as a float in -1.0..=1.0.
    fn read(self, bytes: &[u8], big_endian: bool) -> f32 {
        macro_rules! fixed {
            ($n:literal) => {{
                let mut raw = [0u8; $n];
                raw.copy_from_slice(&bytes[..$n]);
                if big_endian {
                    raw.reverse();
                }
                raw
            }};
        }
        match self {
            Sample::F32 => f32::from_le_bytes(fixed!(4)),
            Sample::I32 => i32::from_le_bytes(fixed!(4)) as f32 / i32::MAX as f32,
            Sample::I16 => i16::from_le_bytes(fixed!(2)) as f32 / i16::MAX as f32,
            Sample::I24 => {
                let raw = fixed!(3);
                // Sign-extend by parking the three bytes in the *top* of an i32
                // and shifting back down: the arithmetic shift carries the sign
                // bit, which a hand-rolled `if raw[2] & 0x80` would have to
                // reproduce by hand.
                let packed = i32::from_le_bytes([0, raw[0], raw[1], raw[2]]);
                (packed >> 8) as f32 / 0x7f_ffff as f32
            }
        }
    }
}

/// The layout of a stream this meter can read, or `None` for one it cannot.
///
/// Non-interleaved is the notable rejection: its channels live in separate
/// blocks, so the stride arithmetic below would read across a channel boundary
/// and report a level that is not the level of anything. Reporting nothing is
/// the honest answer — the caller renders the speech figure as unknown, and the
/// wall clock carries on unaffected.
pub fn layout_of(asbd: &AudioStreamBasicDescription) -> Option<Layout> {
    if asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
        return None;
    }
    let float = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0;
    let integer = asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0;
    let sample = match (float, integer, asbd.mBitsPerChannel) {
        (true, _, 32) => Sample::F32,
        (_, true, 16) => Sample::I16,
        (_, true, 24) => Sample::I24,
        (_, true, 32) => Sample::I32,
        _ => return None,
    };
    let stride = asbd.mBytesPerFrame as usize;
    if stride < sample.width() {
        return None;
    }
    Some(Layout {
        sample,
        stride,
        big_endian: asbd.mFormatFlags & kAudioFormatFlagIsBigEndian != 0,
    })
}

/// Channel 0's RMS across `bytes`, in dBFS.
///
/// Reads as many frames as the slice holds, so a short final buffer measures
/// what is there rather than running off the end.
/// Loudness alone, for the tests that predate [`levels_dbfs`] and for callers
/// that genuinely do not care about amplitude.
#[cfg(test)]
pub fn rms_dbfs(bytes: &[u8], layout: Layout) -> f32 {
    levels_dbfs(bytes, layout).rms_dbfs
}

/// What one buffer measured.
///
/// Both numbers, because they answer different questions and the meter needs
/// both. RMS is loudness — it tracks how a take will *sound* and it is what the
/// speech gate runs on. Peak is amplitude — the largest excursion in the buffer,
/// which is the only thing that says whether the input is about to clip. A
/// speaking voice sits 10–20 dB below its own peaks, so an RMS-only meter reads
/// comfortable while the converter is already square-waving the loud consonants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
}

/// RMS and peak from a single pass over the buffer.
///
/// One pass rather than two: this runs on the audio capture queue for every
/// buffer that arrives, and reading the same few thousand samples twice to get a
/// number that costs one `max` is not worth the cache traffic.
pub fn levels_dbfs(bytes: &[u8], layout: Layout) -> Levels {
    let width = layout.sample.width();
    let mut sum = 0.0f64;
    let mut peak = 0.0f64;
    let mut frames = 0u64;
    let mut at = 0usize;
    while at + width <= bytes.len() {
        let value = layout.sample.read(&bytes[at..], layout.big_endian) as f64;
        sum += value * value;
        // `abs`, because a waveform clips symmetrically and the negative half is
        // just as loud.
        peak = peak.max(value.abs());
        frames += 1;
        at += layout.stride;
    }
    if frames == 0 {
        return Levels {
            rms_dbfs: SILENT_DBFS,
            peak_dbfs: SILENT_DBFS,
        };
    }
    Levels {
        rms_dbfs: to_dbfs((sum / frames as f64).sqrt()),
        peak_dbfs: to_dbfs(peak),
    }
}

fn to_dbfs(amplitude: f64) -> f32 {
    if amplitude <= 0.0 {
        return SILENT_DBFS;
    }
    (20.0 * amplitude.log10()).max(SILENT_DBFS as f64) as f32
}

/// The thresholds the gate runs on. From `config.vad`, so the same numbers can
/// be handed to whatever cuts the silence later.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateConfig {
    /// The quietest level that can ever count as speech.
    pub speech_dbfs: f32,
    /// How far above the tracked noise floor the gate opens.
    pub floor_margin_db: f32,
    /// How far below the opening level it closes.
    pub release_db: f32,
    /// Speech has to hold above the threshold this long before it counts, so a
    /// key press or a chair creak never opens the gate.
    pub attack: Duration,
    /// A quiet stretch shorter than this is a gap between words, not a pause.
    pub hangover: Duration,
}

impl From<crate::config::Vad> for GateConfig {
    fn from(vad: crate::config::Vad) -> Self {
        GateConfig {
            speech_dbfs: vad.speech_dbfs,
            floor_margin_db: vad.floor_margin_db,
            release_db: vad.release_db,
            attack: Duration::from_millis(vad.attack_ms as u64),
            hangover: Duration::from_millis(vad.hangover_ms as u64),
        }
    }
}

/// The speech decision: a noise floor that follows the room, a threshold above
/// it with hysteresis, an attack, and a hangover.
///
/// Pure arithmetic on (level, duration) pairs — no Apple types, no clock, no
/// atomics — which is what makes the whole decision testable without a
/// microphone. See `level_tests.rs`.
#[derive(Debug)]
pub struct Gate {
    cfg: GateConfig,
    floor: f32,
    open: bool,
    /// Time held above the threshold while closed, toward `cfg.attack`.
    rising: Duration,
    /// Time below the threshold while open, toward `cfg.hangover`. Credited as
    /// speech if talking resumes, discarded if it does not — a bridged gap is
    /// speech, a trailing pause is not.
    pending: Duration,
    /// How long the gate has been open without the level once dipping. See
    /// [`Gate::STEADY_LIMIT`].
    sustained: Duration,
}

impl Gate {
    /// How fast the noise floor chases a level below it. Fast, so plugging in a
    /// quieter mic re-arms the gate in a second rather than a minute.
    const FALL_PER_SEC: f32 = 40.0;
    /// How fast it creeps back up while the gate is closed. Slow, and only ever
    /// while closed: a floor that rose during speech would chase the threshold
    /// up past the voice holding it open and gate mid-sentence.
    const RISE_PER_SEC: f32 = 1.0;
    /// How long the gate may stay open on a level that never dips before the
    /// meter decides it is noise and not a voice.
    ///
    /// The escape hatch from the rule above. A noisy preamp hissing at -35 dBFS
    /// is above the absolute threshold, so it opens the gate, and while the gate
    /// is open the floor does not rise — which on its own means an entire
    /// session counts as speech and the estimate reads 100%. What separates the
    /// two is dynamics, not level: a voice dips below its own threshold between
    /// every few words, and hiss never does. So an unbroken stretch longer than
    /// any phrase is taken as the room, and the floor recalibrates onto it.
    const STEADY_LIMIT: Duration = Duration::from_secs(20);

    pub fn new(cfg: GateConfig) -> Gate {
        Gate {
            cfg,
            floor: cfg.speech_dbfs - cfg.floor_margin_db,
            open: false,
            rising: Duration::ZERO,
            pending: Duration::ZERO,
            sustained: Duration::ZERO,
        }
    }

    pub fn floor_dbfs(&self) -> f32 {
        self.floor
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The level the gate opens at right now.
    pub fn threshold(&self) -> f32 {
        self.cfg
            .speech_dbfs
            .max(self.floor + self.cfg.floor_margin_db)
    }

    /// Feed one buffer's level and duration; returns how much of it counts as
    /// speech.
    ///
    /// Usually zero or `secs`, but the two transitions credit in a lump: the
    /// buffer that completes the attack credits the whole run that earned it,
    /// and the buffer that resumes speech credits the gap it bridged.
    pub fn step(&mut self, dbfs: f32, secs: Duration) -> Duration {
        self.track_floor(dbfs, secs);
        let open_at = self.threshold();
        if self.open {
            if dbfs >= open_at - self.cfg.release_db {
                self.sustained += secs;
                if self.sustained >= Self::STEADY_LIMIT {
                    // Not a voice: recalibrate onto it as the new floor and stop
                    // counting it. Costs up to STEADY_LIMIT of overcount once,
                    // and nothing after that — the floor now sits on top of it.
                    self.floor = dbfs;
                    self.open = false;
                    self.sustained = Duration::ZERO;
                    self.pending = Duration::ZERO;
                    self.rising = Duration::ZERO;
                    return Duration::ZERO;
                }
                let bridged = std::mem::take(&mut self.pending);
                return bridged + secs;
            }
            self.sustained = Duration::ZERO;
            self.pending += secs;
            if self.pending >= self.cfg.hangover {
                // Long enough to be a real pause: the gate closes and the
                // pending stretch is dropped rather than padded onto the take.
                self.open = false;
                self.pending = Duration::ZERO;
            }
            Duration::ZERO
        } else {
            if dbfs < open_at {
                self.rising = Duration::ZERO;
                return Duration::ZERO;
            }
            self.rising += secs;
            if self.rising < self.cfg.attack {
                return Duration::ZERO;
            }
            self.open = true;
            self.pending = Duration::ZERO;
            self.sustained = Duration::ZERO;
            std::mem::take(&mut self.rising)
        }
    }

    fn track_floor(&mut self, dbfs: f32, secs: Duration) {
        let secs = secs.as_secs_f32();
        if dbfs < self.floor {
            self.floor = (self.floor - Self::FALL_PER_SEC * secs).max(dbfs);
        } else if !self.open {
            self.floor = (self.floor + Self::RISE_PER_SEC * secs).min(dbfs);
        }
        self.floor = self.floor.clamp(SILENT_DBFS, -20.0);
    }
}

/// What the main thread reads off the meter on its tick.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// The last buffer's RMS, for the meter and the label.
    pub level_dbfs: f32,
    /// The loudest sample seen since the *previous* snapshot, not merely in the
    /// last buffer.
    ///
    /// The main thread repaints at 10Hz while audio arrives every ~20ms, so four
    /// buffers in five are never looked at. Carrying the running maximum across
    /// the gap is what makes a transient — a plosive, a desk knock, the start of
    /// a laugh — visible at all; sampling the latest buffer would show whichever
    /// 20ms the repaint happened to land on.
    pub peak_dbfs: f32,
    /// Whether anything has hit full scale since the capture session started.
    ///
    /// Latched rather than momentary: a clip is a handful of samples, far too
    /// short to catch on a bar, and the reason to know is that the take is
    /// already damaged.
    pub clipped: bool,
    /// The noise floor the gate is currently working against.
    pub floor_dbfs: f32,
    pub speaking: bool,
    /// Speech since the capture session started. Monotonic — callers take
    /// differences to scope it to a chapter.
    pub voiced: Duration,
    pub buffers: u64,
    /// False once a buffer arrives in a layout this cannot read, which makes
    /// [`Snapshot::voiced`] an undercount rather than an estimate. Callers show
    /// the speech figure as unknown instead of showing a wrong one.
    pub readable: bool,
}

/// The meter itself: written from the audio capture queue, read from the main
/// thread.
///
/// The gate sits behind its own `Mutex`, deliberately *not* the writer's state
/// lock in `av_delegate` — metering a buffer must never make the thread that
/// appends it wait on the thread that appends video.
pub struct SpeechMeter {
    level: AtomicU32,
    floor: AtomicU32,
    speaking: AtomicBool,
    voiced_us: AtomicU64,
    /// Running maximum, swapped out by [`SpeechMeter::snapshot`]. Held as the
    /// raw bits of an `f32` so the capture queue never takes a lock to report a
    /// level.
    peak_since_read: AtomicU32,
    clipped: AtomicBool,
    buffers: AtomicU64,
    readable: AtomicBool,
    gate: Mutex<Gate>,
}

impl SpeechMeter {
    pub fn new(cfg: GateConfig) -> SpeechMeter {
        let gate = Gate::new(cfg);
        SpeechMeter {
            level: AtomicU32::new(SILENT_DBFS.to_bits()),
            floor: AtomicU32::new(gate.floor_dbfs().to_bits()),
            speaking: AtomicBool::new(false),
            voiced_us: AtomicU64::new(0),
            peak_since_read: AtomicU32::new(SILENT_DBFS.to_bits()),
            clipped: AtomicBool::new(false),
            buffers: AtomicU64::new(0),
            readable: AtomicBool::new(true),
            gate: Mutex::new(gate),
        }
    }

    /// A meter on the saved thresholds.
    pub fn from_config() -> SpeechMeter {
        SpeechMeter::new(crate::config::load().vad.into())
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            level_dbfs: f32::from_bits(self.level.load(Ordering::Relaxed)),
            floor_dbfs: f32::from_bits(self.floor.load(Ordering::Relaxed)),
            speaking: self.speaking.load(Ordering::Relaxed),
            voiced: Duration::from_micros(self.voiced_us.load(Ordering::Relaxed)),
            // Read *and* reset: the next snapshot asks about the window after
            // this one, so leaving the maximum in place would make one loud
            // moment pin the meter for the rest of the take.
            peak_dbfs: f32::from_bits(
                self.peak_since_read
                    .swap(SILENT_DBFS.to_bits(), Ordering::Relaxed),
            ),
            clipped: self.clipped.load(Ordering::Relaxed),
            buffers: self.buffers.load(Ordering::Relaxed),
            readable: self.readable.load(Ordering::Relaxed),
        }
    }

    /// Lifts the running peak to `dbfs` if it is louder.
    ///
    /// A compare-and-swap loop rather than a plain store: only this queue writes
    /// it, but [`SpeechMeter::snapshot`] resets it concurrently from the main
    /// thread, and a lost update here is a peak that never gets drawn.
    fn raise_peak(&self, dbfs: f32) {
        let mut current = self.peak_since_read.load(Ordering::Relaxed);
        while dbfs > f32::from_bits(current) {
            match self.peak_since_read.compare_exchange_weak(
                current,
                dbfs.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(seen) => current = seen,
            }
        }
    }

    /// Measure one captured audio buffer. Called on the audio capture queue,
    /// before any writer lock is taken, so the meter runs during warmup and
    /// between takes as well as while recording.
    pub fn observe(&self, sample_buffer: &CMSampleBuffer) {
        let Some(asbd) = asbd_of(sample_buffer) else {
            return;
        };
        if asbd.mSampleRate <= 0.0 {
            return;
        }
        let frames = unsafe { sample_buffer.num_samples() }.max(0) as f64;
        let secs = Duration::from_secs_f64(frames / asbd.mSampleRate);
        self.buffers.fetch_add(1, Ordering::Relaxed);

        let Some(layout) = layout_of(&asbd) else {
            self.readable.store(false, Ordering::Relaxed);
            return;
        };
        let Some(levels) = with_pcm(sample_buffer, |bytes| levels_dbfs(bytes, layout)) else {
            self.readable.store(false, Ordering::Relaxed);
            return;
        };
        let dbfs = levels.rms_dbfs;
        self.level.store(dbfs.to_bits(), Ordering::Relaxed);
        self.raise_peak(levels.peak_dbfs);
        if levels.peak_dbfs >= CLIP_DBFS {
            self.clipped.store(true, Ordering::Relaxed);
        }

        let Ok(mut gate) = self.gate.lock() else {
            return;
        };
        let voiced = gate.step(dbfs, secs);
        self.floor
            .store(gate.floor_dbfs().to_bits(), Ordering::Relaxed);
        self.speaking.store(gate.is_open(), Ordering::Relaxed);
        drop(gate);
        if !voiced.is_zero() {
            self.voiced_us
                .fetch_add(voiced.as_micros() as u64, Ordering::Relaxed);
        }
    }
}

/// The stream layout a buffer carries, from its format description.
pub fn asbd_of(sample_buffer: &CMSampleBuffer) -> Option<AudioStreamBasicDescription> {
    let format = unsafe { sample_buffer.format_description() }?;
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&format) };
    if asbd.is_null() {
        return None;
    }
    Some(unsafe { *asbd })
}

/// Runs `read` over the buffer's PCM bytes, or returns `None` when they are not
/// there to be read.
///
/// The pointer is only valid while the block buffer is alive, which is why this
/// is a closure rather than a slice returned to the caller. A buffer whose data
/// is not contiguous from offset 0 is refused rather than partially measured:
/// `length_at_offset` short of `total_length` means the rest lives in another
/// block, and reading past it would be reading someone else's memory.
fn with_pcm<T>(sample_buffer: &CMSampleBuffer, read: impl FnOnce(&[u8]) -> T) -> Option<T> {
    let block = unsafe { sample_buffer.data_buffer() }?;
    let mut at_offset: usize = 0;
    let mut total: usize = 0;
    let mut ptr: *mut c_char = std::ptr::null_mut();
    let status = unsafe { block.data_pointer(0, &mut at_offset, &mut total, &mut ptr) };
    if status != 0 || ptr.is_null() || at_offset == 0 || at_offset < total {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, at_offset) };
    Some(read(bytes))
}

#[cfg(test)]
#[path = "level_tests.rs"]
mod tests;
