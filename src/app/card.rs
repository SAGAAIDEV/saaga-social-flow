//! Drawing the procedural thumbnail, from the button to the candidate list.
//!
//! Two hops, and only one of them is asynchronous.
//!
//! 1. **`bun` runs on the calling thread.** It starts in about thirty
//!    milliseconds and hands back a page, so a worker would buy an async hop and
//!    a way for the button and the picture to disagree about which text was
//!    drawn. See [`crate::card::render`].
//! 2. **WebKit is asynchronous and has to be.** The page loads, the photograph is
//!    read and decoded, and only then is there anything to photograph — so the
//!    web view posts down the channel the App already drains on its tick, the
//!    same shape as every other worker in this crate.
//!
//! The [`Raster`](crate::card::raster::Raster) handle is held on the App for the
//! whole of step 2 and dropped when the event arrives. That is load-bearing:
//! `WKWebView` holds its navigation delegate *weakly*, so a handle dropped early
//! is a snapshot that never fires and a button that never comes back.

use crate::card::{self, raster::RasterEvent};

use super::App;

impl App {
    /// Render all destination formats from one frozen photo and design.
    ///
    /// Returns whether a job started. A card with no title of its own is drawn
    /// with the video's — see [`crate::video_brief::card`].
    pub(super) fn draw_card(&mut self) -> bool {
        // A commit prunes the set an upload may be reading from.
        if self.publish_busy || self.blog_busy || self.distribute_busy {
            self.set_thumbnail_status(
                "An upload is reading the current artwork — draw again when it finishes.",
            );
            return false;
        }
        if self.card_pending.is_some() || self.card_raster.is_some() {
            self.set_thumbnail_status("Already generating artwork…");
            return false;
        }
        let root = self.session.root.clone();
        let design = crate::video_brief::card(&self.session);
        // A title borrowed from the video goes into `card.json` before it is
        // drawn: the set's freshness is checked against that file, and a set
        // drawn with a title the card does not hold would land already stale.
        if design != card::load(&root) {
            if let Err(err) = card::save(&root, &design) {
                self.set_thumbnail_status(&format!("Artwork not drawn: {err:#}"));
                return false;
            }
        }
        match card::assets::Job::new(&root, design) {
            Ok(job) => {
                self.card_pending = Some(job);
                self.draw_next_asset();
                // Approve goes off for the length of the draw: the set it
                // would approve is about to be replaced.
                self.update_thumbnail_view();
                self.card_pending.is_some()
            }
            Err(err) => {
                self.set_thumbnail_status(&format!("Artwork not drawn: {err:#}"));
                false
            }
        }
    }

    fn draw_next_asset(&mut self) {
        let result = (|| -> anyhow::Result<_> {
            let job = self
                .card_pending
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no artwork job"))?;
            let mtm = objc2::MainThreadMarker::new()
                .ok_or_else(|| anyhow::anyhow!("render must run on the main thread"))?;
            let kind = job.kind();
            self.set_thumbnail_status(&format!(
                "Drawing {} artwork ({} of {})…",
                kind.name(),
                job.next + 1,
                card::assets::Kind::ALL.len()
            ));
            let size = kind.size();
            let html =
                card::render::html(&job.root, &job.design(), Some(&job.photo), size.0, size.1)?;
            std::fs::write(job.page(), html)?;
            card::raster::Raster::draw(
                mtm,
                &job.page(),
                &job.root,
                size,
                size.0.max(size.1) as f64,
                self.card_tx.clone(),
            )
        })();
        match result {
            Ok(raster) => self.card_raster = Some(raster),
            Err(err) => {
                self.card_pending = None;
                self.set_thumbnail_status(&format!("Artwork failed: {err:#}"));
            }
        }
        self.sync_controls();
    }

    pub(super) fn drain_card(&mut self) {
        let events = self.card_rx.try_iter().collect::<Vec<_>>();
        if events.is_empty() {
            return;
        }
        // A set is three pictures, so most events here are one picture landing
        // and the next starting. Only the commit — or a failure — is an outcome.
        for event in events {
            self.card_raster = None;
            let Some(mut job) = self.card_pending.take() else {
                continue;
            };
            match event {
                RasterEvent::Drawn { jpeg } => match job.accept(&jpeg) {
                    Ok(false) => {
                        self.card_pending = Some(job);
                        self.draw_next_asset();
                    }
                    Ok(true) => match job.commit() {
                        Ok(()) => {
                            self.set_thumbnail_status(
                                "Artwork drawn: horizontal, vertical and OG. Review it and press \
                                 Approve — nothing is published until you do.",
                            );
                        }
                        Err(err) => {
                            self.set_thumbnail_status(&format!("Could not save artwork: {err:#}"));
                        }
                    },
                    Err(err) => {
                        self.set_thumbnail_status(&format!("Could not save artwork: {err:#}"));
                    }
                },
                RasterEvent::Failed(message) => {
                    self.set_thumbnail_status(&format!("Artwork failed: {message}"));
                }
            }
        }
        self.update_video_view();
        self.update_thumbnail_view();
        self.update_publish_summary();
        self.update_blog_view();
        self.update_distribute_summary();
        self.sync_controls();
    }

    /// Saves the edited card and redraws nothing.
    ///
    /// Saving and drawing are two presses on purpose. The title box is typed
    /// into a word at a time, and a save that also drew would put a `bun` run
    /// and a WebKit snapshot behind every one of them.
    pub(super) fn save_card(
        &mut self,
        fields: &std::collections::BTreeMap<String, String>,
    ) -> bool {
        let root = self.session.root.clone();
        let mut card = card::load(&root);
        if let Some(title) = fields.get("title") {
            card.title = title.clone();
            card.own_title = crate::video_brief::is_own_title(&self.session, title);
        }
        if let Some(description) = fields.get("description") {
            card.description = description.clone();
        }
        if let Some(kicker) = fields.get("kicker") {
            card.kicker = kicker.clone();
        }
        if let Some(format) = fields.get("format") {
            if let Ok(value) = serde_json::from_value(serde_json::json!(format)) {
                card.format = value;
            }
        }
        if let Some(theme) = fields.get("theme") {
            card.theme = theme.clone();
        }
        // A field that will not parse leaves the stored value alone rather than
        // resetting the framing to centre, which is the one edit here that is
        // tedious to redo.
        let nudged = |field: &str| {
            fields
                .get(field)
                .and_then(|text| text.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0))
        };
        if let Some(value) = nudged("focus") {
            card.focus = value;
        }
        if let Some(value) = nudged("focus_y") {
            card.focus_y = value;
        }
        let saved = match card::save(&root, &card) {
            Ok(_) => {
                self.set_thumbnail_status("Design saved. Redraw artwork to draw it.");
                true
            }
            Err(err) => {
                self.set_thumbnail_status(&format!("Could not save the card: {err:#}"));
                false
            }
        };
        self.update_video_view();
        self.update_thumbnail_view();
        saved
    }
}
