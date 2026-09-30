//! The Summary card on Video details: written when a render finishes, and on
//! Summarize again — see [`crate::summary`].
//!
//! Beside the render chain rather than in it. The chain waits on the title and
//! description because the artwork is drawn from them; nothing waits on the
//! summary, so a slow model call here holds up neither the artwork nor the
//! upload.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::App;
use crate::summary::{self, Summary};

pub(super) struct SummaryJob {
    /// The project and version it was started for; a result is saved there by
    /// the job itself and only shown if that is still what is open.
    session: crate::session::Session,
    rx: Receiver<Result<Summary, String>>,
}

/// What the card draws.
#[derive(serde::Serialize)]
pub(super) struct SummaryView {
    pub summary: Option<Summary>,
    /// The plain text the Copy button puts on the pasteboard.
    pub text: String,
    /// The cut has changed since it was written.
    pub stale: bool,
    pub busy: bool,
    /// Whether there is a transcript to summarize at all.
    pub ready: bool,
}

impl App {
    /// A render finished: summarize the video it made, unless the summary on
    /// disk was written from this same cut.
    pub(super) fn summarize_after_render(&mut self) {
        let chapters = summary::final_transcript(&self.session);
        if chapters.is_empty() {
            return;
        }
        let current = summary::load(&self.session)
            .is_some_and(|s| s.transcript_hash == summary::transcript_hash(&chapters));
        if !current {
            self.summarize_video();
        }
    }

    /// Summarize again, or the render's own request.
    pub(super) fn summarize_video(&mut self) {
        if self.summary_job.is_some() {
            self.update_video_brief("The summary is already being written — a moment.");
            return;
        }
        let session = self.session.clone();
        let model = self.notes_pick.model().to_string();
        let provider = self.notes_pick.provider().map(str::to_string);
        let (tx, rx) = mpsc::channel();
        let job_session = session.clone();
        match std::thread::Builder::new()
            .name("video-summary".into())
            .spawn(move || {
                let result = summary::generate(&job_session, &model, provider.as_deref())
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            }) {
            Ok(_) => {
                self.summary_job = Some(SummaryJob { session, rx });
                self.update_video_brief("Summarizing the final video…");
            }
            Err(err) => self.update_video_brief(&format!("Could not start the summary: {err}")),
        }
    }

    pub(super) fn drain_summary(&mut self) {
        let Some(job) = self.summary_job.as_ref() else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the summary stopped unexpectedly".into()),
        };
        let job = self.summary_job.take().expect("active job");
        // Saved by the job either way; shown only where it belongs.
        if job.session.root != self.session.root || job.session.version != self.session.version {
            return;
        }
        match result {
            Ok(_) => self.update_video_brief("Summary of the final video written."),
            Err(err) => self.update_video_brief(&format!("Could not summarize the video: {err}")),
        }
    }

    pub(super) fn summary_view(&self) -> SummaryView {
        let chapters = summary::final_transcript(&self.session);
        let saved = summary::load(&self.session);
        SummaryView {
            text: saved.as_ref().map(summary::plain_text).unwrap_or_default(),
            stale: saved.as_ref().is_some_and(|s| {
                !chapters.is_empty() && s.transcript_hash != summary::transcript_hash(&chapters)
            }),
            busy: self
                .summary_job
                .as_ref()
                .is_some_and(|job| job.session.root == self.session.root),
            ready: !chapters.is_empty(),
            summary: saved,
        }
    }
}
