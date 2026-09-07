//! The figure gesture, from the chord to the ledger row.
//!
//! Split out of [`super`] for the same reason as [`super::framing`]: it is the
//! one part of the app that reasons about a screen rectangle and a moment
//! together, and it has a window of its own to keep alive.
//!
//! ## The order of operations matters
//!
//! ⌃⇧S shows the overlay. A drag posts [`crate::ui::UiEvent::FigureSnipped`],
//! and this is
//! what happens next, in this order:
//!
//! 1. **The overlay comes down.** First, before anything else, because it is
//!    dimming the display and eating every click on it. If the shutter failed
//!    and the window stayed up, the machine would be unusable.
//! 2. **The ledger number is read off disk.** Not counted in memory: a restart
//!    mid-session would restart the count and two figures would land on one
//!    filename.
//! 3. **The shutter is fired and forgotten.** It answers on a ScreenCaptureKit
//!    queue and posts back down the channel — see [`crate::figure::shot`] for
//!    why it must not be waited on here.
//! 4. **The row is appended when the file exists**, in [`App::drain_figure`],
//!    on the main thread. A ledger row whose JPEG failed to write would be a
//!    caption with no picture, which is worse than no figure at all.
//! 5. **The take pauses and the aside starts.** The chapter's writers stay
//!    installed and stop appending — see [`crate::capture::pause`] — and the
//!    microphone goes to `figures/figure-NN.m4a` instead, so the author can say
//!    what the picture shows. The same chord ends the break: the aside is
//!    finished, its row appended, its transcript started, and the take picks up
//!    where it left off with the break absent from the file. Every path that
//!    finishes or cuts a chapter ends a break first — see [`App::end_break`].
//!
//! The moment the figure records is read before any of this, so it is the
//! position in the *file*, which is the only place it can be looked up later.

use crate::figure::aside::Aside;
use crate::figure::{self, Capture, FigureEvent};
use crate::region::{DisplayGeometry, PointRect};

use super::App;

/// A figure being explained: the aside recording it, and what the chord has to
/// pick back up when it ends.
pub(super) struct Break {
    aside: Aside,
    /// The figure's number, which names both its image and its audio.
    figure: u32,
    /// Whether a chapter was open — and so paused — when the break began. A
    /// figure taken between takes has nothing to resume.
    paused: bool,
}

impl App {
    /// ⌃⇧S: show the snip overlay, or take it down if it is already up.
    ///
    /// Toggling matters more than it looks. The chord is pressed while looking
    /// at another app, so the overlay comes up over whatever has focus — and
    /// since it cannot be dismissed with Escape (see [`crate::figure::snip`]),
    /// the same chord that opened it has to close it.
    pub(super) fn capture_figure(&mut self) {
        // On a break, the chord is the way back — see the module docs.
        if self.aside.is_some() {
            self.end_break(true);
            return;
        }
        if self.snip_is_up() {
            self.hide_snip();
            self.set_figure_status("Figure cancelled.");
            return;
        }
        let geometry = match self.figure_display() {
            Ok(geometry) => geometry,
            Err(err) => {
                self.set_figure_status(&format!("No display to capture: {err:#}"));
                return;
            }
        };
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let Some(live) = self.live.as_mut() else { return };

        // Rebuilt whenever the display it was made for is no longer the one to
        // snip: the window is sized and positioned for one display's geometry,
        // and a stale one would dim the wrong screen.
        let stale = live
            .snip
            .as_ref()
            .is_none_or(|snip| snip.geometry() != geometry);
        if stale {
            live.snip = Some(figure::snip::Snip::new(mtm, geometry, live.ui_tx.clone()));
        }
        if let Some(snip) = live.snip.as_mut() {
            snip.show();
        }
        self.set_figure_status("Drag a rectangle to capture a figure.");

        // The snip window is `NSWindowSharingType::None`, but the running
        // stream's content filter was built from an `SCShareableContent`
        // snapshot taken before this window existed — the same trap
        // `toggle_regions` documents. Without this the dim would appear in the
        // recording on the frame it is shown.
        if let Some(screen) = self.screen.as_ref() {
            if let Err(e) = screen.refresh_exclusions() {
                eprintln!("stream-recorder: could not exclude the snip from capture: {e:#}");
            }
        }
    }

    /// A rectangle was drawn. See the module docs for why this order.
    pub(super) fn figure_snipped(&mut self, rect: PointRect) {
        let geometry = self.snip_geometry();
        self.hide_snip();
        let Some(geometry) = geometry else {
            // Only reachable if the window went away between the drag and this
            // tick, which would mean the rect belongs to a display we can no
            // longer name.
            self.set_figure_status("Lost track of the display that was being snipped.");
            return;
        };

        let n = figure::next_n(&self.session.root);
        let (chapter, offset) = match self.clock.position() {
            Some((chapter, offset)) => (Some(chapter), Some(offset)),
            None => (None, None),
        };
        let capture = Capture {
            n,
            file: figure::path_for(&self.session.root, n),
            chapter,
            offset,
            rect: rect.into(),
            // Filled in by the shutter from the bytes it writes.
            width: 0,
            height: 0,
        };
        let scale = geometry.scale();
        self.set_figure_status(&format!(
            "Capturing figure {n:02} — {} × {}…",
            (rect.w * scale).round() as i64,
            (rect.h * scale).round() as i64,
        ));
        figure::shot::capture(
            rect.to_cg_global(&geometry),
            capture,
            self.figure_tx.clone(),
        );
        self.begin_break(n);
    }

