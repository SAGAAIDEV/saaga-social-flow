//! The shared clock every writer and marker anchors to, so a marker's
//! timestamp is valid against camera and screen files without per-file
//! translation.
//!
//! Camera/mic buffers are timestamped against the `AVCaptureSession`'s
//! `synchronizationClock`; screen frames against the `SCStream`'s. These are
//! **not guaranteed to be the same clock** — a capture session with an audio
//! input commonly runs on the audio device's clock rather than the host
//! clock, and that clock drifts against the host by design (it is tied to the
//! interface's sample rate, not the CPU's).
//!
//! So a screen PTS and a camera PTS are only directly comparable once both
//! are expressed on one clock. Everything here exists to make that explicit
//! rather than assumed: [`same_timeline`] answers whether conversion is
//! needed at all, and [`convert`] does it when it is.

use objc2_core_media::{CMClock, CMSyncConvertTime, CMSyncGetRelativeRate, CMTime};

pub struct TimeSync;

impl TimeSync {
    /// The current time on Core Media's host clock.
    pub fn now_host_time() -> CMTime {
        unsafe { CMClock::host_time_clock().time() }
    }

    /// The current time on a specific clock.
    pub fn now_on(clock: &CMClock) -> CMTime {
        unsafe { clock.time() }
    }
}

/// Re-express `time` from one clock's timeline on another's.
pub fn convert(time: CMTime, from: &CMClock, to: &CMClock) -> CMTime {
    unsafe { CMSyncConvertTime(time, from, to) }
}

/// Whether two clocks run at the same rate *and* read the same value now —
/// i.e. whether timestamps from one can be used against the other untouched.
///
/// A rate of exactly 1.0 is not enough on its own: two clocks can tick at the
/// same rate from different epochs, which would put the two files a constant
/// offset apart. Both conditions have to hold.
pub fn same_timeline(a: &CMClock, b: &CMClock) -> bool {
    drift_ppm(a, b) == 0.0 && offset_seconds(a, b).abs() < 0.001
}

/// How fast `a` runs relative to `b`, in parts per million.
///
/// Multiply by 3.6 for milliseconds of drift per hour. Measured at +3.6 ppm
/// (≈13 ms/hour) between an Elgato XLR Dock's clock and the host clock — small
/// enough to ignore across a chapter, large enough to matter across an hour.
pub fn drift_ppm(a: &CMClock, b: &CMClock) -> f64 {
    (unsafe { CMSyncGetRelativeRate(a, b) } - 1.0) * 1e6
}

/// How far `a` reads ahead of `b`, in seconds, sampled now.
///
/// Sampling is not instantaneous, so a few microseconds of noise is expected;
/// this is for telling "same timeline" from "hundreds of milliseconds apart",
/// not for calibration.
pub fn offset_seconds(a: &CMClock, b: &CMClock) -> f64 {
    let now_a = unsafe { a.time() };
    let now_a_on_b = convert(now_a, a, b);
    let now_b = unsafe { b.time() };
    seconds(now_a_on_b) - seconds(now_b)
}

/// A `CMTime` as floating-point seconds, or 0.0 if it is not a valid time.
pub fn seconds(time: CMTime) -> f64 {
    if time.timescale == 0 {
        return 0.0;
    }
    time.value as f64 / f64::from(time.timescale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_media::CMTimeFlags;

    fn time(value: i64, timescale: i32) -> CMTime {
        CMTime {
            value,
            timescale,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    }

    #[test]
    fn seconds_divides_value_by_timescale() {
        assert!((seconds(time(48_000, 48_000)) - 1.0).abs() < f64::EPSILON);
        assert!((seconds(time(3, 2)) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn seconds_treats_a_zero_timescale_as_zero_rather_than_dividing_by_it() {
        assert_eq!(seconds(time(1, 0)), 0.0);
    }

    #[test]
    fn the_host_clock_is_on_its_own_timeline() {
        let host = unsafe { CMClock::host_time_clock() };
        assert!(same_timeline(&host, &host));
        assert!(offset_seconds(&host, &host).abs() < 0.001);
    }

    #[test]
    fn the_host_clock_advances() {
        let first = TimeSync::now_host_time();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = TimeSync::now_host_time();
        assert!(
            seconds(second) > seconds(first),
            "host clock did not advance: {} then {}",
            seconds(first),
            seconds(second)
        );
    }
}
