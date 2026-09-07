//! The aside: what the author says over a figure, recorded on its own.
//!
//! A figure is taken on a break. The chapter is paused — see
//! [`crate::capture::pause`] — nothing is being written to it, and the author
//! turns from the screen to the microphone to say what the reader is looking
//! at. That explanation is the other half of the figure: the caption and the
//! paragraph around it are written from it. So it is recorded as its own file,
//! `figures/figure-NN.m4a`, beside the picture, and the take picks up
//! afterwards as though the break never happened.
//!
//! ## The same session, a second sink
//!
//! The mic is not opened again. `capture::av`'s session keeps running, and an
//! aside is a second writer hung off the same delegate —
//! [`AvDelegate::install_aside`] — that is fed every audio buffer and nothing
//! else, while the chapter's own writers stay installed and paused. Two reasons
//! it is done this way rather than with a second `AVCaptureSession` on the same
//! device. The format the device has settled on is the one this writer locks
//! onto, where a fresh session would start cold and could renegotiate under the
//! writer — the failure `av_delegate` documents as a whole chapter of static.
//! And the level meter and the recording clock keep reading the one stream they
//! always read.
//!
//! ## What happens after is the caller's
//!
//! [`Aside::finish`] finalises the file and hands back where it is and how long
//! it ran. It does not start the transcript, for the reason
//! `Router::finish_chapter` starts a chapter's rather than the writer doing so:
//! the writer's job ends at a playable file, and the one caller that should not
//! upload anything — the hardware test below — is then free not to. Pausing and
//! resuming the chapter are the caller's too, as is the clock that shows how
//! long the break has run; this records what is said in between and nothing
//! more.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use objc2::rc::Retained;

use crate::capture::av::{self, Connection};
use crate::capture::av_delegate::{AsideState, AvDelegate};

/// An aside being recorded. Dropping it without [`Aside::finish`] leaves the
/// writer installed and the file unplayable, so every path that holds one has
/// to end in `finish` — including Quit.
pub struct Aside {
    delegate: Retained<AvDelegate>,
    path: PathBuf,
    started: Instant,
}

/// What an aside came to.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub path: PathBuf,
    /// Wall clock from `start` to `finish`, which is what the status line
    /// showed while it ran. The file's own duration is the authority.
    pub duration: Duration,
}

impl Aside {
    /// Install an audio-only writer for `path` beside whatever the session is
    /// doing. Refused while another aside is recording.
    pub fn start(conn: &Connection, path: &Path) -> Result<Aside> {
        let clock = conn.sync_clock()?;
        let anchor = crate::timesync::TimeSync::now_on(&clock);
        let writer = av::create_audio_writer(&conn.audio_settings, path)
            .with_context(|| format!("creating the aside writer for {}", path.display()))?;
        conn.delegate.install_aside(AsideState::new(writer, anchor))?;
        Ok(Aside {
            delegate: conn.delegate.clone(),
            path: path.to_path_buf(),
            started: Instant::now(),
        })
    }

    /// Take the writer out and finalise the file.
    pub fn finish(self) -> Result<Finished> {
        let state = self
            .delegate
            .take_aside()
            .context("the aside's writer was already taken out")?;
        state.finish().context("finishing the aside")?;
        // The same repair every chapter gets. The writer's session started
        // partway through a continuous stream, which is exactly the case that
        // has left AVAssetWriter's duration metadata wrong before.
        crate::transcode::fix_mp4_metadata(&self.path)
            .context("repairing the aside's container metadata")?;
        Ok(Finished {
            path: self.path,
            duration: self.started.elapsed(),
        })
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Records two seconds from the configured mic through a running session
    /// and checks the file agrees with the clock.
    ///
    /// The one test that proves an aside is a playable file of the right
    /// length: a writer hung off a stream already in progress, then finalised.
    /// Needs a camera and a microphone, so it does not run by default.
    ///
    /// `cargo test figure::aside::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn records_two_seconds_of_mic_to_a_playable_m4a() {
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
        conn.wait_for_warmup(Duration::from_secs(10)).expect("warmup");

        let dir = std::env::temp_dir().join(format!("stream-recorder-aside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("figures").join("figure-01.m4a");

        let aside = Aside::start(&conn, &path).expect("aside starts on an idle session");
        // Starting a second one must fail, and must not touch the first.
        let refused = Aside::start(&conn, &dir.join("second.m4a"));
        assert!(refused.is_err(), "a second aside was allowed over the first");

        std::thread::sleep(Duration::from_secs(2));
        let finished = aside.finish().expect("finish");
        assert!(path.is_file(), "no file at {}", path.display());

        let seconds = crate::edit::cut::probe_duration_seconds(&path).expect("ffprobe");
        println!(
            "aside: {:.2}s on the clock, {seconds:.2}s in the file, {} bytes",
            finished.duration.as_secs_f64(),
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        );
        assert!(
            (seconds - finished.duration.as_secs_f64()).abs() < 0.5,
            "the file is {seconds:.2}s but the aside ran {:.2}s",
            finished.duration.as_secs_f64()
        );
        // No chapter was installed, so the chapter counters must be untouched:
        // the aside is its own sink, not the chapter's.
        assert_eq!(conn.delegate.audio_frames_appended(), 0, "an aside fed the chapter counter");
        assert_eq!(conn.delegate.video_frames_appended(), 0, "an aside wrote video frames");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
