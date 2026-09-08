//! The video's title and description, written by the render.
//!
//! The details pane holds one thing the author types — notes to steer the copy,
//! remembered per project — and shows what the last render wrote. Generation
//! is not a button: a render has already waited for every chapter's transcript,
//! so the moment it finishes is the moment the copy can be written, and
//! [`App::write_copy_after_render`] does exactly that. The one thing it will not
//! do is overwrite copy edited by hand on the YouTube tab — see that method.

use super::App;
use crate::{
    publish::metadata::Metadata,
    video_brief::{self, Brief},
};
use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver, TryRecvError},
};

pub(super) struct CopyJob {
    session: crate::session::Session,
    brief: Brief,
    rx: Receiver<Result<Metadata, String>>,
    /// Started by a render finishing rather than by hand, so the render's own
    /// status line is told how it went.
    from_render: bool,
}
impl App {
    /// The notes typed on the pane, if the event is for the project on screen.
    fn notes_from_fields(&self, fields: &BTreeMap<String, String>) -> Option<String> {
        // Ignore events from a page that was replaced while changing projects.
        if fields.get("root").map(String::as_str) != self.session.root.to_str() {
            return None;
        }
        Some(fields.get("notes").cloned().unwrap_or_default())
    }
    pub(super) fn update_video_brief(&self, status: &str) {
        let Some(live) = &self.live else {
            return;
        };
        let busy = self
            .video_copy_job
            .as_ref()
            .is_some_and(|job| job.session.root == self.session.root);
        live.video_brief_pane.show_local(
            &crate::ui::render::page("video-brief.html", minijinja::context! {
                brief => video_brief::load(&self.session), root => self.session.root.to_string_lossy(),
                status => status, busy => busy, model => self.notes_pick.model(),
            }), &self.session.root, &self.session.root, ".video-brief.html",
        );
    }
    /// Remember the notes. `_apply` is kept for the event's shape; notes never
    /// reach thumbnails or YouTube on their own, so there is nothing to apply.
    pub(super) fn save_video_brief(&mut self, fields: &BTreeMap<String, String>, _apply: bool) {
        let Some(notes) = self.notes_from_fields(fields) else {
            return;
        };
        if self
            .video_copy_job
            .as_ref()
            .is_some_and(|job| job.session.root == self.session.root)
        {
            return;
        }
        let mut brief = video_brief::load(&self.session);
        brief.notes = notes;
        // Autosave never reloads the editor while the user is typing, so only a
        // failure is worth repainting for.
        if let Err(err) = video_brief::save(&self.session.root, &brief) {
            self.update_video_brief(&format!("Could not save notes: {err:#}"));
        }
    }
    /// The pane's own request, kept for the event that still exists; the
    /// render is what normally asks.
    pub(super) fn generate_video_copy(&mut self, fields: &BTreeMap<String, String>) {
        if self.notes_from_fields(fields).is_none() {
            return;
        }
        self.start_video_copy(false);
    }
    /// A render just finished, so every chapter has transcribed: write the
    /// title and description now, unless someone already wrote their own.
    ///
    /// The copy the last generation produced is what `video-brief.json` holds,
    /// and what YouTube will use is `youtube-metadata.json`. The YouTube tab
    /// saves edits to the latter alone, so the two disagreeing means a person
    /// chose different words — and a re-render, which happens after every small
    /// cut, must not undo that.
    pub(super) fn write_copy_after_render(&mut self) {
        let brief = video_brief::load(&self.session);
        let published = crate::publish::metadata::load(&self.session);
        if !brief.title.trim().is_empty() && published != brief.metadata() {
            self.update_video_brief(
                "Kept the title and description edited on the YouTube tab; the render did not rewrite them.",
            );
            return;
        }
        self.start_video_copy(true);
    }
    fn start_video_copy(&mut self, from_render: bool) {
        if self.video_copy_job.is_some() {
            self.update_video_brief("Copy generation is already running. Wait for it to finish.");
            return;
        }
        let brief = video_brief::load(&self.session);
        let source = video_brief::Source::for_session(&self.session, &brief.notes);
        if let Err(err) = source.prompt() {
            self.update_video_brief(&format!("{err:#}"));
            return;
        }
        let status = source.status();
        let model = self.notes_pick.model().to_string();
        let provider = self.notes_pick.provider().map(str::to_string);
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("video-copy".into())
            .spawn(move || {
                let result = video_brief::generate(&source, &model, provider.as_deref())
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            }) {
            Ok(_) => {
                self.video_copy_job = Some(CopyJob {
                    session: self.session.clone(),
                    brief,
                    rx,
                    from_render,
                });
                self.update_video_brief(&status);
                if from_render {
                    self.set_render_status("Render ready — writing the title and description…");
                }
            }
            Err(err) => self.update_video_brief(&format!("Could not start generation: {err}")),
        }
    }
    pub(super) fn drain_video_copy(&mut self) {
        let Some(job) = self.video_copy_job.as_ref() else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("Copy generation stopped unexpectedly. Try again.".into())
            }
        };
        let job = self.video_copy_job.take().expect("active job");
        let status = match result {
            Ok(metadata) => {
                // Over the artwork limits is a note, not a failure — the copy is
                // saved either way and trimming it here is quicker than another
                // model call.
                let note = video_brief::artwork_note(&metadata);
                let brief = Brief {
                    notes: job.brief.notes,
                    title: metadata.title,
                    description: metadata.description,
                };
                match video_brief::sync(&job.session, &brief) {
                    Ok(true) => note.unwrap_or_else(|| "Title and description written, and shared with thumbnails and YouTube. Generate a fresh artwork set in Thumbnails.".into()),
                    Ok(false) => "Copy saved, but needs a valid title before it can update thumbnails and YouTube.".into(),
                    Err(err) => format!("Could not save generated copy: {err:#}"),
                }
            }
            Err(err) => {
                format!("Could not write the title and description: {err}. Your notes and previous copy are saved.")
            }
        };
        // A result always belongs to the project it started in.
        if job.session.root == self.session.root {
            self.update_video_brief(&status);
            if job.from_render {
                self.set_render_status(&format!("Render ready — {status}"));
            }
            self.update_thumbnail_view();
            self.update_publish_summary();
            self.update_blog_view();
            self.sync_controls();
        }
    }
}
