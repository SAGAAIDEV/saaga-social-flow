//! A break in a chapter: the time taken out of its files while nothing was
//! being recorded, and where every later buffer lands because of it.
//!
//! A figure is taken on a break — see [`crate::figure::aside`]. The author
//! stops, snips the screen, explains it into the microphone, and picks the take
//! back up. The chapter has to come out as one continuous file with the break
//! simply absent: not a new chapter, not a new take, and not a hole where the
//! picture freezes and the sound goes quiet for however long the explanation
//! ran.
//!
//! `AVAssetWriter` writes whatever timestamps it is handed, so the break is
//! removed by *retiming*. While paused, buffers are dropped. Once resumed,
//! every buffer is appended with its timestamp pulled back by the total length
//! of every break so far, and the writer never sees the gap. The copy is cheap —
//! [`CMSampleBuffer::create_copy_with_new_timing`] shares the underlying data —
//! and is only made once there is a shift to apply; before the first break a
//! chapter is appended exactly as it always was.
//!
//! ## Late buffers
//!
//! Capture queues deliver a little behind real time, so at both edges of a
//! break a buffer can arrive that belongs to the other side. One captured just
//! before the pause but delivered just after it is still appended: dropping it
//! would cost a frame, or twenty milliseconds of audio at the seam, which is a
//! click. One captured *during* the break but delivered after the resume is
//! dropped: pulled back by the new total it would land before the last buffer
//! already written, and the writer refuses timestamps that run backwards. The
//! `floor` moves up to each resume for exactly this reason. Before the first
//! break it is the chapter's anchor, and the check it replaces.
//!
//! Clock-agnostic: every time here is on whichever stream's clock the owning
//! state uses. The two halves of a chapter each keep their own [`Pause`], fed
//! from the same real instant by the Router, the way their anchors are.

use std::ptr::NonNull;

use objc2_core_foundation::CFRetained;
use objc2_core_media::{kCMTimeInvalid, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags};

/// Where a chapter's buffers land once some of its time has been taken out.
#[derive(Clone, Copy)]
pub struct Pause {
    /// Raw stream time below which nothing is appended: the anchor, then each
    /// resume — see the module docs on late buffers.
    floor: CMTime,
    /// Raw stream time the current break began, while one is on.
    paused_at: Option<CMTime>,
    /// Every break so far, summed. `None` until the first has ended, which is
    /// also the signal that buffers can go through untouched.
    removed: Option<CMTime>,
}

impl Pause {
    /// A chapter that has just opened at `anchor`, with nothing taken out.
    pub fn new(anchor: CMTime) -> Pause {
        Pause {
            floor: anchor,
            paused_at: None,
            removed: None,
        }
    }

    /// Stop appending from `now`. A second call while paused changes nothing.
    pub fn pause(&mut self, now: CMTime) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now);
        }
    }

    /// Start appending again from `now`, with the break just ended added to
    /// the shift. A call while not paused changes nothing.
    pub fn resume(&mut self, now: CMTime) {
        let Some(paused_at) = self.paused_at.take() else {
            return;
        };
        let gap = unsafe { now.subtract(paused_at) };
        self.removed = Some(match self.removed {
            Some(removed) => unsafe { removed.add(gap) },
            None => gap,
        });
        self.floor = now;
    }

    /// Only the tests read this back; the router tracks the break itself.
    #[cfg(test)]
    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Whether buffers need retiming at all. False until the first break ends.
    pub fn is_shifted(&self) -> bool {
        self.removed.is_some()
    }

    /// Everything removed so far, in seconds. Only the tests read it back.
    #[cfg(test)]
    pub fn removed_seconds(&self) -> f64 {
        self.removed.map(crate::timesync::seconds).unwrap_or(0.0)
    }

    /// Where a buffer stamped `pts` lands in the file, or `None` to drop it:
    /// before the chapter began, during a break, or straggling in from one.
    pub fn place(&self, pts: CMTime) -> Option<CMTime> {
        if before(pts, self.floor) {
            return None;
        }
        if self
            .paused_at
            .is_some_and(|paused_at| !before(pts, paused_at))
        {
            return None;
        }
        Some(match self.removed {
            Some(removed) => unsafe { pts.subtract(removed) },
            None => pts,
        })
    }
}

