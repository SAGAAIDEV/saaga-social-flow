//! The video's plan, made before recording: `{root}/plan/`.
//!
//! An idea is talked into the mic as one or more takes (or typed, or taken
//! from a rehearsal), and one model call turns it into a plan — hook, outline,
//! chapters, CTA, recording instructions. Plans are versioned and never
//! overwritten: every build or refine is the next `vN.json`. One version at a
//! time may be approved; approving locks it and writes the speaking-notes deck,
//! which is how the plan reaches the teleprompter, the chapter cards, the
//! titles, the blog headings and the posts — they all read `notes.json`.
//!
//! Per project, like the notes: a plan outlives recording versions.
//!
//! ```text
//! plan/
//!   input.json               the author's instructions and typed idea
//!   take-01.m4a              an idea take, mic only
//!   take-01.transcript.json  written by notes::spawn_chapter_transcript
//!   v1.json, v2.json, …      one per build / refine
//!   current.json             { selected: N } — the version on screen
//!   llm.json                 the last model call
//! ```

pub mod binding;
pub mod schema;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use schema::ChapterKind;
pub use schema::Plan;

use crate::agent::plan::{Refine, Sources};
use crate::notes::TranscriptStatus;
use crate::session::Session;

pub const PLAN_DIR: &str = "plan";
pub const INPUT_JSON: &str = "input.json";
pub const CURRENT_JSON: &str = "current.json";
/// Where a rehearsal-built deck is kept the first time a plan replaces it.
pub const NOTES_BEFORE_PLAN: &str = "notes.before-plan.json";
/// Present while the deck is one a plan wrote, so the rehearsal backup is only
/// ever taken of a deck a plan did not write. A rehearsal deck written after it
/// removes it again ([`rehearsal_wrote_deck`]).
const WROTE_DECK: &str = ".wrote-deck";

pub fn dir(session: &Session) -> PathBuf {
    session.root.join(PLAN_DIR)
}

/// The author's own words, beside the takes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub typed: String,
}

pub fn load_input(dir: &Path) -> Input {
    read_json(&dir.join(INPUT_JSON)).unwrap_or_default()
}

pub fn save_input(dir: &Path, input: &Input) -> Result<()> {
    write_json(&dir.join(INPUT_JSON), input)
}

fn version_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("v{n}.json"))
}

/// Every plan version on disk, in order.
pub fn versions(dir: &Path) -> Vec<u32> {
    let mut out: Vec<u32> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.strip_prefix('v')?.strip_suffix(".json")?.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

pub fn load(dir: &Path, n: u32) -> Result<Plan> {
    read_json(&version_path(dir, n))
}

/// The version on screen: the one last selected, else the newest.
pub fn selected(dir: &Path) -> Option<u32> {
    #[derive(Deserialize)]
    struct Current {
        selected: u32,
    }
    let all = versions(dir);
    read_json::<Current>(&dir.join(CURRENT_JSON))
        .ok()
        .map(|c| c.selected)
        .filter(|n| all.contains(n))
        .or_else(|| all.last().copied())
}

pub fn select(dir: &Path, n: u32) -> Result<()> {
    if !version_path(dir, n).is_file() {
        bail!("there is no Plan {n}");
    }
    write_json(
        &dir.join(CURRENT_JSON),
        &serde_json::json!({ "selected": n }),
    )
}

/// The approved version, if there is one.
pub fn approved(dir: &Path) -> Option<Plan> {
    versions(dir)
        .into_iter()
        .filter_map(|n| load(dir, n).ok())
        .find(|plan| plan.approved)
}

/// The recording chapter that carries the approved plan's call to action, which
/// gets no chapter card and no short: the **last recorded** chapter, whenever
/// the plan ends with a CTA.
///
/// Not the chapter at the CTA's position in the plan. Plan and recording only
/// line up by count, and one extra chapter break would otherwise take the card
/// off a body chapter and leave it in front of the ask. Never chapter one
/// either: a plan is at least a hook and a CTA, so a recording that has only
/// its first chapter has not reached the CTA yet — and chapter one has no card
/// to skip anyway.
pub fn cta_chapter(plan: Option<&schema::PlanBody>, recorded: &[u32]) -> Option<u32> {
    if !plan?.ends_with_cta() {
        return None;
    }
    recorded.iter().copied().max().filter(|&n| n > 1)
}

/// The recording chapter that carries the call to action, for `session`'s
/// current recording version.
///
/// Chapters recorded with the plan on the teleprompter say which plan chapter
/// they were (see [`binding`]), and that is the answer: the last recorded
/// chapter bound to the plan's CTA — or none, if the CTA has not been
/// recorded yet. Only a recording with no bindings at all, made before the
/// teleprompter showed the plan, falls back to [`cta_chapter`]'s reading of
/// the approved plan: the last recorded chapter. `None` for a short, whose
/// folder has no plan.
pub fn cta_chapter_of(session: &Session) -> Option<u32> {
    let recorded = crate::notes::closed_chapter_numbers(&session.dir);
    let bound: Vec<(u32, binding::Binding)> = recorded
        .iter()
        .filter_map(|&n| Some((n, binding::load(&session.dir, n)?)))
        .collect();
    if !bound.is_empty() {
        return bound
            .iter()
            .filter(|(_, b)| b.kind == ChapterKind::Cta)
            .map(|(n, _)| *n)
            .max()
            .filter(|&n| n > 1);
    }
    let plan = approved(&dir(session))?;
    cta_chapter(Some(&plan.body), &recorded)
}

/// The plan the recording follows: the approved version, else the newest.
///
/// This is what the teleprompter shows and what New Chapter takes its layout
/// from. A draft drives the recording too — the author records from the plan
/// they have — but only an approved one writes `notes.json`, so a draft never
/// reaches the titles, blog headings and posts that read the deck.
pub fn recording_plan(dir: &Path) -> Option<Plan> {
    approved(dir).or_else(|| load(dir, *versions(dir).last()?).ok())
}

/// The plan `session`'s current recording was made against: the version its
/// most recently recorded bound chapter names, else the approved version.
/// What the video details and the outline are written from.
pub fn plan_for_recording(session: &Session) -> Option<Plan> {
    let plan_dir = dir(session);
    crate::notes::closed_chapter_numbers(&session.dir)
        .into_iter()
        .rev()
        .find_map(|n| binding::load(&session.dir, n))
        .and_then(|b| load(&plan_dir, b.plan).ok())
        .or_else(|| approved(&plan_dir))
}

/// The plan chapter recording chapter `n` was recorded for, if it was.
pub fn bound_chapter(session: &Session, n: u32) -> Option<schema::PlanChapter> {
    binding::resolve(&dir(session), &session.dir, n)
}

/// Writes `plan` as the next version and selects it. Never overwrites: the
/// number is taken from what is on disk, and the file is created new.
pub fn save_new(dir: &Path, mut plan: Plan) -> Result<Plan> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    plan.number = versions(dir).last().copied().unwrap_or(0) + 1;
    plan.approved = false;
    let path = version_path(dir, plan.number);
    let text = serde_json::to_string_pretty(&plan)? + "\n";
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))?;
    select(dir, plan.number)?;
    Ok(plan)
}

