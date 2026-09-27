//! The Plan tab's live half: idea takes on the microphone, and the build.
//!
//! Split out of [`super`] like [`super::figures`] and [`super::video_brief`]:
//! everything here is state the App holds for one tab — the take being
//! recorded, the job building a plan — and the handlers for the Plan pane's
//! messages. What a plan *is*, and every file it lives in, is
//! [`crate::plan`]'s; the pane is [`crate::ui::planning`].
//!
//! ## An idea take is a figure's aside without the figure
//!
//! Mic only, recorded by [`Aside`] — a second writer on the capture session
//! that is already running, never a second session (see
//! `docs/figure-aside-plan.md` for the static a cold one produced). So the
//! rules that keep a figure's aside safe keep a take safe too:
//!
//! - The delegate holds one aside at a time. A take and a figure's break
//!   cannot both record, and Record idea refuses during a break rather than
//!   meet that refusal as an error.
//! - A take and a chapter do not overlap. Record idea refuses while a chapter
//!   is recording, and Start Recording refuses while a take is — each says
//!   why on its own tab's status line. Both could technically run, but a take
//!   recorded mid-chapter would have the same words in two files.
//! - Every path that stops the capture session finishes an open take first
//!   ([`App::finish_plan_take`]): Quit, a device switch, and a project switch.
//!   An aside dropped unfinished leaves an `.m4a` with no trailer, which will
//!   not play and will not transcribe.
//!
//! ## The build waits, then calls the model once
//!
//! [`PlanJob`] is [`super::video_brief::CopyJob`]'s pattern: a thread, a
//! receiver polled from the App loop, and the project it started in, so a
//! result lands only where it was asked for. Before the call it waits for the
//! takes' transcripts — a take stopped a moment ago is still uploading — on
//! [`crate::plan::takes_still_running`], and for the chapters' when planning
//! from a rehearsal. One job at a time, across projects: two would race to the
//! next version number in the same folder.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::figure::aside::Aside;
use crate::plan::Plan;
use crate::session::Session;
use crate::ui::planning::{self, PlanLive};

use super::App;

/// How often the build re-reads its transcripts while waiting, and how often
/// the pane's take rows are checked for a transcript that has landed.
const POLL_EVERY: Duration = Duration::from_secs(1);

/// An idea take being recorded.
pub(super) struct PlanTake {
    aside: Aside,
    /// Names the file, `plan/take-NN.m4a`, and the row on the pane.
    n: u32,
    /// For the timer on the status line.
    started: Instant,
    /// The whole second the timer last painted, so it repaints once a second
    /// rather than on every tick.
    shown_secs: Option<u64>,
}

enum JobEvent {
    Status(String),
    /// Boxed: a plan is a few hundred bytes, and every status line would
    /// otherwise be sent at that size.
    Done(Result<Box<Plan>, String>),
}

/// A plan being built.
pub(super) struct PlanJob {
    /// The project it was started in; a result is only shown there.
    session: Session,
    rx: Receiver<JobEvent>,
}

/// What one build asks for, handed to its thread whole.
struct Request {
    refine_from: Option<u32>,
    note: String,
    rehearsal: bool,
    /// The takes on disk when Build was pressed — the only ones it waits on.
    /// A take recorded after that is not in this build, and must not be
    /// mistaken for an orphan because its transcript has not started yet.
    takes: Vec<u32>,
    model: String,
    provider: Option<String>,
}

#[derive(Default)]
pub(super) struct PlanState {
    take: Option<PlanTake>,
    job: Option<PlanJob>,
    /// Each take's state as the pane last drew it, so a transcript landing
    /// patches that one row rather than repainting a page being typed in.
    shown: BTreeMap<u32, &'static str>,
    polled: Option<Instant>,
}

/// Why Record idea cannot start now, or `None` when it can.
fn idea_refusal(chapter: Option<u32>, on_break: bool, building: bool) -> Option<String> {
    if let Some(n) = chapter {
        return Some(format!(
            "Chapter {n:02} is recording — stop it before recording an idea."
        ));
    }
    if on_break {
        return Some(
            "On a break for a figure — press ⌃⇧S to finish it before recording an idea.".into(),
        );
    }
    building.then(|| {
        "A plan is being built — record the next idea once it lands, or it would be left out."
            .into()
    })
}

