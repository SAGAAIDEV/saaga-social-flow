//! Chapter/take state machine: manages chapter numbering, swaps AVAssetWriter
//! outputs, finalizes completed chapters, and logs chapter/retake events.
//!
//! The router owns:
//! - Current chapter number (1-indexed)
//! - Reference to the AvDelegate's shared state (the Arc<Mutex<AvState>>)
//! - The session directory (where chapter files land)
//! - The live session's encoder settings, reused for every chapter writer
//! - Each stream's [`crate::ops`] graph lifecycle: built and opened per chapter
//!   in `build_chapter`, carried inside the state it belongs to, and closed
//!   beside the finished file in `finish_chapter` or beside the *moved* file in
//!   `discard_chapter`
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | the state machine: chapter numbering, and the order writers are swapped in |
//! | [`chapter`] | one chapter's outputs — writers, anchors, graphs, paths, finish and discard |

mod chapter;

pub use chapter::ScreenTrack;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_media::CMClock;
use objc2_foundation::{NSDictionary, NSString};

use crate::capture::av_delegate::{AvDelegate, AvState};
use crate::capture::screen_delegate::ScreenState;
use crate::layouts::Pair;
use crate::markers::MarkerLog;

/// The chapter/take router state machine.
pub struct Router {
    session_dir: PathBuf,
    current_chapter: u32,
    avdelegate_state: Arc<std::sync::Mutex<Option<AvState>>>,
    /// The encoder settings queried from the live capture session at startup.
    /// Every chapter writer must be built from these — see create_chapter_writer.
    video_settings: Retained<NSDictionary<NSString, AnyObject>>,
    audio_settings: Retained<NSDictionary<NSString, AnyObject>>,
    /// The clock camera/mic buffers are timestamped against.
    av_clock: Retained<CMClock>,
    /// `None` unless a display is being captured.
    screen: Option<ScreenTrack>,
    pair: Pair,
    marker_log: MarkerLog,
}


impl Router {
    /// Open `first_chapter` and install its writer, against a capture session
    /// that is already running.
    ///
    /// There is deliberately no `start()` convenience that hardcodes chapter 1:
    /// chapter 1 is not special, and the moment it *was* special — created at
    /// startup instead of on demand — it was the one chapter that reliably
    /// came out as static, because its writer locked onto the audio format the
    /// device was using before AppKit came up. Every chapter now begins the
    /// same way, from a user action, well clear of any device renegotiation.
    /// See the `app` module docs.
    pub fn start_at(
        session_dir: &Path,
        avdelegate: &Retained<AvDelegate>,
        video_settings: Retained<NSDictionary<NSString, AnyObject>>,
        audio_settings: Retained<NSDictionary<NSString, AnyObject>>,
        av_clock: Retained<CMClock>,
        screen: Option<ScreenTrack>,
        pair: Pair,
        first_chapter: u32,
    ) -> Result<Router> {
        let marker_log = MarkerLog::new(session_dir)?;
        let state_arc = avdelegate.state_arc();

        let router = Router {
            session_dir: session_dir.to_path_buf(),
            current_chapter: first_chapter,
            avdelegate_state: state_arc,
            video_settings,
            audio_settings,
            av_clock,
            screen,
            pair,
            marker_log,
        };
        router.open_chapter(first_chapter)?;
        Ok(router)
    }

    /// Install a built chapter, returning whatever it displaced.
    fn install_chapter(
        &self,
        av: AvState,
        screen: Option<ScreenState>,
    ) -> (Option<AvState>, Option<ScreenState>) {
        let old_av = self.avdelegate_state.lock().unwrap().replace(av);
        let old_screen = match (&self.screen, screen) {
            (Some(track), Some(state)) => {
                let anchor = state.anchor;
                let displaced = track.state.lock().unwrap().replace(state);
                // Must follow the install, not precede it: seeding appends to
                // whatever writer is currently in place.
                track.delegate.seed_chapter(anchor);
                displaced
            }
            // No screen track configured, or none built: clear any stale state
            // rather than leaving a finished writer installed.
            (Some(track), None) => track.state.lock().unwrap().take(),
            (None, _) => None,
        };
        (old_av, old_screen)
    }

    fn open_chapter(&self, chapter: u32) -> Result<()> {
        let (av, screen) = self.build_chapter(chapter)?;
        self.install_chapter(av, screen);
        Ok(())
    }

    /// Take both of the current chapter's writers out of the delegates.
    fn take_current(&self) -> Result<(AvState, Option<ScreenState>)> {
        let av = self
            .avdelegate_state
            .lock()
            .unwrap()
            .take()
            .context("no chapter was recording")?;
        let screen = self
            .screen
            .as_ref()
            .and_then(|track| track.state.lock().unwrap().take());
        Ok((av, screen))
    }

    /// Cut to a new chapter: finish the current chapter (extract audio, transcode)
    /// and open the next one.
    ///
    /// Both files are closed and both reopened together, so the pair stays
    /// chapter-for-chapter aligned even if one of them errors.
    pub fn cut_chapter(&mut self) -> Result<()> {
        let old_chapter = self.current_chapter;
        let next_chapter = old_chapter + 1;

        // Build first, swap second: see build_chapter.
        let (av, screen) = self.build_chapter(next_chapter)?;
        let (old_av, old_screen) = self.install_chapter(av, screen);
        let old_av = old_av.context("no chapter was recording")?;
        self.current_chapter = next_chapter;

        // Finish the old chapter synchronously (finish() is fast enough).
        self.finish_chapter(old_av, old_screen, old_chapter)?;

        Ok(())
    }

