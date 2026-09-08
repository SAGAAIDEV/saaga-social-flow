//! Tests for the parts of the meter that do not need a microphone: reading PCM
//! out of raw bytes, and the speech gate.
//!
//! Everything the estimate's accuracy depends on is in here. What is not is the
//! `CMSampleBuffer` plumbing in `observe` — a sample buffer cannot be
//! synthesized in a unit test, so that stays a thin adapter over these two
//! pieces and is verified live against the meter on screen.

use std::time::Duration;

use super::*;

const F32_MONO: Layout = Layout {
    sample: Sample::F32,
    stride: 4,
    big_endian: false,
};

fn float_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// A buffer's worth of tone at a given amplitude, as the gate sees it: one
/// level, one duration.
fn buffer_dbfs(amplitude: f32) -> f32 {
    let samples: Vec<f32> = (0..960)
        .map(|n| (n as f32 * 0.13).sin() * amplitude)
        .collect();
    rms_dbfs(&float_bytes(&samples), F32_MONO)
}

const BUFFER: Duration = Duration::from_millis(20);

fn test_gate() -> Gate {
    Gate::new(GateConfig::from(crate::config::Vad::default()))
}

/// Feeds `secs` of one level, a buffer at a time, and returns what counted.
fn feed(gate: &mut Gate, dbfs: f32, secs: f64) -> Duration {
    let buffers = (secs / BUFFER.as_secs_f64()).round() as u32;
    (0..buffers).map(|_| gate.step(dbfs, BUFFER)).sum()
}

#[test]
fn full_scale_is_zero_dbfs_and_silence_is_the_floor() {
    let full = rms_dbfs(&float_bytes(&[1.0, -1.0, 1.0, -1.0]), F32_MONO);
    assert!((full - 0.0).abs() < 0.01, "full scale square: {full} dBFS");

    let half = rms_dbfs(&float_bytes(&[0.5, -0.5, 0.5, -0.5]), F32_MONO);
    assert!((half + 6.02).abs() < 0.05, "half scale: {half} dBFS");

    let silent = rms_dbfs(&float_bytes(&[0.0; 64]), F32_MONO);
    assert_eq!(silent, SILENT_DBFS);
    assert_eq!(rms_dbfs(&[], F32_MONO), SILENT_DBFS);
}

/// Every layout a capture device here has been seen to deliver has to measure
/// the same signal at the same level — a meter that reads 24-bit as 30 dB quieter
/// than float would gate a whole session as silence.
#[test]
fn every_sample_layout_reads_the_same_level() {
    let reference = rms_dbfs(&float_bytes(&[0.5, -0.5, 0.5, -0.5]), F32_MONO);

    let i16_bytes: Vec<u8> = [16384i16, -16384, 16384, -16384]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let i16_level = rms_dbfs(
        &i16_bytes,
        Layout {
            sample: Sample::I16,
            stride: 2,
            big_endian: false,
        },
    );
    assert!((i16_level - reference).abs() < 0.05, "i16: {i16_level}");

    // 24-bit packed, three bytes per sample, half scale.
    let i24_bytes: Vec<u8> = [0x400000i32, -0x400000, 0x400000, -0x400000]
        .iter()
        .flat_map(|s| s.to_le_bytes()[..3].to_vec())
        .collect();
    let i24_level = rms_dbfs(
        &i24_bytes,
        Layout {
            sample: Sample::I24,
            stride: 3,
            big_endian: false,
        },
    );
    assert!((i24_level - reference).abs() < 0.05, "i24: {i24_level}");

    let i32_bytes: Vec<u8> = [0x4000_0000i32, -0x4000_0000]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let i32_level = rms_dbfs(
        &i32_bytes,
        Layout {
            sample: Sample::I32,
            stride: 4,
            big_endian: false,
        },
    );
    assert!((i32_level - reference).abs() < 0.05, "i32: {i32_level}");
}

/// Channel 0 only. A stereo buffer whose right channel is loud must not lift the
/// reading off a quiet left channel.
#[test]
fn stride_skips_the_other_channels() {
    let interleaved = float_bytes(&[0.5, 1.0, -0.5, -1.0, 0.5, 1.0, -0.5, -1.0]);
    let level = rms_dbfs(
        &interleaved,
        Layout {
            sample: Sample::F32,
            stride: 8,
            big_endian: false,
        },
    );
    assert!((level + 6.02).abs() < 0.05, "left channel: {level} dBFS");
}

