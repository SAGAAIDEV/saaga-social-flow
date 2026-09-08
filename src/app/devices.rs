//! Bringing capture devices up and down: camera, mic, and the screen stream.
//!
//! The rule this module exists to keep honest is that **a device change leaves
//! recording stopped**. Rebuilding an `AVCaptureSession` renegotiates the audio
//! device, and a writer created before that settles produces valid AAC that is
//! 100% static — silently, with no error. No fixed delay can guarantee the
//! transition is over, so the replacement writer waits for a human to press New
//! Chapter. See the [module docs](super) for the full history.
//!
//! A *layout* change is deliberately not like that, and the difference is the
//! reason these two concerns live in separate files: it touches neither the
//! capture session nor the audio device, so it cuts and re-opens a chapter
//! automatically. That path is in [`super::framing`].

use std::time::Duration;

use anyhow::{Context, Result};

use crate::app::App;
use crate::capture::av;
use crate::capture::{screen_stream, screen_writer};
use crate::router::ScreenTrack;

impl App {
    /// Finish the current chapter and rebuild capture on the new devices.
    ///
    /// Deliberately leaves recording *stopped*: a freshly started capture
    /// session is exactly the state that produces a static chapter, because
    /// the new device has not finished negotiating its audio format yet. The
    /// next New Chapter press opens the replacement writer once it has.
    pub(super) fn switch_devices(&mut self, camera_uid: String, audio_uid: String) -> Result<()> {
        if camera_uid == self.camera_uid && audio_uid == self.audio_uid {
            return Ok(());
        }

        // Close out the running session cleanly before touching devices.
        if let Some(conn) = &self.connection {
            unsafe { conn.session.stopRunning() };
        }
        // The aside's writer hangs off the session about to be torn down; it
        // has to be finished before that session goes.
        self.end_break(false);
        if let Some(router) = &mut self.router {
            self.next_chapter = router.current_chapter_number() + 1;
            router
                .stop()
                .context("finishing chapter for device switch")?;
        }
        // Bank the open chapter here too: the switch stops recording, and a
        // chapter left open on the clock would keep counting against a capture
        // session that no longer exists.
        self.clock.close();
        self.router = None;
        self.connection = None;

        let connection = av::Connection::start_capture(&camera_uid, &audio_uid)
            .context("restarting capture on new device")?;
        connection.wait_for_warmup(Duration::from_secs(5))?;

        self.camera_uid = camera_uid.clone();
        self.audio_uid = audio_uid.clone();
        self.connection = Some(connection);
        self.install_preview();

        let mut cfg = crate::config::load();
        cfg.camera_device_uid = Some(camera_uid);
        cfg.audio_device_uid = Some(audio_uid);
        crate::config::save(&cfg)?;

        println!(
            "stream-recorder: switched devices — press New Chapter to record chapter {}",
            self.next_chapter
        );
        self.sync_controls();
        Ok(())
    }

    /// Report how well the two capture clocks agree, once both are running.
    ///
    /// Worth printing rather than assuming: the camera session and the screen
    /// stream are timestamped against different clocks (a capture session with
    /// an audio input runs on the audio interface's clock), and the rate
    /// difference between them is what makes a chapter's two files drift
    /// apart. A few ppm is normal and harmless at chapter lengths; a large
    /// number here is the first thing to look at if a pair ever looks out of
    /// sync in the edit.
    pub(super) fn report_clock_drift(&self) {
        let (Some(conn), Some(screen)) = (self.connection.as_ref(), self.screen.as_ref()) else {
            return;
        };
        let (Ok(av_clock), Some(screen_clock)) = (conn.sync_clock(), screen.sync_clock()) else {
            return;
        };
        if crate::timesync::same_timeline(&screen_clock, &av_clock) {
            println!("stream-recorder: screen and camera share one clock — no drift");
            return;
        }
        let drift_ms_per_hour = crate::timesync::drift_ppm(&screen_clock, &av_clock) * 3.6;
        println!(
            "stream-recorder: screen vs camera clock drift {drift_ms_per_hour:+.1} ms/hour \
             (offset now {:+.1} ms)",
            crate::timesync::offset_seconds(&screen_clock, &av_clock) * 1000.0,
        );
    }

    /// The display to capture, or `None` when there should be no screen
    /// capture at all.
    ///
    /// Two independent reasons to have none, and they are genuinely different
    /// things: the operator picked "No screen" in the dropdown, or the current
    /// layout has no screen slot. A talking-head chapter is camera-only in both
    /// orientations, so recording a screen file for it would produce a second
    /// output that no composition consumes and no stage reads.
    pub(super) fn screen_wanted(&self) -> Option<&str> {
        self.layout()
            .needs_screen()
            .then_some(self.screen_uid.as_deref())
            .flatten()
    }

