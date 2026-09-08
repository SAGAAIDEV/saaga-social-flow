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
    /// Returns whether a job started. The render chain reads it: a refusal
    /// here — no title, no photo — is the end of that chain, not a step in it.
    pub(super) fn draw_card(&mut self) -> bool {
        if self.card_pending.is_some() || self.card_raster.is_some() {
            self.set_thumbnail_status("Already generating artwork…");
            return false;
        }
        match card::assets::Job::new(&self.session.root, card::load(&self.session.root)) {
            Ok(job) => { self.card_pending = Some(job); self.draw_next_asset(); self.card_pending.is_some() }
            Err(err) => { self.set_thumbnail_status(&format!("Artwork not drawn: {err:#}")); false }
        }
    }

    fn draw_next_asset(&mut self) {
        let result = (|| -> anyhow::Result<_> {
            let job = self.card_pending.as_ref().ok_or_else(|| anyhow::anyhow!("no artwork job"))?;
            let mtm = objc2::MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("render must run on the main thread"))?;
            let kind = job.kind();
            self.set_thumbnail_status(&format!("Drawing {} artwork…", kind.name()));
            let size = kind.size();
            let html = card::render::html(&job.root, &job.design(), Some(&job.photo), size.0, size.1)?;
            std::fs::write(job.page(), html)?;
            card::raster::Raster::draw(mtm, &job.page(), &job.root, size, size.0.max(size.1) as f64, self.card_tx.clone())
        })();
        match result {
            Ok(raster) => self.card_raster = Some(raster),
            Err(err) => { self.card_pending = None; self.set_thumbnail_status(&format!("Artwork failed: {err:#}")); }
        }
        self.sync_controls();
    }

    pub(super) fn drain_card(&mut self) {
        let events = self.card_rx.try_iter().collect::<Vec<_>>();
        if events.is_empty() { return; }
        // A set is three pictures, so most events here are one picture landing
        // and the next starting. Only the commit — or a failure — is an outcome.
        let mut committed = false;
        let mut failed = false;
        for event in events {
            self.card_raster = None;
            let Some(mut job) = self.card_pending.take() else { continue };
            match event {
                RasterEvent::Drawn { jpeg } => match job.accept(&jpeg) {
                    Ok(false) => { self.card_pending = Some(job); self.draw_next_asset(); failed = self.card_pending.is_none(); }
                    Ok(true) => match job.commit() {
                        Ok(()) => { committed = true; self.set_thumbnail_status("Artwork ready: horizontal, vertical and OG. YouTube, blog and social exports use this set."); }
                        Err(err) => { failed = true; self.set_thumbnail_status(&format!("Could not save artwork: {err:#}")); }
                    },
                    Err(err) => { failed = true; self.set_thumbnail_status(&format!("Could not save artwork: {err:#}")); }
                },
                RasterEvent::Failed(message) => { failed = true; self.set_thumbnail_status(&format!("Artwork failed: {message}")); }
            }
        }
        self.update_video_view();
        self.update_publish_summary();
        self.update_blog_view();
        self.sync_controls();
        if committed {
            self.finish_pipeline_with_upload();
        } else if failed {
            self.pipeline = false;
        }
    }

    /// Saves the edited card and redraws nothing.
    ///
    /// Saving and drawing are two presses on purpose. The title box is typed
    /// into a word at a time, and a save that also drew would put a `bun` run
    /// and a WebKit snapshot behind every one of them.
    pub(super) fn save_card(&mut self, fields: &std::collections::BTreeMap<String, String>) -> bool {
        let root = self.session.root.clone();
        let mut card = card::load(&root);
        if let Some(title) = fields.get("title") {
            card.title = title.clone();
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
        if let Some(focus) = fields.get("focus") {
            // A field that will not parse leaves the stored value alone rather
            // than resetting the framing to centre, which is the one edit here
            // that is tedious to redo.
            if let Ok(value) = focus.trim().parse::<f64>() {
                if value.is_finite() {
                    card.focus = value.clamp(0.0, 1.0);
                }
            }
        }
        let saved = match card::save(&root, &card) {
            Ok(_) => { self.set_thumbnail_status("Design saved. Redraw artwork to draw it."); true }
            Err(err) => { self.set_thumbnail_status(&format!("Could not save the card: {err:#}")); false }
        };
        self.update_video_view();
        saved
    }
}
