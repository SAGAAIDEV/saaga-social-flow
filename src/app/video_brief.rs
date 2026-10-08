//! The video's title and description, written from the transcript.
//!
//! The details pane holds one thing the author types — notes to steer the copy,
//! remembered per project — and shows what Write last wrote. Write is a button
//! on Video details and on the YouTube tab; both start the one job, read the
//! saved notes, and report on both tabs' status lines.

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
    /// The project the job was started in; a result is only shown there.
    pub(super) session: crate::session::Session,
    brief: Brief,
    rx: Receiver<Result<Metadata, String>>,
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
    /// Says what the copy step is doing, and repaints the pane it writes into.
    ///
    /// The line is the native one above the pane rather than text in the page,
    /// so a progress message never costs the notes box its focus; the repaint
    /// is for the two states the page itself draws differently — the notes
    /// locked while a generation runs, and the new copy when it lands.
    pub(super) fn update_video_brief(&self, status: &str) {
        self.set_video_details_status(status);
        self.update_video_view();
    }
    /// [`Self::update_video_brief`] for Write, which is pressed on the YouTube
    /// tab too: that tab has its own line, and a press there answered one tab
    /// over looked like nothing happened.
    fn report_copy(&self, status: &str) {
        self.update_video_brief(status);
        self.set_publish_status(status);
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
        self.start_video_copy();
    }
    /// Whether a generation is now running.
    fn start_video_copy(&mut self) -> bool {
        if self.video_copy_job.is_some() {
            self.report_copy("Copy generation is already running. Wait for it to finish.");
            return false;
        }
        let brief = video_brief::load(&self.session);
        let source = video_brief::Source::for_session(&self.session, &brief.notes);
        if let Err(err) = source.prompt() {
            self.report_copy(&format!("{err:#}"));
            return false;
        }
        // The render used to guarantee this; a button does not.
        if source.completed < source.total {
            self.report_copy(&format!(
                "Wait for every chapter to transcribe first — {} of {} are done.",
                source.completed, source.total
            ));
            return false;
        }
        let status = source.status();
        let model = self.notes_pick.model().to_string();
        let provider = self.notes_pick.provider().map(str::to_string);
        // The team's footer ends every generated description — see
        // `crate::team`. As last read: a save made since is on screen already.
        let team = self.team_template();
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("video-copy".into())
            .spawn(move || {
                let result = video_brief::generate(&source, &model, provider.as_deref())
                    .map(|metadata| crate::publish::metadata::Metadata {
                        description: crate::team::with_footer(&metadata.description, &team),
                        ..metadata
                    })
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            }) {
            Ok(_) => {
                self.video_copy_job = Some(CopyJob {
                    session: self.session.clone(),
                    brief,
                    rx,
                });
                // The YouTube tab's Write button reads "Writing…" while it runs.
                self.update_publish_summary();
                self.report_copy(&status);
                true
            }
            Err(err) => {
                self.report_copy(&format!("Could not start generation: {err}"));
                false
            }
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
        let here = job.session.root == self.session.root;
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
                    Ok(true) => {
                        // Write replaces the copy, so edits not yet saved on
                        // the YouTube tab go with it — otherwise the tab kept
                        // showing them over the copy just written.
                        if here {
                            self.youtube_draft = None;
                        }
                        let written = note.unwrap_or_else(|| "Title and description written, and shared with the artwork and YouTube.".into());
                        // Saved here, not sent: the video already up keeps its
                        // copy until Save video details pushes the new one.
                        match crate::publish::longform(&job.session) {
                            Some(_) => format!("{written} The video on YouTube still has the old copy — press Save video details to change it there."),
                            None => written,
                        }
                    }
                    Ok(false) => "Copy saved, but needs a valid title before it can update the artwork and YouTube.".into(),
                    Err(err) => format!("Could not save generated copy: {err:#}"),
                }
            }
            Err(err) => {
                format!("Could not write the title and description: {err}. Your notes and previous copy are saved.")
            }
        };
        // A result always belongs to the project it started in.
        if here {
            // The tab shows the title the artwork is drawn for.
            self.update_thumbnail_view();
            self.update_publish_summary();
            self.update_blog_view();
            self.sync_controls();
            // Last: repainting the YouTube tab resets its status line.
            self.report_copy(&status);
        }
    }
}