/// A hand edit to an existing version. Refused on an approved one — that is
/// what approving means — and it cannot approve or renumber through here.
pub fn update(dir: &Path, plan: &Plan) -> Result<()> {
    let stored = load(dir, plan.number)?;
    if stored.approved {
        bail!(
            "Plan {} is approved — un-approve it to edit it",
            stored.number
        );
    }
    let mut next = plan.clone();
    next.approved = false;
    write_json(&version_path(dir, plan.number), &next)
}

/// Approve version `n`: un-approve every other, lock this one, and write the
/// deck from it into `notes_dir`. The first time, a deck a rehearsal built is
/// kept as [`NOTES_BEFORE_PLAN`] rather than overwritten.
pub fn approve(dir: &Path, n: u32, notes_dir: &Path) -> Result<PathBuf> {
    let mut plan = load(dir, n)?;
    for other in versions(dir).into_iter().filter(|&m| m != n) {
        let mut version = load(dir, other)?;
        if version.approved {
            version.approved = false;
            write_json(&version_path(dir, other), &version)?;
        }
    }
    plan.approved = true;
    write_json(&version_path(dir, n), &plan)?;

    let marker = dir.join(WROTE_DECK);
    let existing = notes_dir.join(crate::notes::NOTES_JSON);
    if !marker.exists() && existing.is_file() {
        std::fs::copy(&existing, notes_dir.join(NOTES_BEFORE_PLAN))
            .with_context(|| format!("keeping {}", existing.display()))?;
    }
    let html = crate::notes::write_deck(notes_dir, &plan.to_notes(), "Chapter")?;
    std::fs::write(&marker, "").with_context(|| format!("writing {}", marker.display()))?;
    Ok(html)
}

/// Whether a plan has written the deck — what the Project tab says the
/// speaking notes came from.
pub fn deck_from_plan(dir: &Path) -> bool {
    dir.join(WROTE_DECK).exists()
}

/// Why the rehearsal's Notes button cannot write the deck now, or `None`.
///
/// Refused while a version is approved: decision 5 of
/// `docs/plan-tab-plan.md` is that only the approved version writes
/// `notes.json`. Asked when Notes is pressed, and again by the notes job just
/// before it writes, since a plan can be approved while it waits on
/// transcripts.
pub fn rehearsal_notes_refusal(dir: &Path) -> Option<String> {
    approved(dir).map(|plan| {
        format!(
            "Plan {} is approved, so it writes the speaking notes — un-approve it on the Plan tab \
             to write notes from the rehearsal instead.",
            plan.number
        )
    })
}

/// A rehearsal has written the deck: it is no longer a plan's, so the Project
/// tab says where it came from, and the next approval keeps it as
/// [`NOTES_BEFORE_PLAN`] before replacing it — rather than finding the marker
/// from an earlier approval and overwriting it with no copy.
pub fn rehearsal_wrote_deck(dir: &Path) -> Result<()> {
    let marker = dir.join(WROTE_DECK);
    match std::fs::remove_file(&marker) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", marker.display())),
    }
}

/// [`approve`], for a project with recordings already in it.
///
/// Approving overwrites the speaking notes — that is the point: the
/// teleprompter, the chapter cards and the deck move to the new plan. But the
/// deck is the project's, read by chapter number, and a take already recorded
/// against the old one would have its blog sections and posts relabelled from
/// the new plan's chapters. So before the overwrite, every recording version
/// with chapters the old deck speaks for keeps a copy of it — see
/// [`freeze_decks`] — and [`resolved_deck`] reads that copy for that version.
pub fn approve_for(session: &Session, n: u32) -> Result<PathBuf> {
    let frozen = freeze_decks(session)?;
    if frozen > 0 {
        eprintln!(
            "stream-recorder: kept the speaking notes {frozen} recording version(s) were made with"
        );
    }
    approve(&dir(session), n, &session.notes_dir()?)
}

/// The version's own copy of the speaking notes it was recorded with, beside
/// its takes. Absent for a version recorded under the current deck.
pub const VERSION_DECK: &str = "notes.json";