    /// Bring up screen capture for the current display and layout, if both
    /// call for one. A no-op otherwise, so callers do not have to ask twice.
    ///
    /// Never fatal. A screen that will not open is a degraded session — camera
    /// and mic still record — so this reports and clears the choice rather than
    /// propagating, which is how the screen path has always behaved.
    pub(super) fn open_screen(&mut self) {
        let Some(uid) = self.screen_wanted().map(str::to_string) else {
            return;
        };
        // Refuse rather than fall back. `start_capture(_, None)` is a valid
        // call that configures the whole display at its native size — right for
        // a caller that wants everything, catastrophic here: the file would come
        // out at the display's aspect instead of the slot's, and `object-fit:
        // cover` would shear a quarter of the width off in the render with
        // nothing anywhere reporting a problem. `current_capture` returns `None`
        // whenever the region set is empty, which `rebuild_regions` leaves it as
        // when `display_geometry` fails, so this is reachable from a display
        // that is mid-mode-change or going to sleep.
        let Some(capture) = self.current_capture() else {
            eprintln!(
                "stream-recorder: {} needs a screen region and this display has none yet — \
                 not starting screen capture rather than recording the whole display at the \
                 wrong aspect",
                self.layout().block,
            );
            return;
        };
        match screen_stream::ScreenConnection::start_capture(&uid, Some(capture)) {
            Ok(connection) => {
                // Not fatal: an idle display legitimately sends nothing, and
                // the stream is still live and will deliver once something on
                // screen changes.
                if let Err(e) = connection.wait_for_warmup(Duration::from_secs(5)) {
                    eprintln!("stream-recorder: screen capture is quiet — {e:#}");
                }
                self.screen = Some(connection);
                self.report_clock_drift();
            }
            Err(e) => {
                // Leave the choice cleared rather than remembering a display
                // we could not open.
                self.screen_uid = None;
                eprintln!("stream-recorder: could not start screen capture: {e:#}");
            }
        }
    }

    /// Stop the screen stream, reporting what it wrote.
    pub(super) fn stop_screen(&mut self) {
        if let Some(screen) = self.screen.take() {
            if let Err(e) = screen.stop() {
                eprintln!("stream-recorder: error stopping screen capture: {e:#}");
            }
        }
    }

    /// The Router's handle on the screen stream, or `None` when no display is
    /// selected. Built fresh per chapter-open so the settings always match the
    /// stream that is actually running.
    pub(super) fn screen_track(&self) -> Result<Option<ScreenTrack>> {
        screen_track(self.screen.as_ref())
    }

    /// Point the screen dropdown at a display (or at nothing), start or stop
    /// the stream to match, and remember the choice.
    ///
    /// Like a camera or mic change, this deliberately leaves recording
    /// *stopped*: the chapter in progress is finished, and the next New
    /// Chapter press opens a pair of writers against the new arrangement.
    /// Swapping one of a chapter's two files mid-chapter would produce a
    /// screen file that starts partway through its camera counterpart.
    pub(super) fn select_screen(&mut self, screen_uid: Option<String>) -> Result<()> {
        if screen_uid == self.screen_uid {
            return Ok(());
        }

        // Close out the current chapter before the file set changes under it.
        if let Some(router) = &mut self.router {
            self.next_chapter = router.current_chapter_number() + 1;
            router
                .stop()
                .context("finishing chapter for screen switch")?;
        }
        self.router = None;

        self.stop_screen();
        // The overlay is pinned to one display's geometry, so it cannot follow
        // a display change — drop it and let the next Show Regions press build
        // one for the new screen.
        if let Some(live) = self.live.as_mut() {
            live.overlay = None;
        }

        self.screen_uid = screen_uid;
        // Regions are expressed in the new display's point space, so they have
        // to be rebuilt before anything reads one.
        self.rebuild_regions();
        self.open_screen();
        self.sync_overlay();

        let mut cfg = crate::config::load();
        cfg.screen_display_id = self.screen_uid.clone();
        crate::config::save(&cfg)?;

        match self
            .screen_uid
            .as_ref()
            .and_then(|uid| self.displays.iter().find(|d| &d.uid == uid))
        {
            Some(display) => println!(
                "stream-recorder: capturing \"{}\" at {}×{} — press New Chapter to record chapter {}",
                display.name,
                self.screen.as_ref().map_or(0, |s| s.width),
                self.screen.as_ref().map_or(0, |s| s.height),
                self.next_chapter,
            ),
            None => println!("stream-recorder: screen capture off"),
        }
        self.sync_controls();
        self.install_preview();
        Ok(())
    }
}

/// The Router's handle on a running screen stream.
///
/// A free function rather than only a method because
/// [`Router::reopen_with_screen`] needs it from inside a closure that has
/// already borrowed the connection mutably, so `&self` is not available.
pub(super) fn screen_track(
    screen: Option<&screen_stream::ScreenConnection>,
) -> Result<Option<ScreenTrack>> {
    let Some(screen) = screen else {
        return Ok(None);
    };
    let clock = screen
        .sync_clock()
        .context("the screen stream exposes no synchronization clock")?;
    Ok(Some(ScreenTrack {
        state: screen.delegate.state_arc(),
        delegate: screen.delegate.clone(),
        settings: screen_writer::video_settings(screen.width, screen.height)?,
        clock,
        width: screen.width,
        height: screen.height,
    }))
}