    /// Finish the current chapter, let the caller change the screen stream,
    /// then open the next chapter against whatever it hands back.
    ///
    /// This is what a layout or region change goes through. It cannot be a
    /// `cut_chapter` with a step bolted on, because the ordering is the exact
    /// reverse: a cut *builds first and swaps second* so no buffer is dropped,
    /// while this has to have **no writer installed at all** while the stream
    /// is reconfigured. An `AVAssetWriterInput` locks its dimensions at
    /// creation, and a resized `SCStream` delivering into a live writer is a
    /// mismatch neither framework reports — ScreenCaptureKit does not know
    /// about the writer, and AVFoundation is handed buffers that are simply the
    /// wrong size. Doing it in one method rather than exposing a finish and an
    /// open means a caller cannot get that order wrong.
    ///
    /// The cost is a real gap: buffers arriving while `reconfigure` runs are
    /// dropped. That is correct — they belong to neither chapter, having been
    /// captured at neither framing.
    ///
    /// **On error the Router is left writerless.** If `reconfigure` or the
    /// rebuild fails, the finished chapter is safely on disk and nothing is
    /// recording; the caller's recovery is to drop this Router and let the next
    /// New Chapter press build a fresh one, which is what `app` does for every
    /// other screen failure.
    pub fn reopen_with_screen(
        &mut self,
        reconfigure: impl FnOnce() -> Result<Option<ScreenTrack>>,
    ) -> Result<()> {
        let old_chapter = self.current_chapter;
        let (old_av, old_screen) = self.take_current()?;
        self.finish_chapter(old_av, old_screen, old_chapter)?;

        // Nothing is installed now, so the stream's dimensions are free to
        // move. Anchors are re-derived by build_chapter below, against
        // whatever clock the replacement track carries — which matters when
        // reconfigure restarted the stream rather than updating it.
        self.screen = reconfigure()?;

        let next_chapter = old_chapter + 1;
        let (av, screen) = self.build_chapter(next_chapter)?;
        self.install_chapter(av, screen);
        self.current_chapter = next_chapter;
        Ok(())
    }

    /// Retake the current chapter: discard the in-progress files, start fresh
    /// with the same chapter number.
    ///
    /// Order matters here in a way it does not for a cut: the old writers must
    /// be finished and their files moved to `.discarded/` BEFORE the
    /// replacements are created — AVAssetWriter refuses to startWriting over an
    /// existing file (AVError -11823 "Cannot Save"), and the replacements
    /// target the same chapter-NN paths. The delegates simply drop buffers
    /// during the brief writerless gap, which is fine: a retake is explicitly
    /// throwing the current take away.
    pub fn retake_chapter(&mut self) -> Result<()> {
        let chapter_num = self.current_chapter;

        // Pull both chapters out first; capture keeps running writerless.
        let (old_av, old_screen) = self.take_current()?;
        self.discard_chapter(old_av, old_screen, chapter_num)?;

        // The paths are free now — build the same-numbered chapter fresh.
        self.open_chapter(chapter_num)
            .context("creating writers for the retaken chapter")
    }

    /// Stop recording: finish the current chapter and clean up.
    /// This should be called when the user presses Quit or the app exits.
    pub fn stop(&mut self) -> Result<()> {
        // Take both states out entirely: after stop, any buffer still in
        // flight gets dropped instead of racing a finished writer.
        let (av, screen) = self.take_current()?;
        self.finish_chapter(av, screen, self.current_chapter)
            .context("finishing final chapter")
    }

    /// Stop appending to the open chapter without closing it — a break for a
    /// figure, see [`crate::capture::pause`]. Both files pause from the same
    /// real instant, the way both open from one.
    pub fn pause_chapter(&self) -> Result<()> {
        let (av_now, screen_now) = self.anchors();
        {
            let mut guard = self.avdelegate_state.lock().unwrap();
            let av = guard.as_mut().context("no chapter was recording")?;
            av.pause.pause(av_now);
        }
        if let (Some(track), Some(now)) = (&self.screen, screen_now) {
            if let Some(screen) = track.state.lock().unwrap().as_mut() {
                screen.pause.pause(now);
            }
        }
        Ok(())
    }

    /// Pick the open chapter back up. Everything appended from here is retimed
    /// so the break is absent from the files.
    pub fn resume_chapter(&self) -> Result<()> {
        let (av_now, screen_now) = self.anchors();
        {
            let mut guard = self.avdelegate_state.lock().unwrap();
            let av = guard.as_mut().context("no chapter was recording")?;
            av.pause.resume(av_now);
        }
        if let (Some(track), Some(now)) = (&self.screen, screen_now) {
            if let Some(screen) = track.state.lock().unwrap().as_mut() {
                screen.pause.resume(now);
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn current_chapter_number(&self) -> u32 {
        self.current_chapter
    }

    pub fn set_pair(&mut self, pair: Pair) {
        self.pair = pair;
    }
}

#[cfg(test)]
mod tests;