/// Why Start Recording cannot start a chapter now, or `None` when it can.
fn recording_refusal(take: Option<u32>) -> Option<String> {
    take.map(|n| {
        format!(
            "Idea take {n:02} is recording on the Plan tab — stop it before starting a chapter."
        )
    })
}

/// Why Build, Refine or Plan from rehearsal cannot start, or `None`.
///
/// `job` is whether a build is running, and whether it is this project's.
/// `approved` is the selected version's number when that version is approved:
/// a refine starts from the selected version, and a fresh build beside an
/// approved one is refused too, so the lock reads the same on every button.
fn build_refusal(job: Option<bool>, take: Option<u32>, approved: Option<u32>) -> Option<String> {
    match job {
        Some(true) => return Some("A plan is already being built — wait for it to land.".into()),
        Some(false) => {
            return Some(
                "A plan is being built for another project — wait for it to land, then build \
                 this one."
                    .into(),
            )
        }
        None => {}
    }
    if let Some(n) = take {
        return Some(format!(
            "Take {n:02} is recording — stop it first, or it would be left out of the plan."
        ));
    }
    approved.map(|n| format!("Plan {n} is approved — un-approve it to refine it or build again."))
}

/// `125` seconds as `2:05`.
fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl App {
    fn plan_dir(&self) -> std::path::PathBuf {
        crate::plan::dir(&self.session)
    }

    /// Whether a Plan message came from a page drawn for the open project.
    fn plan_page_is_current(&self, root: &str) -> bool {
        self.session.root.to_str() == Some(root)
    }

    fn plan_building_here(&self) -> bool {
        self.plan
            .job
            .as_ref()
            .is_some_and(|job| job.session.root == self.session.root)
    }

    fn plan_live(&self) -> PlanLive {
        PlanLive {
            recording: self.plan.take.as_ref().map(|take| take.n),
            building: self.plan_building_here(),
        }
    }

    /// The Plan tab's line above its pane.
    fn set_plan_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_plan_status(text);
        }
    }

    /// The Plan tab's pane: the author's input, the idea takes and the
    /// selected plan version. Per project, like the plan itself, so a version
    /// switch leaves it alone and a project switch redraws it.
    ///
    /// Only called after the author pressed something on the page, or when a
    /// plan lands — never for progress, which goes on the status line — since
    /// a repaint takes the caret out of any box being typed in.
    pub(super) fn update_plan_view(&mut self) {
        let view = planning::plan_view(&self.session, self.plan_live());
        self.plan.shown = view.takes.iter().map(|row| (row.n, row.state)).collect();
        if let Some(live) = self.live.as_ref() {
            live.plan_pane.show(&planning::plan_page(&view));
        }
    }

    /// Start Recording's check: a chapter does not start over an idea take.
    /// Says why on the Record tab's line, which is where the press was.
    pub(super) fn plan_take_blocks_recording(&self) -> bool {
        match recording_refusal(self.plan.take.as_ref().map(|take| take.n)) {
            Some(why) => {
                self.set_render_status(&why);
                true
            }
            None => false,
        }
    }

    /// Record idea / Stop.
    pub(super) fn toggle_plan_take(&mut self, root: &str) {
        if !self.plan_page_is_current(root) {
            return;
        }
        if self.plan.take.is_some() {
            self.finish_plan_take();
        } else {
            self.start_plan_take();
        }
    }

    fn start_plan_take(&mut self) {
        let chapter = self.router.as_ref().map(|r| r.current_chapter_number());
        if let Some(why) = idea_refusal(chapter, self.aside.is_some(), self.plan_building_here()) {
            self.set_plan_status(&why);
            return;
        }
        let Some(conn) = self.connection.as_ref() else {
            self.set_plan_status(
                "No microphone is running — pick one on the Video recording tab, then try again.",
            );
            return;
        };
        let dir = self.plan_dir();
        let n = crate::plan::next_take_number(&dir);
        let path = crate::plan::take_path(&dir, n);
        match Aside::start(conn, &path) {
            Ok(aside) => {
                self.plan.take = Some(PlanTake {
                    aside,
                    n,
                    started: Instant::now(),
                    shown_secs: None,
                });
                self.set_plan_status(&format!(
                    "Recording take {n:02} — talk the idea through, then press Stop."
                ));
                self.update_plan_view();
            }
            Err(err) => {
                // A writer that failed to install can still have made the
                // file, and an empty take would sit in the list as a failure
                // nobody recorded.
                let _ = std::fs::remove_file(&path);
                self.set_plan_status(&format!("Could not start take {n:02}: {err:#}"));
            }
        }
    }

    /// Finish the open idea take, if there is one, and send it to be
    /// transcribed. Every path that stops capture or leaves the project calls
    /// this first — see the module docs.
    pub(super) fn finish_plan_take(&mut self) {
        let Some(PlanTake { aside, n, .. }) = self.plan.take.take() else {
            return;
        };
        match aside.finish() {
            Ok(finished) => {
                let secs = finished.duration.as_secs();
                // The words are what the plan is built from, so the upload
                // starts now; a Build pressed before it lands waits for it.
                crate::notes::spawn_chapter_transcript(finished.path);
                self.set_plan_status(&format!(
                    "Take {n:02} recorded ({}) — transcribing it now.",
                    clock(secs)
                ));
            }
            Err(err) => self.set_plan_status(&format!("Take {n:02} was not saved: {err:#}")),
        }
        self.update_plan_view();
    }

    pub(super) fn delete_plan_take(&mut self, root: &str, n: u32) {
        if !self.plan_page_is_current(root) {
            return;
        }
        if self.plan.take.as_ref().is_some_and(|take| take.n == n) {
            self.set_plan_status(&format!("Take {n:02} is recording — stop it first."));
            return;
        }
        if self.plan_building_here() {
            self.set_plan_status(&format!(
                "A plan is being built from these takes — delete take {n:02} once it lands."
            ));
            return;
        }
        match crate::plan::delete_take(&self.plan_dir(), n) {
            Ok(()) => self.set_plan_status(&format!("Take {n:02} deleted.")),
            Err(err) => self.set_plan_status(&format!("Could not delete take {n:02}: {err:#}")),
        }
        self.update_plan_view();
    }

    /// Send a failed or orphaned take to be transcribed again.
    pub(super) fn retry_plan_take(&mut self, root: &str, n: u32) {
        if !self.plan_page_is_current(root) {
            return;
        }
        let audio = crate::plan::take_path(&self.plan_dir(), n);
        if self.plan.take.as_ref().is_some_and(|take| take.n == n) || !audio.is_file() {
            self.set_plan_status(&format!("Take {n:02} has no finished recording to send."));
            return;
        }
        crate::notes::spawn_chapter_transcript(audio);
        self.set_plan_status(&format!("Sending take {n:02} to be transcribed again…"));
        self.update_plan_view();
    }

    /// The instructions and typed-idea boxes, on every keystroke. Never
    /// repaints on success — see [`App::update_plan_view`].
    pub(super) fn save_plan_input(&mut self, fields: &BTreeMap<String, String>) {
        if !fields
            .get("root")
            .is_some_and(|root| self.plan_page_is_current(root))
        {
            return;
        }
        let dir = self.plan_dir();
        let mut input = crate::plan::load_input(&dir);
        if let Some(text) = fields.get("instructions") {
            input.instructions = text.clone();
        }
        if let Some(text) = fields.get("typed") {
            input.typed = text.clone();
        }
        if let Err(err) = crate::plan::save_input(&dir, &input) {
            self.set_plan_status(&format!(
                "Could not save your idea and instructions: {err:#}"
            ));
        }
    }

    /// Hand edits to a plan version, on every keystroke. An approved version
    /// refuses them — [`crate::plan::update`] is the lock — and the page is
    /// redrawn then, since it was showing that version as editable.
    pub(super) fn save_plan(&mut self, fields: &BTreeMap<String, String>) {
        if !fields
            .get("root")
            .is_some_and(|root| self.plan_page_is_current(root))
        {
            return;
        }
        let Some(n) = fields.get("number").and_then(|n| n.parse::<u32>().ok()) else {
            return;
        };
        let dir = self.plan_dir();
        let stored = match crate::plan::load(&dir, n) {
            Ok(plan) => plan,
            Err(err) => {
                self.set_plan_status(&format!("Could not read Plan {n}: {err:#}"));
                return;
            }
        };
        let edited = planning::plan_from_fields(&stored, fields);
        if edited == stored {
            return;
        }
        if let Err(err) = crate::plan::update(&dir, &edited) {
            self.set_plan_status(&format!("Not saved: {err:#}."));
            if stored.approved {
                self.update_plan_view();
            }
        }
    }

    pub(super) fn select_plan_version(&mut self, root: &str, n: u32) {
        if !self.plan_page_is_current(root) {
            return;
        }
        let dir = self.plan_dir();
        match crate::plan::select(&dir, n) {
            Ok(()) => {
                let approved = crate::plan::approved(&dir).map(|plan| plan.number);
                self.set_plan_status(&match approved {
                    Some(m) if m == n => {
                        format!("Plan {n} on screen — approved, and the speaking notes.")
                    }
                    Some(m) => format!(
                        "Plan {n} on screen — Plan {m} is still the approved one, and the \
                         speaking notes."
                    ),
                    None => format!("Plan {n} on screen — not approved yet."),
                });
            }
            Err(err) => self.set_plan_status(&format!("{err:#}")),
        }
        self.update_plan_view();
        self.update_project_view();
    }

    /// Approve version `n` — which writes the speaking-notes deck from it and
    /// reloads the teleprompter — or lift its lock, leaving the deck as it is.
    pub(super) fn approve_plan(&mut self, root: &str, n: u32, value: bool) {
        if !self.plan_page_is_current(root) {
            return;
        }
        let dir = self.plan_dir();
        if value {
            let written = self
                .session
                .notes_dir()
                .and_then(|notes| crate::plan::approve(&dir, n, &notes));
            match written {
                Ok(_) => {
                    self.reload_deck();
                    self.set_plan_status(&format!(
                        "Plan {n} approved — it is the speaking notes on the Record tab now, \
                         and locked against edits."
                    ));
                }
                Err(err) => self.set_plan_status(&format!("Could not approve Plan {n}: {err:#}")),
            }
        } else {
            match crate::plan::unapprove(&dir, n) {
                Ok(()) => self.set_plan_status(&format!(
                    "Plan {n} is no longer approved — it can be edited and refined again. The \
                     speaking notes are unchanged."
                )),
                Err(err) => {
                    self.set_plan_status(&format!("Could not un-approve Plan {n}: {err:#}"))
                }
            }
        }
        self.update_plan_view();
        self.update_project_view();
    }

    /// Build plan, Refine and Plan from rehearsal.
    pub(super) fn build_plan(&mut self, root: &str, refine: bool, note: &str, rehearsal: bool) {
        if !self.plan_page_is_current(root) {
            return;
        }
        let dir = self.plan_dir();
        let selected = crate::plan::selected(&dir);
        let approved = selected
            .and_then(|n| crate::plan::load(&dir, n).ok())
            .filter(|plan| plan.approved)
            .map(|plan| plan.number);
        let job = self
            .plan
            .job
            .as_ref()
            .map(|job| job.session.root == self.session.root);
        let take = self.plan.take.as_ref().map(|take| take.n);
        if let Some(why) = build_refusal(job, take, approved) {
            self.set_plan_status(&why);
            return;
        }
        if rehearsal && crate::notes::closed_chapter_numbers(&self.session.dir).is_empty() {
            self.set_plan_status(
                "This recording version has no chapters yet — record a rehearsal first.",
            );
            return;
        }
        let request = Request {
            refine_from: if refine { selected } else { None },
            note: if refine {
                note.to_string()
            } else {
                String::new()
            },
            rehearsal,
            takes: crate::plan::takes(&dir).iter().map(|take| take.n).collect(),
            model: self.notes_pick.model().to_string(),
            provider: self.notes_pick.provider().map(str::to_string),
        };
        let what = match request.refine_from {
            Some(n) => format!("Refining Plan {n}"),
            None if rehearsal => "Planning from the rehearsal".to_string(),
            None => "Building a plan".to_string(),
        };
        let session = self.session.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = {
            let session = session.clone();
            std::thread::Builder::new()
                .name("plan-build".into())
                .spawn(move || run_job(&session, &request, &tx))
        };
        match spawned {
            Ok(_) => {
                self.plan.job = Some(PlanJob { session, rx });
                self.set_plan_status(&format!("{what}…"));
                self.update_plan_view();
            }
            Err(err) => self.set_plan_status(&format!("Could not start the plan: {err}")),
        }
    }

    /// The build's progress and result. Progress for a project that is no
    /// longer open is dropped, and so is its result — the version it wrote is
    /// on disk there, and shows when that project is opened again.
    pub(super) fn drain_plan(&mut self) {
        let Some(job) = self.plan.job.as_ref() else {
            return;
        };
        let mut status = None;
        let mut done = None;
        loop {
            match job.rx.try_recv() {
                Ok(JobEvent::Status(line)) => status = Some(line),
                Ok(JobEvent::Done(result)) => {
                    done = Some(result);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = Some(Err("the plan job stopped unexpectedly — try again".into()));
                    break;
                }
            }
        }
        let here = job.session.root == self.session.root;
        if let Some(line) = status.filter(|_| here) {
            self.set_plan_status(&line);
        }
        let Some(result) = done else {
            return;
        };
        self.plan.job = None;
        if !here {
            return;
        }
        match result {
            Ok(plan) => self.set_plan_status(&format!(
                "Plan {} ready — read it through, edit what is off, then Approve it to make it \
                 the speaking notes.",
                plan.number
            )),
            Err(err) => self.set_plan_status(&format!("Could not build the plan: {err}")),
        }
        // The buttons were switched off for the build, so the page is redrawn
        // either way: with the new version, or as it was.
        self.update_plan_view();
        self.update_project_view();
    }

    /// Once a tick: the take's timer, and any take row whose transcript has
    /// landed since the pane was drawn.
    pub(super) fn tick_plan(&mut self) {
        let timer = self.plan.take.as_mut().and_then(|take| {
            let secs = take.started.elapsed().as_secs();
            (take.shown_secs != Some(secs)).then(|| {
                take.shown_secs = Some(secs);
                format!(
                    "Recording take {:02} — {}. Talk the idea through, then press Stop.",
                    take.n,
                    clock(secs)
                )
            })
        });
        if let Some(line) = timer {
            self.set_plan_status(&line);
        }
        self.poll_plan_takes();
    }

    /// Patch the take rows whose transcript state changed, one row at a time
    /// through the pane's `planTake`, so a transcript landing never repaints
    /// the page under someone typing in it. Only while a row was drawn as
    /// transcribing: otherwise nothing is going to change on its own.
    fn poll_plan_takes(&mut self) {
        if self.plan.polled.is_some_and(|at| at.elapsed() < POLL_EVERY) {
            return;
        }
        self.plan.polled = Some(Instant::now());
        if !self
            .plan
            .shown
            .values()
            .any(|state| *state == "transcribing")
        {
            return;
        }
        let rows = planning::take_rows(&self.plan_dir(), self.plan_live().recording);
        let mut landed = Vec::new();
        for row in rows {
            let Some(shown) = self.plan.shown.get_mut(&row.n) else {
                continue;
            };
            if *shown == row.state {
                continue;
            }
            *shown = row.state;
            if row.state == "ready" {
                landed.push(row.n);
            }
            if let (Some(live), Ok(json)) = (self.live.as_ref(), serde_json::to_string(&row)) {
                live.plan_pane
                    .eval(&format!("window.planTake && window.planTake({json});"));
            }
        }
        // The timer and a build's progress own the line while they run.
        if !landed.is_empty() && self.plan.take.is_none() && !self.plan_building_here() {
            let names: Vec<String> = landed.iter().map(|n| format!("{n:02}")).collect();
            self.set_plan_status(&format!(
                "Take {} transcribed — it goes into the next build.",
                names.join(", ")
            ));
        }
    }

    /// "Go to recording →".
    pub(super) fn go_to_recording(&self) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.select_tab("draft");
        }
    }
}

