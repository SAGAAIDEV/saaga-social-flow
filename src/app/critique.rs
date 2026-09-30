//! The Critique card on Video details — see [`crate::critique`].
//!
//! On a thread: two model calls. When it lands, the new plan is already
//! approved on disk, so everything that shows the plan is told: the
//! teleprompter and the next chapter's layout, the Plan tab, the Project tab.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::App;
use crate::critique::{self, Critique};

pub(super) struct CritiqueJob {
    session: crate::session::Session,
    rx: Receiver<Result<Critique, String>>,
}

/// What the card draws.
#[derive(serde::Serialize)]
pub(super) struct CritiqueView {
    pub critique: Option<Critique>,
    pub text: String,
    pub busy: bool,
    /// Why the button is off, when it is.
    pub blocked: Option<String>,
}

impl App {
    pub(super) fn critique_take(&mut self, direction: &str) {
        if self.critique_job.is_some() {
            self.update_video_brief("The critique is already being written — a moment.");
            return;
        }
        if self.router.is_some() {
            self.update_video_brief("Stop recording first — the open chapter has no words yet.");
            return;
        }
        if self.plan_building_somewhere() {
            self.update_video_brief(
                "A plan is being built on the Plan tab — critique once it lands, so the two do \
                 not write the same version.",
            );
            return;
        }
        if let Some(why) = critique::refusal(&self.session) {
            self.update_video_brief(&why);
            return;
        }
        let session = self.session.clone();
        let job_session = session.clone();
        let direction = direction.to_string();
        let model = self.notes_pick.model().to_string();
        let provider = self.notes_pick.provider().map(str::to_string);
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("take-critique".into())
            .spawn(move || {
                let result = critique::run(&job_session, &direction, &model, provider.as_deref())
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            }) {
            Ok(_) => {
                self.critique_job = Some(CritiqueJob { session, rx });
                self.update_video_brief(
                    "Critiquing every chapter, then rewriting the plan for the next take…",
                );
            }
            Err(err) => self.update_video_brief(&format!("Could not start the critique: {err}")),
        }
    }

    pub(super) fn drain_critique(&mut self) {
        let Some(job) = self.critique_job.as_ref() else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the critique stopped unexpectedly".into()),
        };
        let job = self.critique_job.take().expect("active job");
        if job.session.root != self.session.root {
            eprintln!("stream-recorder: a critique finished for another project");
            return;
        }
        match result {
            Ok(critique) => {
                let line = match critique.plan_written {
                    Some(n) => format!(
                        "Critique written, and Plan {n} — reorganized from it — is now the \
                         speaking notes. New Version records the next take."
                    ),
                    None => "Critique written.".into(),
                };
                // The plan changed under every view that shows it.
                self.recording_plan_changed();
                self.update_plan_view();
                self.update_project_view();
                self.update_video_brief(&line);
            }
            Err(err) => self.update_video_brief(&format!("Could not critique the take: {err}")),
        }
    }

    pub(super) fn critique_view(&self) -> CritiqueView {
        let saved = critique::load(&self.session);
        let busy = self
            .critique_job
            .as_ref()
            .is_some_and(|job| job.session.root == self.session.root);
        CritiqueView {
            text: saved.as_ref().map(critique::plain_text).unwrap_or_default(),
            blocked: (!busy).then(|| critique::refusal(&self.session)).flatten(),
            busy,
            critique: saved,
        }
    }
}