#[test]
fn big_endian_is_read_the_right_way_round() {
    let swapped: Vec<u8> = [16384i16, -16384]
        .iter()
        .flat_map(|s| s.to_be_bytes())
        .collect();
    let level = rms_dbfs(
        &swapped,
        Layout {
            sample: Sample::I16,
            stride: 2,
            big_endian: true,
        },
    );
    assert!((level + 6.02).abs() < 0.05, "big endian i16: {level} dBFS");
}

/// Non-interleaved is refused rather than measured: its channels are in separate
/// blocks, so the stride arithmetic would read across the boundary.
#[test]
fn unreadable_layouts_are_refused() {
    let mut asbd = AudioStreamBasicDescription {
        mSampleRate: 48_000.0,
        mFormatID: 0,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsNonInterleaved,
        mBytesPerPacket: 8,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4,
        mChannelsPerFrame: 2,
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    assert!(layout_of(&asbd).is_none(), "non-interleaved was accepted");

    asbd.mFormatFlags = kAudioFormatFlagIsFloat;
    assert_eq!(layout_of(&asbd).map(|l| l.sample), Some(Sample::F32));

    // A frame narrower than one sample cannot be walked.
    asbd.mBytesPerFrame = 2;
    assert!(layout_of(&asbd).is_none(), "impossible stride was accepted");
}

#[test]
fn a_minute_of_room_noise_counts_as_nothing() {
    let mut gate = test_gate();
    let counted = feed(&mut gate, buffer_dbfs(0.002), 60.0);
    assert_eq!(counted, Duration::ZERO, "quiet room counted {counted:?}");
    assert!(!gate.is_open());
}

#[test]
fn a_second_of_speech_counts_about_a_second() {
    let mut gate = test_gate();
    feed(&mut gate, buffer_dbfs(0.002), 5.0);
    let counted = feed(&mut gate, buffer_dbfs(0.2), 1.0);
    assert!(
        (0.95..=1.02).contains(&counted.as_secs_f64()),
        "one second of speech counted {counted:?}"
    );
}

/// A click is above the threshold and is not speech. The attack window is what
/// tells them apart.
#[test]
fn a_click_does_not_open_the_gate() {
    let mut gate = test_gate();
    feed(&mut gate, buffer_dbfs(0.002), 5.0);
    let counted = feed(&mut gate, buffer_dbfs(0.9), 0.04);
    assert_eq!(counted, Duration::ZERO, "a 40ms click counted {counted:?}");
    assert!(!gate.is_open());
}

/// The gap between two words is speech; the pause between two sentences is not.
/// Both are silence — only their length tells them apart.
#[test]
fn short_gaps_are_bridged_and_long_pauses_are_not() {
    let mut gate = test_gate();
    feed(&mut gate, buffer_dbfs(0.002), 5.0);

    let bridged = feed(&mut gate, buffer_dbfs(0.2), 0.5)
        + feed(&mut gate, buffer_dbfs(0.002), 0.2)
        + feed(&mut gate, buffer_dbfs(0.2), 0.5);
    assert!(
        (1.15..=1.25).contains(&bridged.as_secs_f64()),
        "a 200ms gap inside a phrase counted {bridged:?}, expected the gap bridged"
    );

    // The same speech either side of a two-second pause counts only the speech.
    let mut gate = test_gate();
    feed(&mut gate, buffer_dbfs(0.002), 5.0);
    let paused = feed(&mut gate, buffer_dbfs(0.2), 0.5)
        + feed(&mut gate, buffer_dbfs(0.002), 2.0)
        + feed(&mut gate, buffer_dbfs(0.2), 0.5);
    assert!(
        (0.95..=1.05).contains(&paused.as_secs_f64()),
        "a 2s pause between phrases counted {paused:?}, expected it dropped"
    );
}

/// The point of tracking the floor: the same voice, recorded 20 dB hotter,
/// counts the same. A fixed threshold is what needs re-tuning per mic.
#[test]
fn the_floor_follows_the_room() {
    let quiet = {
        let mut gate = test_gate();
        feed(&mut gate, buffer_dbfs(0.0002), 5.0);
        feed(&mut gate, buffer_dbfs(0.02), 2.0)
    };
    let loud = {
        let mut gate = test_gate();
        feed(&mut gate, buffer_dbfs(0.002), 5.0);
        feed(&mut gate, buffer_dbfs(0.2), 2.0)
    };
    assert!(
        (quiet.as_secs_f64() - loud.as_secs_f64()).abs() < 0.05,
        "same signal-to-noise counted differently: quiet rig {quiet:?}, loud rig {loud:?}"
    );
}

/// Hiss is above the absolute threshold and is not speech. Level alone cannot
/// tell them apart — only that a voice dips and a hiss does not — so the meter
/// gets one phrase's worth of benefit of the doubt and then recalibrates.
#[test]
fn steady_noise_stops_counting_once_it_has_been_heard_out() {
    let mut gate = test_gate();
    let hiss = buffer_dbfs(0.05);

    let early = feed(&mut gate, hiss, 30.0);
    assert!(
        early.as_secs_f64() < 21.0,
        "30s of hiss counted {early:?} before recalibrating"
    );
    let later = feed(&mut gate, hiss, 30.0);
    assert_eq!(later, Duration::ZERO, "hiss still counted {later:?}");

    // And a voice over it still registers, against the raised floor.
    let over = feed(&mut gate, buffer_dbfs(0.5), 1.0);
    assert!(
        over.as_secs_f64() > 0.9,
        "a voice over the hiss counted only {over:?}"
    );
}

/// The floor must not chase a voice up mid-sentence: two minutes of talking
/// counts as two minutes, not as however long the follower took to catch up.
///
/// Modelled as speech with the gaps in it — half-second phrases separated by
/// 120ms — because that is what distinguishes a monologue from a tone, and a
/// constant amplitude would be asserting the opposite of the test above.
#[test]
fn a_long_monologue_stays_counted() {
    let mut gate = test_gate();
    let room = buffer_dbfs(0.002);
    let voice = buffer_dbfs(0.2);
    feed(&mut gate, room, 5.0);

    let mut counted = Duration::ZERO;
    for _ in 0..194 {
        counted += feed(&mut gate, voice, 0.5);
        counted += feed(&mut gate, room, 0.12);
    }
    assert!(
        counted.as_secs_f64() > 118.0,
        "two minutes of talking counted {counted:?}"
    );
}

/// Peak and RMS are different questions, and the gap between them is the whole
/// reason the bar draws peak: a voice's crest factor means an input that reads
/// comfortable in RMS can already be clipping.
#[test]
fn peak_sees_the_transient_that_rms_averages_away() {
    // One full-scale sample in an otherwise quiet buffer.
    let mut samples = vec![0.01f32; 512];
    samples[300] = 1.0;
    let levels = levels_dbfs(&float_bytes(&samples), F32_MONO);

    assert!(
        levels.peak_dbfs > -0.1,
        "peak caught it: {}",
        levels.peak_dbfs
    );
    assert!(
        levels.rms_dbfs < -20.0,
        "rms averaged it away: {}",
        levels.rms_dbfs
    );
    assert!(
        levels.peak_dbfs >= levels.rms_dbfs,
        "peak is never below rms"
    );
}

#[test]
fn peak_is_symmetric_because_clipping_is() {
    let up = levels_dbfs(&float_bytes(&[0.0, 0.8, 0.0]), F32_MONO);
    let down = levels_dbfs(&float_bytes(&[0.0, -0.8, 0.0]), F32_MONO);
    assert_eq!(up.peak_dbfs, down.peak_dbfs);
}

#[test]
fn an_empty_buffer_is_silent_in_both_measures() {
    let levels = levels_dbfs(&[], F32_MONO);
    assert_eq!(levels.rms_dbfs, SILENT_DBFS);
    assert_eq!(levels.peak_dbfs, SILENT_DBFS);
}

/// A steady tone is the one case where they nearly agree — within the 3dB a
/// square wave's crest factor allows — which is a sanity check on the units.
#[test]
fn a_square_wave_peaks_where_it_averages() {
    let levels = levels_dbfs(&float_bytes(&[1.0, -1.0, 1.0, -1.0]), F32_MONO);
    assert!((levels.peak_dbfs - levels.rms_dbfs).abs() < 0.01);
}