/// Every recording folder in the project: `drafts/vN` for each version, or
/// `drafts` itself for a project that never had versions.
fn recording_dirs(session: &Session) -> Vec<PathBuf> {
    let drafts = session.root.join("drafts");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&drafts)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix('v'))
                .is_some_and(|n| n.parse::<u32>().is_ok())
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    if dirs.is_empty() && !crate::notes::closed_chapter_numbers(&drafts).is_empty() {
        dirs.push(drafts);
    }
    dirs.sort();
    dirs
}

/// Copy the project's speaking notes into every recording version that has a
/// chapter recorded without a plan binding and no copy of its own yet — the
/// versions whose chapters the deck, by position, is speaking for. A version
/// whose every chapter is bound needs no copy: [`resolved_deck`] reads its
/// chapters from their bindings. Returns how many copies were made.
pub fn freeze_decks(session: &Session) -> Result<usize> {
    let project_deck = session.notes_dir()?.join(crate::notes::NOTES_JSON);
    if !project_deck.is_file() {
        return Ok(0);
    }
    let mut frozen = 0;
    for dir in recording_dirs(session) {
        let copy = dir.join(VERSION_DECK);
        if copy.exists() {
            continue;
        }
        let unbound = crate::notes::closed_chapter_numbers(&dir)
            .into_iter()
            .any(|n| binding::load(&dir, n).is_none());
        if unbound {
            std::fs::copy(&project_deck, &copy)
                .with_context(|| format!("keeping the speaking notes in {}", copy.display()))?;
            frozen += 1;
        }
    }
    Ok(frozen)
}

/// The speaking notes for `session`'s recording version, one entry per
/// recorded chapter by position — what the blog, the posts, the chapter cards
/// and the titles read.
///
/// Each chapter from the plan chapter it was recorded for when it has a
/// binding; otherwise from the version's own copy of the deck (see
/// [`freeze_decks`]), else the project's deck. `None` when there is neither a
/// deck nor a binding.
pub fn resolved_deck(session: &Session) -> Option<crate::notes::NotesData> {
    let base = crate::notes::load_notes(&session.dir).ok().or_else(|| {
        session
            .notes_dir()
            .ok()
            .and_then(|dir| crate::notes::load_notes(&dir).ok())
    });
    let closed = crate::notes::closed_chapter_numbers(&session.dir);
    let bound: Vec<(u32, schema::PlanChapter)> = closed
        .iter()
        .filter_map(|&n| Some((n, bound_chapter(session, n)?)))
        .collect();
    if bound.is_empty() {
        return base;
    }
    let base_len = base.as_ref().map_or(0, |deck| deck.chapters.len());
    let last = closed.iter().copied().max().unwrap_or(0) as usize;
    let chapters = (1..=last.max(base_len))
        .map(|n| match bound.iter().find(|(m, _)| *m as usize == n) {
            Some((_, chapter)) => crate::notes::Chapter {
                title: chapter.title.clone(),
                points: chapter.points.clone(),
                verbatim: chapter.verbatim.clone(),
                cues: chapter.cues.clone(),
            },
            None => base
                .as_ref()
                .and_then(|deck| deck.chapters.get(n - 1).cloned())
                .unwrap_or_else(|| crate::notes::Chapter {
                    title: String::new(),
                    points: Vec::new(),
                    verbatim: None,
                    cues: Vec::new(),
                }),
        })
        .collect();
    let title = base
        .as_ref()
        .map(|deck| deck.title.clone())
        .filter(|title| !title.trim().is_empty())
        .or_else(|| plan_for_recording(session).map(|plan| plan.body.working_title))
        .unwrap_or_default();
    Some(crate::notes::NotesData {
        title,
        version: session.version,
        chapters,
    })
}

/// Lift the lock. The deck stays as it was: it was written from this plan,
/// and nothing has replaced it yet.
pub fn unapprove(dir: &Path, n: u32) -> Result<()> {
    let mut plan = load(dir, n)?;
    plan.approved = false;
    write_json(&version_path(dir, n), &plan)
}

/// What steers a rehearsal's speaking notes, now the Record tab's Notes
/// prompt field is gone: its job is the plan's instructions box, so the
/// author's instructions come first. The prompt saved before the field went is
/// the fallback, so a standing instruction is not dropped without a word.
pub fn notes_steer(input: &Input, saved: &str) -> Option<String> {
    [input.instructions.trim(), saved.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .map(str::to_string)
}

/// An idea take on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    pub n: u32,
    pub audio: PathBuf,
}

pub fn takes(dir: &Path) -> Vec<Take> {
    let mut out: Vec<Take> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let n = name
                .to_str()?
                .strip_prefix("take-")?
                .strip_suffix(".m4a")?
                .parse()
                .ok()?;
            Some(Take {
                n,
                audio: entry.path(),
            })
        })
        .collect();
    out.sort_by_key(|take| take.n);
    out
}

/// The number the next idea take is recorded under: one past the highest on
/// disk, so a deleted take in the middle leaves a gap rather than a reused
/// name.
pub fn next_take_number(dir: &Path) -> u32 {
    takes(dir).last().map_or(1, |take| take.n + 1)
}

pub fn take_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("take-{n:02}.m4a"))
}

/// Delete an idea take: its audio and its transcript, so the plan is never
/// built from words whose recording is gone. Refused while its transcript is
/// still being written — the job would land its file after the delete, and if
/// the next take reused the number, that stale transcript would sit where the
/// new take's belongs and read as already done.
pub fn delete_take(dir: &Path, n: u32) -> Result<()> {
    let audio = take_path(dir, n);
    let transcript = crate::notes::transcript_path(&audio);
    if crate::notes::transcript_running(&transcript).is_some() {
        bail!("take {n:02} is still transcribing — delete it once that finishes");
    }
    if !audio.is_file() && !transcript.is_file() {
        bail!("there is no take {n:02}");
    }
    for path in [&audio, &transcript] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("deleting {}", path.display())),
        }
    }
    Ok(())
}

