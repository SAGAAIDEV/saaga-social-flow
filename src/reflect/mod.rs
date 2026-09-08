//! Reflect stage: read what every generative step did, propose better prompts.
//!
//! Three buttons, three guarantees. **Reflect** only reads. **Validate** runs a
//! proposed preamble against this project's real content and checks the result is
//! usable — a rewrite can no longer break the output *shape*, since every stage's
//! schema is structural, but it can still produce copy nobody would send.
//! **Apply** writes only what a human ticked *and* a validation passed, archiving
//! whatever it replaced.
//!
//! The corpus is built from local files; only Validate and Reflect itself cost a
//! model call.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

use crate::session::Session;

pub mod corpus;
pub mod diff;
pub mod pane;
pub mod schema;
pub mod validate;

pub use schema::{ReflectReport, Rewrite};

pub enum ReflectEvent {
    Status(String),
    /// A fresh report and where it was written.
    Ready(PathBuf, ReflectReport),
    /// One rewrite finished validating. The updated report is already on disk —
    /// the view re-reads it rather than carrying a second copy that could drift.
    Validated(String),
    /// Overlays written, with how many and their new versions.
    Applied(Vec<String>),
    Failed(String),
}

/// Reads every local ledger and asks the model what to change.
#[tracing::instrument(skip_all)]
pub fn spawn_reflect(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<ReflectEvent>,
) {
    if let Err(err) =
        thread::Builder::new()
            .name("reflect".into())
            .spawn(
                move || match run_reflect(&session, &model, provider.as_deref(), &tx) {
                    Ok((path, report)) => {
                        eprintln!("stream-recorder: reflection → {}", path.display());
                        let _ = tx.send(ReflectEvent::Ready(path, report));
                    }
                    Err(err) => {
                        eprintln!("stream-recorder: reflection failed: {err:#}");
                        let _ = tx.send(ReflectEvent::Failed(format!("Reflect failed: {err:#}")));
                    }
                },
            )
    {
        eprintln!("stream-recorder: could not start reflect job: {err}");
    }
}

fn run_reflect(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<ReflectEvent>,
) -> Result<(PathBuf, ReflectReport)> {
    let status = |msg: String| {
        let _ = tx.send(ReflectEvent::Status(msg));
    };

    status("Reading what every generative step produced…".to_string());
    let gathered = gather(session)?;
    let inputs = gathered.inputs();
    if inputs.is_empty() {
        bail_thin()?;
    }
    status(format!("Reflecting on {}…", inputs.summary()));

    let (reflection, step) = crate::agent::reflect::extract_reflection(
        gathered.render(),
        model,
        provider,
        Some(&session.root),
    )?;
    let dir = session.reflect_dir();
    crate::agent::trace::write_step(&dir, &session.root, &step)?;

    let report = ReflectReport {
        project: session.title(),
        version: session.version,
        generated_at: crate::schedule::ledger::now_rfc3339(),
        inputs,
        reflection,
    };
    let path = schema::save(&dir, &report)?;
    Ok((path, report))
}

fn bail_thin() -> Result<()> {
    anyhow::bail!(
        "nothing to reflect on yet — generate titles or posts, and edit what you disagree with"
    )
}

/// Runs one proposed preamble for real and records whether it holds up.
#[tracing::instrument(skip_all, fields(index))]
pub fn spawn_validate(
    session: Session,
    index: usize,
    model: String,
    provider: Option<String>,
    tx: Sender<ReflectEvent>,
) {
    if let Err(err) = thread::Builder::new()
        .name("reflect-validate".into())
        .spawn(
            move || match run_validate(&session, index, &model, provider.as_deref(), &tx) {
                Ok(detail) => {
                    let _ = tx.send(ReflectEvent::Validated(detail));
                }
                Err(err) => {
                    eprintln!("stream-recorder: validation failed: {err:#}");
                    let _ = tx.send(ReflectEvent::Failed(format!("Validate failed: {err:#}")));
                }
            },
        )
    {
        eprintln!("stream-recorder: could not start validate job: {err}");
    }
}

fn run_validate(
    session: &Session,
    index: usize,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<ReflectEvent>,
) -> Result<String> {
    let status = |msg: String| {
        let _ = tx.send(ReflectEvent::Status(msg));
    };
    let dir = session.reflect_dir();
    let mut report = schema::load(&dir).context("no reflection yet — press Reflect first")?;
    let Some(rewrite) = report.reflection.rewrite.get(index).cloned() else {
        anyhow::bail!("no rewrite at position {index}");
    };
    status(format!(
        "Running the proposed {} against this project…",
        rewrite.prompt_id
    ));

    let outcome = validate::run(session, &rewrite, model, provider);
    let detail = outcome.detail.clone();
    eprintln!(
        "stream-recorder: validation of {} — {}",
        rewrite.prompt_id, detail
    );
    if let Some(slot) = report.reflection.rewrite.get_mut(index) {
        slot.validation = Some(outcome);
    }
    schema::save(&dir, &report)?;
    Ok(detail)
}

