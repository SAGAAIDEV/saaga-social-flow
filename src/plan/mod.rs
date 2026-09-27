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

pub mod schema;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub use schema::Plan;

use crate::agent::plan::{Refine, Sources};
use crate::notes::TranscriptStatus;
use crate::session::Session;

pub const PLAN_DIR: &str = "plan";
pub const INPUT_JSON: &str = "input.json";
pub const CURRENT_JSON: &str = "current.json";
/// Where a rehearsal-built deck is kept the first time a plan replaces it.
pub const NOTES_BEFORE_PLAN: &str = "notes.before-plan.json";
/// Present once a plan has written the deck, so the rehearsal backup is only
/// ever taken of a deck a plan did not write.
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
// The Plan tab's autosave (phase 3).
#[allow(dead_code)]
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

/// Where the next idea take is recorded.
// Record idea on the Plan tab (phase 3).
#[allow(dead_code)]
pub fn next_take_path(dir: &Path) -> PathBuf {
    let n = takes(dir).last().map_or(1, |take| take.n + 1);
    dir.join(format!("take-{n:02}.m4a"))
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
        let html = approve(&dir, n, &session.notes_dir()?)?;
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
        assert_eq!(next_take_path(&dir), dir.join("take-01.m4a"));
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
        assert_eq!(next_take_path(&dir), dir.join("take-03.m4a"));
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
}