/// Where an idea take's transcript job stands, for a job about to wait on it.
///
/// The chapter waiters in `notes` build `chapter-NN` names inside a recording
/// folder, so they cannot be pointed at `plan/take-NN`; this is the same three
/// states over the take's own transcript path.
#[derive(Debug, Clone)]
pub enum TakeJob {
    /// The transcript says the job is over: words, silence, a skip or an error.
    Finished,
    /// A job is running for it in this process right now, on this step.
    Running(crate::notes::TranscriptStage),
    /// Neither: no transcript, or a `processing` one with nothing behind it —
    /// the app quit mid-upload, say. Nothing is going to finish it.
    Orphaned,
}

pub fn take_job(take: &Take) -> TakeJob {
    let out = crate::notes::transcript_path(&take.audio);
    if let Some(stage) = crate::notes::transcript_running(&out) {
        return TakeJob::Running(stage);
    }
    match crate::notes::load_transcript_at(&take.audio) {
        Some(transcript) if crate::notes::is_terminal(&transcript) => TakeJob::Finished,
        _ => TakeJob::Orphaned,
    }
}

/// Why an orphaned take is marked failed, in the words the Plan tab shows.
pub const ORPHANED: &str = "no transcript job is running for this take (the app may have quit \
                            mid-transcript) — press Retry to send it again";

/// The takes among `numbers` a transcript job is still running for, with its
/// step. An orphaned take is marked failed on the way, with a reason, rather
/// than waited on forever — the same rule as `notes::still_running`.
pub fn takes_still_running(
    dir: &Path,
    numbers: &[u32],
) -> Vec<(u32, crate::notes::TranscriptStage)> {
    numbers
        .iter()
        .filter_map(|&n| {
            let take = Take {
                n,
                audio: take_path(dir, n),
            };
            match take_job(&take) {
                TakeJob::Running(stage) => Some((n, stage)),
                TakeJob::Finished => None,
                TakeJob::Orphaned => {
                    // A take deleted since the job started has nothing to mark.
                    if take.audio.is_file() {
                        crate::notes::record_transcript_failure(&take.audio, ORPHANED);
                    }
                    None
                }
            }
        })
        .collect()
}

/// "Waiting for take 02 (uploading, 0m05s) transcript…" — the chapter
/// waiter's wording, so a build and a render read alike.
pub fn take_waiting_message(running: &[(u32, crate::notes::TranscriptStage)]) -> String {
    let parts: Vec<String> = running
        .iter()
        .map(|(n, stage)| {
            let secs = stage.since.elapsed().as_secs();
            format!("{n:02} ({}, {}m{:02}s)", stage.step, secs / 60, secs % 60)
        })
        .collect();
    let noun = if running.len() == 1 { "take" } else { "takes" };
    format!("Waiting for {noun} {} transcript…", parts.join(", "))
}

/// Where a take's transcript stands, for saying so.
#[derive(Debug, Clone, PartialEq)]
pub enum TakeText {
    Words(String),
    /// Finished with nothing usable — silent, skipped or failed — and why.
    Nothing(String),
    Transcribing,
}

pub fn take_text(take: &Take) -> TakeText {
    let Some(transcript) = crate::notes::load_transcript_at(&take.audio) else {
        return TakeText::Transcribing;
    };
    match transcript.status {
        TranscriptStatus::Completed if !transcript.text.trim().is_empty() => {
            TakeText::Words(transcript.text)
        }
        TranscriptStatus::Processing => TakeText::Transcribing,
        _ => TakeText::Nothing(
            transcript
                .error
                .unwrap_or_else(|| "no speech in this take".into()),
        ),
    }
}

/// Everything a plan is built from, and the labels it records as its sources.
pub fn gather(session: &Session, rehearsal: bool) -> (Sources, Vec<String>) {
    let dir = dir(session);
    let input = load_input(&dir);
    let mut labels = Vec::new();
    let takes: Vec<(u32, String)> = takes(&dir)
        .iter()
        .filter_map(|take| match take_text(take) {
            TakeText::Words(text) => {
                labels.push(format!("take {:02}", take.n));
                Some((take.n, text))
            }
            _ => None,
        })
        .collect();
    if !input.typed.trim().is_empty() {
        labels.push("typed idea".into());
    }
    let rehearsal = if rehearsal {
        let chapters = crate::notes::collect_completed(&session.dir);
        let version = session.version.map_or(String::new(), |v| format!(" v{v}"));
        labels.extend(
            chapters
                .iter()
                .map(|(n, _)| format!("rehearsal{version} chapter {n:02}")),
        );
        chapters
    } else {
        Vec::new()
    };
    (
        Sources {
            instructions: input.instructions,
            typed: input.typed,
            takes,
            rehearsal,
        },
        labels,
    )
}

/// Build a new plan version — fresh, or refined from `refine_from` with
/// `note` — save it, select it, and record the model call. Refining an
/// approved version is refused.
pub fn build(
    session: &Session,
    refine_from: Option<u32>,
    note: &str,
    rehearsal: bool,
    model: &str,
    provider: Option<&str>,
) -> Result<Plan> {
    let dir = dir(session);
    let base = refine_from.map(|n| load(&dir, n)).transpose()?;
    if let Some(base) = base.as_ref().filter(|base| base.approved) {
        bail!(
            "Plan {} is approved — un-approve it to refine it",
            base.number
        );
    }
    let (sources, labels) = gather(session, rehearsal);
    let refine = base.as_ref().map(|base| Refine { base, note });
    let (body, step) = crate::agent::plan::build_plan(
        &session.title(),
        &sources,
        refine.as_ref(),
        model,
        provider,
        Some(&session.root),
    )?;
    let plan = save_new(
        &dir,
        Plan {
            number: 0,
            body,
            approved: false,
            refined_from: refine_from,
            refine_note: note.trim().to_string(),
            sources: labels,
            created_at: chrono::Local::now().to_rfc3339(),
        },
    )?;
    crate::agent::trace::write_step(&dir, &session.root, &step)?;
    Ok(plan)
}