fn before(a: CMTime, b: CMTime) -> bool {
    let order = unsafe { a.compare(b) };
    order < 0
}

/// `buffer` with its timing moved so its first sample sits at `pts`.
///
/// Read off the buffer's own first timing entry and shifted, rather than built
/// from scratch: an audio buffer holds hundreds of samples described by one
/// entry whose `duration` is *per sample*, and a fresh entry with the buffer's
/// whole duration in that slot would stretch every sample to fill it. The
/// decode stamp moves by the same amount when it is set at all — camera and
/// microphone buffers carry none. `None` when Core Media refuses, which the
/// caller treats as a dropped buffer rather than one appended at the wrong time.
pub fn retime(buffer: &CMSampleBuffer, pts: CMTime) -> Option<CFRetained<CMSampleBuffer>> {
    let mut timing = CMSampleTimingInfo {
        duration: unsafe { kCMTimeInvalid },
        presentationTimeStamp: unsafe { kCMTimeInvalid },
        decodeTimeStamp: unsafe { kCMTimeInvalid },
    };
    if unsafe { buffer.sample_timing_info(0, NonNull::from(&mut timing)) } != 0 {
        return None;
    }
    let shift = unsafe { timing.presentationTimeStamp.subtract(pts) };
    timing.presentationTimeStamp = pts;
    if timing.decodeTimeStamp.flags.contains(CMTimeFlags::Valid) {
        timing.decodeTimeStamp = unsafe { timing.decodeTimeStamp.subtract(shift) };
    }
    let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
    let status = unsafe {
        CMSampleBuffer::create_copy_with_new_timing(
            None,
            buffer,
            1,
            &timing,
            NonNull::from(&mut out),
        )
    };
    if status != 0 || out.is_null() {
        return None;
    }
    Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timesync::seconds;

    fn at(secs: f64) -> CMTime {
        CMTime {
            value: (secs * 1_000_000_000.0).round() as i64,
            timescale: 1_000_000_000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    }

    fn placed(pause: &Pause, secs: f64) -> Option<f64> {
        pause.place(at(secs)).map(seconds)
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-6)
    }

    /// Before any break this is the anchor check it replaces, and nothing moves.
    #[test]
    fn an_unbroken_chapter_passes_buffers_through_from_its_anchor() {
        let pause = Pause::new(at(100.0));
        assert_eq!(placed(&pause, 99.9), None, "before the anchor");
        assert!(close(placed(&pause, 100.0), 100.0));
        assert!(close(placed(&pause, 130.5), 130.5));
        assert!(!pause.is_shifted());
        assert_eq!(pause.removed_seconds(), 0.0);
    }

    /// The whole point: after a three-second break, a buffer stamped thirteen
    /// seconds in lands at ten, right where the file left off.
    #[test]
    fn a_break_is_taken_out_of_everything_after_it() {
        let mut pause = Pause::new(at(0.0));
        pause.pause(at(10.0));
        assert!(pause.is_paused());
        assert_eq!(placed(&pause, 10.0), None, "the break has begun");
        assert_eq!(placed(&pause, 11.0), None, "during the break");

        pause.resume(at(13.0));
        assert!(!pause.is_paused());
        assert!(pause.is_shifted());
        assert!((pause.removed_seconds() - 3.0).abs() < 1e-6);
        assert!(close(placed(&pause, 13.0), 10.0));
        assert!(close(placed(&pause, 14.5), 11.5));
    }

    /// Breaks add up; the second one shifts by both.
    #[test]
    fn a_second_break_adds_to_the_first() {
        let mut pause = Pause::new(at(0.0));
        pause.pause(at(10.0));
        pause.resume(at(13.0));
        pause.pause(at(20.0));
        pause.resume(at(21.0));
        assert!((pause.removed_seconds() - 4.0).abs() < 1e-6);
        assert!(close(placed(&pause, 21.0), 17.0));
    }

    /// The two kinds of late buffer, and why they are treated differently —
    /// see the module docs.
    #[test]
    fn a_late_buffer_from_before_the_break_lands_and_one_from_during_it_does_not() {
        let mut pause = Pause::new(at(0.0));
        pause.pause(at(10.0));
        // Captured at 9.98, delivered after the pause began: still the take's.
        assert!(close(placed(&pause, 9.98), 9.98));

        pause.resume(at(13.0));
        // Captured at 12.9, during the break, delivered after the resume: pulled
        // back it would land at 9.9, before what is already written.
        assert_eq!(placed(&pause, 12.9), None);
        assert!(close(placed(&pause, 13.0), 10.0));
    }

    #[test]
    fn pausing_twice_or_resuming_idle_changes_nothing() {
        let mut pause = Pause::new(at(0.0));
        pause.resume(at(5.0));
        assert!(!pause.is_shifted(), "a resume with no pause removed time");
        pause.pause(at(10.0));
        pause.pause(at(11.0));
        pause.resume(at(12.0));
        assert!(
            (pause.removed_seconds() - 2.0).abs() < 1e-6,
            "the second pause moved the start"
        );
    }
}