/// Writes the ticked, validated overlays.
#[tracing::instrument(skip_all)]
pub fn spawn_apply(session: Session, tx: Sender<ReflectEvent>) {
    if let Err(err) = thread::Builder::new()
        .name("reflect-apply".into())
        .spawn(move || match run_apply(&session, &tx) {
            Ok(applied) => {
                let _ = tx.send(ReflectEvent::Applied(applied));
            }
            Err(err) => {
                eprintln!("stream-recorder: apply failed: {err:#}");
                let _ = tx.send(ReflectEvent::Failed(format!("Apply failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start apply job: {err}");
    }
}

fn run_apply(session: &Session, tx: &Sender<ReflectEvent>) -> Result<Vec<String>> {
    let status = |msg: String| {
        let _ = tx.send(ReflectEvent::Status(msg));
    };
    let dir = session.reflect_dir();
    let report = schema::load(&dir).context("no reflection yet — press Reflect first")?;
    let ready: Vec<&Rewrite> = report.applicable().collect();
    if ready.is_empty() {
        status("Nothing to apply — tick a rewrite and validate it first.".to_string());
        return Ok(Vec::new());
    }

    let at = crate::schedule::ledger::now_rfc3339();
    let mut applied = Vec::new();
    for rewrite in ready {
        let row = crate::agent::prompt::apply(
            &session.root,
            &rewrite.prompt_id,
            &rewrite.preamble,
            "reflect",
            Some(&rewrite.why),
            &at,
        )?;
        let line = format!("{} → v{}", row.prompt_id, row.version);
        status(format!("Applied {line}."));
        applied.push(line);
    }
    Ok(applied)
}

/// Builds the corpus from whatever this project has on disk.
fn gather(session: &Session) -> Result<corpus::Corpus> {
    let steps = crate::agent::trace::load_ledger(&session.root).unwrap_or_default();

    let mut gathered = corpus::Corpus {
        project: session.title(),
        llm_steps: steps.len(),
        ..corpus::Corpus::default()
    };

    // The preambles a rewrite would replace, as they stand right now.
    for id in [
        crate::agent::prompt::TITLES,
        crate::agent::prompt::POSTS,
        crate::agent::prompt::SUBSTACK,
        crate::agent::prompt::BLOG,
    ] {
        if let Some(resolved) = crate::agent::prompt::live(id, Some(&session.root)) {
            gathered
                .prompts
                .push((id.to_string(), resolved.label(), resolved.text));
        }
    }

    if let (Some(generated), Ok(final_titles)) = (
        latest_output(&steps, crate::agent::prompt::TITLES),
        crate::titles::load(&session.titles_dir()),
    ) {
        gathered.titles = corpus::title_deltas(&generated, &final_titles);
    }

    if let (Some(generated), Ok(final_posts)) = (
        latest_output(&steps, crate::agent::prompt::POSTS),
        crate::posts::load_manifest(&session.posts_dir()),
    ) {
        gathered.captions = corpus::caption_deltas(&generated, &final_posts);
    }

    if let Ok(plan) = crate::schedule::load_plan(&session.schedule_dir()) {
        gathered.declined = corpus::declined(&plan);
    }

    if let Ok(rows) = crate::analytics::schema::load_rows(&session.root) {
        gathered.measured = corpus::measured(&rows);
    }

    Ok(gathered)
}

/// The most recent output for a prompt id — what the model last produced.
fn latest_output(
    steps: &[crate::agent::trace::LlmStep],
    prompt_id: &str,
) -> Option<serde_json::Value> {
    steps
        .iter()
        .rev()
        .find(|step| step.prompt_id == prompt_id)
        .map(|step| step.output.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::prompt::Resolved;
    use crate::agent::trace::LlmStep;

    fn step(prompt_id: &str, preamble: &str, output: serde_json::Value) -> LlmStep {
        LlmStep::new(
            prompt_id,
            "step",
            "model",
            None,
            &Resolved {
                text: preamble.into(),
                version: Some(0),
                hash: crate::agent::prompt::hash_of(preamble),
            },
            "prompt",
            output,
        )
        .unwrap()
    }

    #[test]
    fn the_latest_output_for_a_prompt_wins() {
        let steps = vec![
            step("titles.chapter_cards", "old", serde_json::json!({"n": 1})),
            step("posts.social", "p", serde_json::json!({"items": []})),
            step("titles.chapter_cards", "new", serde_json::json!({"n": 2})),
        ];
        assert_eq!(
            latest_output(&steps, "titles.chapter_cards"),
            Some(serde_json::json!({"n": 2}))
        );
        assert_eq!(latest_output(&steps, "notes.slide_deck"), None);
    }

    #[test]
    fn a_prompt_never_run_has_no_output() {
        assert_eq!(latest_output(&[], "posts.social"), None);
    }
}