/// `cargo run -- plan <project> …` — see [`crate::cli::Command::Plan`].
pub struct Headless<'a> {
    pub project: &'a Path,
    pub refine: Option<&'a str>,
    pub from: Option<u32>,
    pub rehearsal: bool,
    pub approve: Option<u32>,
    pub unapprove: Option<u32>,
    pub typed: Option<&'a str>,
    pub instructions: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// Build, refine, approve or un-approve a plan without the window.
pub fn run_headless(request: Headless) -> Result<()> {
    let Headless {
        project,
        refine,
        from,
        rehearsal,
        approve: approve_n,
        unapprove: unapprove_n,
        typed,
        instructions,
        model,
    } = request;
    let session = Session::open_root(project.to_path_buf())?;
    let dir = dir(&session);
    if let Some(n) = approve_n {
        let html = approve_for(&session, n)?;
        println!("Plan {n} approved; the deck is {}", html.display());
        return Ok(());
    }
    if let Some(n) = unapprove_n {
        unapprove(&dir, n)?;
        println!("Plan {n} is no longer approved; the deck is unchanged");
        return Ok(());
    }
    if typed.is_some() || instructions.is_some() {
        let mut input = load_input(&dir);
        if let Some(typed) = typed {
            input.typed = typed.to_string();
        }
        if let Some(instructions) = instructions {
            input.instructions = instructions.to_string();
        }
        save_input(&dir, &input)?;
    }
    for take in takes(&dir) {
        match take_text(&take) {
            TakeText::Words(_) => {}
            TakeText::Transcribing => {
                println!("take {:02} is still transcribing — left out", take.n)
            }
            TakeText::Nothing(why) => println!("take {:02} left out: {why}", take.n),
        }
    }
    let refine_from = match (refine, from) {
        (Some(_), Some(n)) => Some(n),
        (Some(_), None) => {
            Some(selected(&dir).context("there is no plan to refine yet — build one first")?)
        }
        (None, _) => None,
    };
    let (default_model, default_provider) = crate::outline::model_choice();
    let model = model.map(str::to_string).unwrap_or(default_model);
    println!("planning with {model}…");
    let plan = build(
        &session,
        refine_from,
        refine.unwrap_or_default(),
        rehearsal,
        &model,
        default_provider.as_deref(),
    )?;
    println!("{}\n", serde_json::to_string_pretty(&plan)?);
    println!(
        "wrote {} — approve it with --approve {}",
        version_path(&dir, plan.number).display(),
        plan.number
    );
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Through a temporary file, so a crash mid-write never leaves half a plan.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::schema::{ChapterKind, PlanBody, PlanChapter};
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("plan-mod-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plan(title: &str) -> Plan {
        let chapter = |kind, title: &str| PlanChapter {
            kind,
            title: title.into(),
            goal: String::new(),
            points: vec!["a point".into()],
            verbatim: None,
            cues: Vec::new(),
            show: String::new(),
            layout: None,
            est_seconds: None,
        };
        Plan {
            body: PlanBody {
                working_title: title.into(),
                chapters: vec![
                    chapter(ChapterKind::Hook, "Open"),
                    chapter(ChapterKind::Body, "Middle"),
                    chapter(ChapterKind::Cta, "Close"),
                ],
                ..PlanBody::default()
            },
            ..Plan::default()
        }
    }

    #[test]
    fn versions_are_numbered_in_order_and_never_overwritten() {
        let dir = temp("versions");
        assert_eq!(selected(&dir), None);
        let first = save_new(&dir, plan("One")).unwrap();
        let second = save_new(&dir, plan("Two")).unwrap();
        assert_eq!((first.number, second.number), (1, 2));
        assert_eq!(versions(&dir), [1, 2]);
        assert_eq!(load(&dir, 1).unwrap().body.working_title, "One");
        assert_eq!(selected(&dir), Some(2), "a new version is selected");
        select(&dir, 1).unwrap();
        assert_eq!(selected(&dir), Some(1));
        assert!(select(&dir, 9).is_err());
    }

    #[test]
    fn an_approved_version_refuses_edits_until_unapproved() {
        let dir = temp("lock");
        let notes = dir.join("notes");
        let mut one = save_new(&dir, plan("One")).unwrap();
        approve(&dir, 1, &notes).unwrap();
        one.body.working_title = "Edited".into();
        assert!(update(&dir, &one)
            .unwrap_err()
            .to_string()
            .contains("approved"));
        unapprove(&dir, 1).unwrap();
        update(&dir, &one).unwrap();
        assert_eq!(load(&dir, 1).unwrap().body.working_title, "Edited");
    }

    #[test]
    fn approving_one_version_unapproves_the_rest_and_writes_its_deck() {
        let dir = temp("approve");
        let notes = dir.join("notes");
        save_new(&dir, plan("One")).unwrap();
        save_new(&dir, plan("Two")).unwrap();
        approve(&dir, 1, &notes).unwrap();
        let html = approve(&dir, 2, &notes).unwrap();
        assert!(html.is_file());
        assert!(!load(&dir, 1).unwrap().approved);
        assert!(load(&dir, 2).unwrap().approved);
        assert_eq!(approved(&dir).unwrap().number, 2);
        let deck = crate::notes::load_notes(&notes).unwrap();
        assert_eq!(deck.title, "Two");
        assert_eq!(deck.chapters.len(), 3);
    }

    /// A deck a rehearsal built is kept the first time a plan replaces it —
    /// and only then, so a later approval never backs up the plan's own deck
    /// over it.
    #[test]
    fn a_rehearsal_deck_is_kept_once_before_the_plan_replaces_it() {
        let dir = temp("backup");
        let notes = dir.join("notes");
        let rehearsal = crate::notes::NotesData {
            title: "Rehearsal".into(),
            version: None,
            chapters: Vec::new(),
        };
        crate::notes::write_deck(&notes, &rehearsal, "Chapter").unwrap();
        save_new(&dir, plan("One")).unwrap();
        save_new(&dir, plan("Two")).unwrap();
        approve(&dir, 1, &notes).unwrap();
        approve(&dir, 2, &notes).unwrap();
        let kept: crate::notes::NotesData = read_json(&notes.join(NOTES_BEFORE_PLAN)).unwrap();
        assert_eq!(kept.title, "Rehearsal");
    }

    /// Only the approved version writes the deck: the rehearsal's Notes is
    /// refused while one is approved. Once a rehearsal deck is written after
    /// an un-approve, the deck is the rehearsal's again — the Project tab says
    /// so, and approving again keeps that deck before replacing it.
    #[test]
    fn a_rehearsal_deck_waits_for_the_plan_to_be_unapproved_and_is_kept_again() {
        let dir = temp("rehearsal-after");
        let notes = dir.join("notes");
        save_new(&dir, plan("One")).unwrap();
        assert_eq!(rehearsal_notes_refusal(&dir), None, "nothing approved yet");
        approve(&dir, 1, &notes).unwrap();
        let why = rehearsal_notes_refusal(&dir).unwrap();
        assert!(why.contains("Plan 1 is approved"), "{why}");
        assert!(deck_from_plan(&dir));

        unapprove(&dir, 1).unwrap();
        assert_eq!(rehearsal_notes_refusal(&dir), None);
        let rehearsal = crate::notes::NotesData {
            title: "Second rehearsal".into(),
            version: None,
            chapters: Vec::new(),
        };
        crate::notes::write_deck(&notes, &rehearsal, "Chapter").unwrap();
        rehearsal_wrote_deck(&dir).unwrap();
        assert!(!deck_from_plan(&dir), "the deck is the rehearsal's now");
        rehearsal_wrote_deck(&dir).unwrap();

        approve(&dir, 1, &notes).unwrap();
        let kept: crate::notes::NotesData = read_json(&notes.join(NOTES_BEFORE_PLAN)).unwrap();
        assert_eq!(kept.title, "Second rehearsal");
        assert!(deck_from_plan(&dir));
    }

    /// The CTA is whatever was recorded last, not the plan's position for it:
    /// a recording with one chapter more than the plan still loses the card in
    /// front of its last chapter, not in front of chapter three.
    #[test]
    fn the_cta_chapter_is_the_last_recorded_one_when_the_plan_ends_with_a_cta() {
        let planned = plan("Deploys");
        assert_eq!(cta_chapter(None, &[1, 2, 3]), None, "no plan, no change");
        assert_eq!(cta_chapter(Some(&planned.body), &[1, 2, 3]), Some(3));
        assert_eq!(
            cta_chapter(Some(&planned.body), &[1, 2, 3, 4]),
            Some(4),
            "an extra chapter moves the CTA to the end, with the recording"
        );
        assert_eq!(cta_chapter(Some(&planned.body), &[1, 2]), Some(2));
        assert_eq!(
            cta_chapter(Some(&planned.body), &[1]),
            None,
            "the hook alone has not reached the CTA"
        );
        assert_eq!(cta_chapter(Some(&planned.body), &[]), None);
        let mut open_ended = planned.clone();
        open_ended.body.chapters.pop();
        assert_eq!(cta_chapter(Some(&open_ended.body), &[1, 2, 3]), None);
    }

    #[test]
    fn the_cta_chapter_of_a_session_needs_an_approved_plan() {
        let root = temp("cta-session");
        let session = Session {
            dir: root.join("drafts/v1"),
            root: root.clone(),
            version: Some(1),
        };
        std::fs::create_dir_all(&session.dir).unwrap();
        for n in 1..=4 {
            std::fs::write(session.dir.join(format!("chapter-{n:02}.mp3")), "").unwrap();
        }
        save_new(&dir(&session), plan("Deploys")).unwrap();
        assert_eq!(cta_chapter_of(&session), None, "built, not approved");
        approve(&dir(&session), 1, &root.join("notes")).unwrap();
        assert_eq!(cta_chapter_of(&session), Some(4));
    }

    #[test]
    fn a_saved_approved_flag_is_never_carried_into_a_new_version() {
        let dir = temp("new-unapproved");
        let mut copy = plan("Copy");
        copy.approved = true;
        assert!(!save_new(&dir, copy).unwrap().approved);
    }

    #[test]
    fn takes_are_listed_in_order_and_the_next_one_follows_the_last() {
        let dir = temp("takes");
        assert_eq!(
            take_path(&dir, next_take_number(&dir)),
            dir.join("take-01.m4a")
        );
        for name in [
            "take-02.m4a",
            "take-01.m4a",
            "take-01.transcript.json",
            "v1.json",
        ] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let listed: Vec<u32> = takes(&dir).iter().map(|t| t.n).collect();
        assert_eq!(listed, [1, 2]);
        assert_eq!(
            take_path(&dir, next_take_number(&dir)),
            dir.join("take-03.m4a")
        );
    }

    #[test]
    fn a_takes_transcript_state_is_read_from_beside_it() {
        let dir = temp("take-text");
        let take = Take {
            n: 1,
            audio: dir.join("take-01.m4a"),
        };
        assert_eq!(take_text(&take), TakeText::Transcribing);
        let transcript = dir.join("take-01.transcript.json");
        std::fs::write(
            &transcript,
            r#"{"status":"completed","text":"Deploys are slow."}"#,
        )
        .unwrap();
        assert_eq!(
            take_text(&take),
            TakeText::Words("Deploys are slow.".into())
        );
        std::fs::write(&transcript, r#"{"status":"skipped","error":"silent"}"#).unwrap();
        assert_eq!(take_text(&take), TakeText::Nothing("silent".into()));
    }

    /// The plan job waits on a take only while a job is actually running for
    /// it. A take with neither a job nor a finished transcript is marked
    /// failed with a reason and not waited on — a quit mid-upload must not
    /// hold every later build for the waiter's full timeout.
    #[test]
    fn the_take_waiter_waits_only_on_running_jobs_and_fails_orphans() {
        let dir = temp("waiter");
        for n in 1..=3 {
            std::fs::write(take_path(&dir, n), "").unwrap();
        }
        std::fs::write(
            dir.join("take-01.transcript.json"),
            r#"{"status":"completed","text":"Deploys are slow."}"#,
        )
        .unwrap();
        let running = crate::notes::hold_running_for_test(&dir.join("take-02.transcript.json"))
            .expect("free");

        let waiting = takes_still_running(&dir, &[1, 2, 3]);
        let numbers: Vec<u32> = waiting.iter().map(|(n, _)| *n).collect();
        assert_eq!(numbers, [2], "only the take with a live job is waited on");
        let line = take_waiting_message(&waiting);
        assert!(line.starts_with("Waiting for take 02 (starting"), "{line}");

        // Take 03 had nothing behind it: it now reads as finished, with why.
        let three = Take {
            n: 3,
            audio: take_path(&dir, 3),
        };
        assert!(matches!(take_job(&three), TakeJob::Finished));
        match take_text(&three) {
            TakeText::Nothing(why) => assert!(why.contains("Retry"), "{why}"),
            other => panic!("expected a failed take, got {other:?}"),
        }

        drop(running);
        // Its job ended without writing a transcript: orphaned in turn.
        assert!(takes_still_running(&dir, &[1, 2, 3]).is_empty());
    }

    #[test]
    fn deleting_a_take_removes_its_transcript_and_refuses_while_transcribing() {
        let dir = temp("delete-take");
        std::fs::write(take_path(&dir, 1), "").unwrap();
        std::fs::write(dir.join("take-01.transcript.json"), "{}").unwrap();
        std::fs::write(take_path(&dir, 2), "").unwrap();

        let busy = crate::notes::hold_running_for_test(&dir.join("take-02.transcript.json"))
            .expect("free");
        let refused = delete_take(&dir, 2).unwrap_err().to_string();
        assert!(refused.contains("still transcribing"), "{refused}");
        assert!(
            take_path(&dir, 2).is_file(),
            "a refused delete touches nothing"
        );
        drop(busy);

        delete_take(&dir, 1).unwrap();
        assert!(!take_path(&dir, 1).exists());
        assert!(!dir.join("take-01.transcript.json").exists());
        assert!(
            delete_take(&dir, 1).is_err(),
            "there is no take 01 any more"
        );
        // A gap is left, not refilled: the next take follows the highest.
        assert_eq!(next_take_number(&dir), 3);
    }

    #[test]
    fn the_notes_are_steered_by_the_plan_instructions_before_the_old_prompt() {
        let mut input = Input::default();
        assert_eq!(notes_steer(&input, "  "), None);
        assert_eq!(
            notes_steer(&input, "keep the intro tight").as_deref(),
            Some("keep the intro tight")
        );
        input.instructions = " For engineers. ".into();
        assert_eq!(
            notes_steer(&input, "keep the intro tight").as_deref(),
            Some("For engineers.")
        );
    }

    #[test]
    fn input_round_trips_and_defaults_when_missing() {
        let dir = temp("input");
        assert_eq!(load_input(&dir), Input::default());
        let input = Input {
            instructions: "For engineers.".into(),
            typed: "Cache hit rate.".into(),
        };
        save_input(&dir, &input).unwrap();
        assert_eq!(load_input(&dir), input);
    }

    fn project(tag: &str) -> Session {
        let root = temp(tag);
        let dir = root.join("drafts").join("v1");
        std::fs::create_dir_all(&dir).unwrap();
        Session {
            root,
            dir,
            version: Some(1),
        }
    }

    fn record(session: &Session, n: u32) {
        std::fs::write(session.dir.join(format!("chapter-{n:02}.mp4")), "").unwrap();
    }

    /// The approved version drives the recording; with none approved, the
    /// newest does — whichever one is on screen in the Plan tab.
    #[test]
    fn the_recording_follows_the_approved_plan_else_the_newest() {
        let dir = temp("recording-plan");
        assert!(recording_plan(&dir).is_none());
        save_new(&dir, plan("One")).unwrap();
        save_new(&dir, plan("Two")).unwrap();
        select(&dir, 1).unwrap();
        assert_eq!(recording_plan(&dir).unwrap().number, 2);
        approve(&dir, 1, &dir.join("notes")).unwrap();
        assert_eq!(recording_plan(&dir).unwrap().number, 1);
    }

    /// With takes bound to the plan, the CTA is the chapter recorded for it —
    /// not the last one recorded — and a recording that has not reached it
    /// has none.
    #[test]
    fn the_cta_is_the_chapter_recorded_for_it() {
        let session = project("cta-bound");
        let saved = save_new(&dir(&session), plan("One")).unwrap();
        for n in 1..=4 {
            record(&session, n);
            binding::bind(&session.dir, n, Some(&saved)).unwrap();
        }
        // Chapters 1-3 are the plan's hook, body and CTA; 4 is past it.
        assert_eq!(cta_chapter_of(&session), Some(3));

        let short = project("cta-not-yet");
        let saved = save_new(&dir(&short), plan("One")).unwrap();
        for n in 1..=2 {
            record(&short, n);
            binding::bind(&short.dir, n, Some(&saved)).unwrap();
        }
        assert_eq!(cta_chapter_of(&short), None, "the ask is not recorded yet");
    }

    /// A recording made before takes were bound keeps the old reading: the
    /// last recorded chapter, when the approved plan ends with a CTA.
    #[test]
    fn an_unbound_recording_falls_back_to_the_last_chapter() {
        let session = project("cta-legacy");
        save_new(&dir(&session), plan("One")).unwrap();
        for n in 1..=4 {
            record(&session, n);
        }
        assert_eq!(cta_chapter_of(&session), None, "nothing approved");
        approve(&dir(&session), 1, &session.root.join("notes")).unwrap();
        assert_eq!(cta_chapter_of(&session), Some(4));
    }

    /// What the copy and the outline are written from is the plan the takes
    /// were recorded against, even once a newer one is approved.
    #[test]
    fn the_plan_for_a_recording_is_the_one_its_takes_name() {
        let session = project("recorded-against");
        let first = save_new(&dir(&session), plan("One")).unwrap();
        assert!(plan_for_recording(&session).is_none());
        record(&session, 1);
        binding::bind(&session.dir, 1, Some(&first)).unwrap();
        save_new(&dir(&session), plan("Two")).unwrap();
        approve(&dir(&session), 2, &session.root.join("notes")).unwrap();
        assert_eq!(plan_for_recording(&session).unwrap().number, 1);
        assert_eq!(bound_chapter(&session, 1).unwrap().title, "Open");
    }

    fn deck(title: &str, chapters: &[&str]) -> crate::notes::NotesData {
        crate::notes::NotesData {
            title: title.into(),
            version: None,
            chapters: chapters
                .iter()
                .map(|t| crate::notes::Chapter {
                    title: (*t).into(),
                    points: vec![format!("{t} point")],
                    verbatim: None,
                    cues: Vec::new(),
                })
                .collect(),
        }
    }

    /// A take recorded before plans keeps the notes it was made with: the new
    /// plan overwrites the speaking notes, and that take's blog and posts still
    /// read its own chapters, not the new plan's by position.
    #[test]
    fn approving_over_an_old_take_keeps_its_notes_and_overwrites_the_speaking_notes() {
        let session = project("freeze-old");
        let notes = session.notes_dir().unwrap();
        crate::notes::write_deck(
            &notes,
            &deck("Rehearsal", &["One", "Two", "Three"]),
            "Chapter",
        )
        .unwrap();
        for n in 1..=3 {
            record(&session, n);
        }
        save_new(&dir(&session), plan("New")).unwrap();
        approve_for(&session, 1).unwrap();

        // The speaking notes are the new plan's.
        let speaking = crate::notes::load_notes(&notes).unwrap();
        assert_eq!(speaking.title, "New");
        assert_eq!(speaking.chapters[0].title, "Open");
        // The old take reads the notes it was recorded with.
        let kept = resolved_deck(&session).unwrap();
        assert_eq!(kept.title, "Rehearsal");
        let titles: Vec<_> = kept.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["One", "Two", "Three"]);

        // A second approval does not replace the copy with a later deck.
        save_new(&dir(&session), plan("Newer")).unwrap();
        approve_for(&session, 2).unwrap();
        assert_eq!(resolved_deck(&session).unwrap().title, "Rehearsal");
    }

    /// A take recorded with the teleprompter needs no copy: its chapters read
    /// from their plan bindings, so a new plan does not relabel them.
    #[test]
    fn a_bound_take_reads_its_own_plan_chapters_after_a_new_plan() {
        let session = project("bound");
        let first = save_new(&dir(&session), plan("First")).unwrap();
        approve_for(&session, first.number).unwrap();
        for n in 1..=3 {
            record(&session, n);
            binding::bind(&session.dir, n, Some(&first)).unwrap();
        }
        let mut second = plan("Second");
        second.body.chapters[1].title = "Reorganized middle".into();
        let second = save_new(&dir(&session), second).unwrap();
        assert_eq!(freeze_decks(&session).unwrap(), 0, "every chapter is bound");
        approve_for(&session, second.number).unwrap();
        let deck = resolved_deck(&session).unwrap();
        assert_eq!(
            deck.chapters[1].title, "Middle",
            "the plan it was recorded against"
        );
        assert!(!session.dir.join(VERSION_DECK).exists());
    }

    /// A version with some chapters bound and some not: the bound ones from
    /// their plan, the rest from the deck the version kept.
    #[test]
    fn a_mixed_take_takes_bound_chapters_from_the_plan_and_the_rest_from_its_copy() {
        let session = project("mixed");
        crate::notes::write_deck(
            &session.notes_dir().unwrap(),
            &deck("Rehearsal", &["One", "Two", "Three"]),
            "Chapter",
        )
        .unwrap();
        for n in 1..=3 {
            record(&session, n);
        }
        let plan = save_new(&dir(&session), plan("Plan")).unwrap();
        binding::bind(&session.dir, 2, Some(&plan)).unwrap();
        approve_for(&session, plan.number).unwrap();
        let titles: Vec<String> = resolved_deck(&session)
            .unwrap()
            .chapters
            .into_iter()
            .map(|c| c.title)
            .collect();
        assert_eq!(titles, ["One", "Middle", "Three"]);
    }
}