    /// Pause the take, if one is rolling, and start recording the explanation.
    fn begin_break(&mut self, n: u32) {
        let paused = match self.router.as_ref().map(|router| router.pause_chapter()) {
            Some(Ok(())) => {
                self.clock.pause();
                true
            }
            Some(Err(err)) => {
                // Without the pause the explanation would land in the chapter
                // too. Better no aside than the same words in two files.
                self.set_figure_status(&format!(
                    "Figure {n:02} captured, but the take could not pause: {err:#}"
                ));
                return;
            }
            None => false,
        };
        let Some(conn) = self.connection.as_ref() else {
            self.set_figure_status(&format!(
                "Figure {n:02} captured — no microphone to explain it into."
            ));
            self.resume_take(paused);
            return;
        };
        match Aside::start(conn, &figure::audio_path_for(&self.session.root, n)) {
            Ok(aside) => {
                self.aside = Some(Break {
                    aside,
                    figure: n,
                    paused,
                });
                self.set_figure_status(&format!(
                    "Figure {n:02} — say what it shows, then ⌃⇧S to pick the take back up."
                ));
            }
            Err(err) => {
                self.set_figure_status(&format!(
                    "Figure {n:02} captured, but its explanation is not recording: {err:#}"
                ));
                self.resume_take(paused);
            }
        }
    }

    /// End the break, if one is on. `resume` picks the take back up where it
    /// paused; without it the chapter stays paused for the caller to finish,
    /// which closes the file at the pause point.
    pub(super) fn end_break(&mut self, resume: bool) {
        let Some(Break {
            aside,
            figure,
            paused,
        }) = self.aside.take()
        else {
            return;
        };
        let image = figure::path_for(&self.session.root, figure);
        match aside.finish() {
            Ok(finished) => {
                let secs = finished.duration.as_secs();
                match figure::append_aside(&self.session.root, &image, &finished.path) {
                    Ok(()) => self.set_figure_status(&format!(
                        "Figure {figure:02} explained ({}:{:02}).",
                        secs / 60,
                        secs % 60
                    )),
                    Err(err) => self.set_figure_status(&format!(
                        "Figure {figure:02}'s explanation was recorded but not filed: {err:#}"
                    )),
                }
                // The words are what the blurb is written from, so the upload
                // starts now rather than when someone presses a button — and
                // the blurb follows on its own once they land.
                crate::notes::spawn_chapter_transcript(finished.path);
                figure::blurb::auto::spawn(
                    self.session.clone(),
                    figure,
                    self.posts_pick.model().to_string(),
                    self.posts_pick.provider().map(str::to_string),
                    self.figure_tx.clone(),
                );
            }
            Err(err) => self.set_figure_status(&format!(
                "Figure {figure:02}'s explanation was not saved: {err:#}"
            )),
        }
        if resume {
            self.resume_take(paused);
        }
    }

    /// Pick the chapter back up after a break, if there was one to pause.
    fn resume_take(&mut self, paused: bool) {
        if !paused {
            return;
        }
        if let Some(router) = self.router.as_ref() {
            if let Err(err) = router.resume_chapter() {
                eprintln!("stream-recorder: could not resume the take: {err:#}");
            }
        }
        self.clock.resume();
    }

    pub(super) fn figure_snip_cancelled(&mut self) {
        self.hide_snip();
        self.set_figure_status("Figure cancelled.");
    }

    /// Write a blurb for every figure that has none.
    pub(super) fn write_blurbs(&mut self) {
        if self.figure_busy {
            self.set_figure_status("Already writing blurbs…");
            return;
        }
        let left = figure::unwritten(&self.session.root);
        if left.is_empty() {
            // Two different nothings, and the operator needs to know which.
            self.set_figure_status(match figure::load(&self.session.root).is_empty() {
                true => "No figures yet — press ⌃⇧S over the screen and drag one.",
                false => "Every figure already has a blurb.",
            });
            return;
        }
        self.figure_busy = true;
        self.set_figure_status(&format!("Writing {} blurb(s)…", left.len()));
        figure::blurb::spawn(
            self.session.clone(),
            // The Post tab's pick, the same model the article is written with:
            // a caption in a different voice from the prose around it reads as
            // a caption from somewhere else.
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            self.figure_tx.clone(),
        );
        self.sync_controls();
    }