/// The build's thread: wait for the transcripts it is built from, then make
/// the one model call.
fn run_job(session: &Session, request: &Request, tx: &Sender<JobEvent>) {
    let status = |line: String| {
        let _ = tx.send(JobEvent::Status(line));
    };
    let dir = crate::plan::dir(session);
    let chapters = if request.rehearsal {
        crate::notes::closed_chapter_numbers(&session.dir)
    } else {
        Vec::new()
    };
    let deadline = Instant::now() + crate::notes::WAIT_FOR;
    loop {
        let takes = crate::plan::takes_still_running(&dir, &request.takes);
        let recorded = crate::notes::still_running(&session.dir, &chapters);
        if takes.is_empty() && recorded.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            status("Stopped waiting for transcripts — planning from the ones that landed.".into());
            break;
        }
        let mut lines = Vec::new();
        if !takes.is_empty() {
            lines.push(crate::plan::take_waiting_message(&takes));
        }
        if !recorded.is_empty() {
            lines.push(crate::notes::waiting_message(&recorded));
        }
        status(lines.join(" "));
        std::thread::sleep(POLL_EVERY);
    }
    status(format!("Asking {} for the plan…", request.model));
    let result = crate::plan::build(
        session,
        request.refine_from,
        &request.note,
        request.rehearsal,
        &request.model,
        request.provider.as_deref(),
    )
    .map(Box::new)
    .map_err(|err| format!("{err:#}"));
    let _ = tx.send(JobEvent::Done(result));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Record idea and Start Recording refuse each other, and a figure's break
    /// holds the one aside the capture session allows.
    #[test]
    fn an_idea_take_refuses_a_chapter_a_break_and_a_build() {
        assert_eq!(idea_refusal(None, false, false), None);
        let chapter = idea_refusal(Some(3), false, false).unwrap();
        assert!(chapter.contains("Chapter 03 is recording"), "{chapter}");
        let on_break = idea_refusal(None, true, false).unwrap();
        assert!(on_break.contains("⌃⇧S"), "{on_break}");
        let building = idea_refusal(None, false, true).unwrap();
        assert!(building.contains("left out"), "{building}");
        // A chapter is the first thing named: it is the one to stop.
        assert!(idea_refusal(Some(1), true, true)
            .unwrap()
            .starts_with("Chapter 01"));
    }

    #[test]
    fn start_recording_refuses_while_an_idea_take_records() {
        assert_eq!(recording_refusal(None), None);
        let why = recording_refusal(Some(2)).unwrap();
        assert!(why.contains("Idea take 02"), "{why}");
        assert!(why.contains("Plan tab"), "{why}");
    }

    /// One build at a time across projects, none over a take still
    /// recording, and none beside an approved version.
    #[test]
    fn a_build_is_refused_while_busy_or_approved() {
        assert_eq!(build_refusal(None, None, None), None);
        assert!(build_refusal(Some(true), None, None)
            .unwrap()
            .contains("already being built"));
        assert!(build_refusal(Some(false), None, None)
            .unwrap()
            .contains("another project"));
        assert!(build_refusal(None, Some(4), None)
            .unwrap()
            .contains("Take 04 is recording"));
        let locked = build_refusal(None, None, Some(2)).unwrap();
        assert!(locked.contains("Plan 2 is approved"), "{locked}");
    }

    #[test]
    fn the_take_timer_reads_minutes_and_seconds() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(125), "2:05");
    }
}