#[cfg(test)]
mod live {
    use std::time::Duration;

    use crate::capture::av::{self, Connection};
    use crate::timesync::TimeSync;

    /// Records one second, breaks for one, records one more, and checks the
    /// file is two seconds long rather than three.
    ///
    /// The proof that retiming reaches the encoder: a real chapter writer on a
    /// real stream, paused and resumed the way the Router does it. Needs a
    /// camera and a microphone, so it does not run by default.
    ///
    /// `cargo test capture::pause::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn a_break_is_absent_from_the_file() {
        assert!(
            crate::permissions::ensure_audio_access().expect("permission query"),
            "microphone permission denied — System Settings > Privacy & Security > Microphone"
        );
        let cfg = crate::config::load();
        let mics = av::list_audio_devices().expect("audio devices");
        let uid = cfg
            .audio_device_uid
            .filter(|uid| mics.iter().any(|d| &d.uid == uid))
            .unwrap_or_else(|| mics[0].uid.clone());
        let camera = av::list_camera_devices().expect("cameras")[0].uid.clone();
        let conn = Connection::start_capture(&camera, &uid).expect("capture");
        conn.wait_for_warmup(Duration::from_secs(10))
            .expect("warmup");

        let dir =
            std::env::temp_dir().join(format!("stream-recorder-pause-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chapter.mp4");
        conn.install_writer(&path).expect("chapter writer");
        let clock = conn.sync_clock().expect("clock");
        let state = conn.delegate.state_arc();

        std::thread::sleep(Duration::from_secs(1));
        {
            let mut guard = state.lock().unwrap();
            guard
                .as_mut()
                .unwrap()
                .pause
                .pause(TimeSync::now_on(&clock));
        }
        let before = conn.delegate.audio_frames_appended();
        std::thread::sleep(Duration::from_secs(1));
        let during = conn.delegate.audio_frames_appended();
        {
            let mut guard = state.lock().unwrap();
            guard
                .as_mut()
                .unwrap()
                .pause
                .resume(TimeSync::now_on(&clock));
        }
        std::thread::sleep(Duration::from_secs(1));
        conn.stop_and_finish().expect("finish");
        crate::transcode::fix_mp4_metadata(&path).expect("metadata repair");

        let seconds = crate::edit::cut::probe_duration_seconds(&path).expect("ffprobe");
        println!(
            "chapter: {seconds:.2}s in the file for 3s of wall clock with a 1s break; \
             {} audio buffer(s) appended during the break",
            during - before
        );
        assert!(
            during - before <= 2,
            "{} buffers were appended during the break",
            during - before
        );
        assert!(
            (seconds - 2.0).abs() < 0.35,
            "the file is {seconds:.2}s — the break was not taken out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