    pub(super) fn drain_figure(&mut self) {
        let events: Vec<_> = self.figure_rx.try_iter().collect();
        if events.is_empty() {
            return;
        }
        let mut repaint = false;
        for event in events {
            match event {
                FigureEvent::Captured(capture) => {
                    let n = capture.n;
                    // The row goes down only now that the JPEG exists — see the
                    // module docs.
                    // The shutter answers after the break has begun, so this
                    // must not talk over the instruction the break just gave.
                    let hint = if self.aside.as_ref().is_some_and(|b| b.figure == n) {
                        "Say what it shows, then ⌃⇧S to pick the take back up."
                    } else {
                        "Its blurb follows once the explanation has transcribed."
                    };
                    match figure::append_capture(&self.session.root, &capture) {
                        Ok(()) => self.set_figure_status(&format!(
                            "Figure {n:02} captured. {hint}"
                        )),
                        Err(err) => self.set_figure_status(&format!(
                            "Figure {n:02} was captured but not recorded: {err:#}"
                        )),
                    }
                    repaint = true;
                }
                FigureEvent::Failed(message) => {
                    self.set_figure_status(&format!("Capture failed: {message}"));
                }
                FigureEvent::Status(message) => self.set_figure_status(&message),
                FigureEvent::Blurbs { written, failures } => {
                    self.figure_busy = false;
                    self.set_figure_status(&blurb_summary(written, &failures));
                    repaint = true;
                }
                FigureEvent::Explained {
                    n,
                    explained,
                    failure,
                } => {
                    self.set_figure_status(&match failure {
                        Some(reason) => format!("Figure {n:02}: no blurb yet — {reason}"),
                        None if explained => {
                            format!("Figure {n:02}'s blurb is written from your explanation.")
                        }
                        None => format!(
                            "Figure {n:02}'s blurb is written from the picture alone — \
                             no explanation was heard."
                        ),
                    });
                    repaint = true;
                }
            }
        }
        if repaint {
            self.update_blog_view();
            self.sync_controls();
        }
    }

    /// Figures are the Blog tab's business, so they report there.
    pub(super) fn set_figure_status(&self, message: &str) {
        self.set_blog_status(message);
    }

    fn snip_is_up(&self) -> bool {
        self.live
            .as_ref()
            .and_then(|live| live.snip.as_ref())
            .is_some_and(|snip| snip.is_visible())
    }

    /// The geometry the live snip window was built for, which is the only one
    /// the rect it posted can be interpreted against.
    fn snip_geometry(&self) -> Option<DisplayGeometry> {
        self.live
            .as_ref()
            .and_then(|live| live.snip.as_ref())
            .map(|snip| snip.geometry())
    }

    fn hide_snip(&mut self) {
        if let Some(snip) = self.live.as_mut().and_then(|live| live.snip.as_mut()) {
            snip.hide();
        }
    }

    /// Which display a figure comes from: the one being recorded, or the main
    /// one when no screen is selected.
    ///
    /// The fallback is what makes this work on a talking-head layout, where
    /// there is no screen capture at all and `geometry` is `None` — a figure is
    /// still worth taking, and refusing one because the *recording* is not
    /// using the screen would be a strange rule to explain.
    fn figure_display(&self) -> anyhow::Result<DisplayGeometry> {
        if let Some(geometry) = self.geometry {
            return Ok(geometry);
        }
        let main = objc2_core_graphics::CGMainDisplayID();
        crate::capture::screen::display_geometry(&main.to_string())
    }
}

/// One line for a finished pass, naming what failed rather than only counting.
///
/// A pass that wrote three of five and says "3 written" leaves the operator to
/// find the other two by eye. Naming them is the difference between pressing
/// the button again and wondering whether it is worth it.
fn blurb_summary(written: usize, failures: &[String]) -> String {
    match (written, failures.len()) {
        (0, 0) => "Nothing left to write about.".to_string(),
        (written, 0) => format!("{written} blurb(s) written."),
        (0, _) => format!("No blurbs written — {}", failures.join("; ")),
        (written, _) => format!(
            "{written} blurb(s) written, {} failed — {}",
            failures.len(),
            failures.join("; ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_pass_just_counts() {
        assert_eq!(blurb_summary(3, &[]), "3 blurb(s) written.");
        assert_eq!(blurb_summary(0, &[]), "Nothing left to write about.");
    }

    /// The whole point of the summary: a partial pass has to name what it
    /// missed, because nothing else will.
    #[test]
    fn a_partial_pass_names_what_failed() {
        let failures = vec!["figure 02: no credit".to_string()];
        let line = blurb_summary(2, &failures);
        assert!(line.contains("2 blurb(s) written"), "{line}");
        assert!(line.contains("1 failed"), "{line}");
        assert!(line.contains("figure 02: no credit"), "{line}");
    }

    #[test]
    fn a_pass_that_wrote_nothing_leads_with_the_reason() {
        let line = blurb_summary(0, &["figure 01: OPENROUTER_API_KEY unset".to_string()]);
        assert!(line.starts_with("No blurbs written"), "{line}");
        assert!(line.contains("OPENROUTER_API_KEY"), "{line}");
    }
}
