//! Local component-building stage. Export once; subsequent runs preserve editorial inputs.
//!
//! The stage writes `component-job/components.json`, validated against the
//! landing repo's catalog. [`built`] is how the publish reads it back: an
//! article places a component by the step id it was requested under, and this
//! is where that id becomes the block the CMS receives.
use crate::session::Session;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Existing recorder project root.
    #[arg(long)]
    pub session: PathBuf,
    /// Landing checkout containing the component catalog and validator.
    #[arg(long)]
    pub landing_repo: PathBuf,
    /// Stable topic identity. Reuse across retries and recording versions.
    #[arg(long)]
    pub source_id: String,
    /// Existing horizontal output file. Required on first export.
    #[arg(long)]
    pub horizontal: Option<PathBuf>,
    /// Existing vertical output file. Required on first export.
    #[arg(long)]
    pub vertical: Option<PathBuf>,
    /// JSON array of {id, brief} component requests. Required on first export.
    #[arg(long)]
    pub steps: Option<PathBuf>,
    /// Plain-text transcript override; otherwise use the session's chapter transcripts.
    #[arg(long)]
    pub transcript: Option<PathBuf>,
    #[arg(long, default_value = "education", value_parser = ["education", "blog", "ai-seo-automation"])]
    pub destination: String,
    /// prepare exports the job; run launches Codex; check validates a manual repair.
    #[arg(long, default_value = "prepare", value_parser = ["prepare", "run", "check"])]
    pub action: String,
}

/// Where the stage leaves its work, under the blog directory.
pub const JOB_DIR: &str = "component-job";
const RESULT_JSON: &str = "components.json";

/// The steps file the article is written against and the stage is run from.
pub const STEPS_JSON: &str = "steps.json";

/// The components this article is expected to place, as the prompt describes
/// them.
///
/// Read from `{blog}/steps.json` — the same `[{id, brief}]` shape `--steps`
/// takes — because the article has to be written *before* anything is built:
/// what the model needs in order to place a component well is what it is for,
/// and the brief is the only description of that which exists at draft time.
///
/// Empty whenever the file is absent, which is the common case. An article that
/// places no components is the normal article.
pub fn requested(session: &Session) -> Vec<super::generate::EmbedOffer> {
    let path = session.blog_dir().join(STEPS_JSON);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let steps: Value = match serde_json::from_str(&text) {
        Ok(steps) => steps,
        Err(err) => {
            eprintln!(
                "stream-recorder: {} is not valid JSON: {err}",
                path.display()
            );
            return Vec::new();
        }
    };
    steps
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|step| {
            let id = step["id"].as_str()?.trim();
            let brief = step["brief"].as_str()?.trim();
            (!id.is_empty() && !brief.is_empty()).then(|| super::generate::EmbedOffer {
                id: id.to_string(),
                brief: brief.to_string(),
            })
        })
        .collect()
}

/// The built components, by the step id each was requested under.
///
/// Empty whenever the stage has not run, which is the common case and not an
/// error: an article that places no components needs none. What it must not do
/// is guess — a malformed or half-written result reads as empty here and the
/// publish drops the blocks that referenced it, which is the same refusal an
/// unbuilt component gets.
pub fn built(session: &Session) -> std::collections::BTreeMap<String, Value> {
    let path = session.blog_dir().join(JOB_DIR).join(RESULT_JSON);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Default::default();
    };
    let Ok(result) = serde_json::from_str::<Value>(&text) else {
        eprintln!("stream-recorder: {} is not valid JSON", path.display());
        return Default::default();
    };
    result["blocks"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let id = entry["stepId"].as_str()?;
            // The block only, not the wrapper: `stepId` is this side's
            // bookkeeping and would be an unknown key on the component.
            let block = entry.get("block")?;
            block.is_object().then(|| (id.to_string(), block.clone()))
        })
        .collect()
}

fn existing_file(path: Option<&Path>, label: &str) -> Result<PathBuf> {
    let path = path.with_context(|| format!("--{label} is required on first export"))?;
    let path = path
        .canonicalize()
        .with_context(|| format!("reading {label}: {}", path.display()))?;
    if !path.is_file() {
        bail!("{label} must be a file");
    }
    Ok(path)
}

/// Does not overwrite either input on retry. The source ID belongs to the topic.
fn export(job: &Path, topic: &Value, transcript: &str) -> Result<()> {
    if job.exists() {
        let previous: Value = serde_json::from_slice(&std::fs::read(job.join("topic.json"))?)?;
        if previous["sourceId"] != topic["sourceId"] {
            bail!("Existing job belongs to another source ID");
        }
        return Ok(());
    }
    if transcript.trim().is_empty() {
        bail!("No transcript available; provide --transcript");
    }
    // Exclusive directory creation prevents two exporters from writing the same job.
    std::fs::create_dir(job).context("creating component job")?;
    std::fs::write(job.join("transcript.txt"), transcript)?;
    std::fs::write(job.join("topic.json"), serde_json::to_vec_pretty(topic)?)?;
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    if args.source_id.trim().is_empty() {
        bail!("--source-id must not be empty");
    }
    let root = args
        .session
        .canonicalize()
        .context("opening existing session")?;
    if !root.join("drafts").is_dir() {
        bail!("Expected a recorder session with a drafts directory");
    }
    let repo = args
        .landing_repo
        .canonicalize()
        .context("opening landing checkout")?;
    let runner = std::env::var_os("CONTENT_WORKFLOW_RUNNER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/content-job.mjs")
        });
    if !runner.is_file() {
        bail!("Recorder content-job runner not found; set CONTENT_WORKFLOW_RUNNER");
    }
    let session = Session::open_root(root)?;
    let blog = session.blog_dir();
    std::fs::create_dir_all(&blog)?;
    let job = blog.join(JOB_DIR);
    if job.exists() {
        let previous: Value = serde_json::from_slice(&std::fs::read(job.join("topic.json"))?)?;
        if previous["sourceId"] != args.source_id {
            bail!("Existing job belongs to another source ID");
        }
        println!("Using existing topic inputs in {}", job.display());
    } else {
        if args.action == "check" {
            bail!("Prepare or run the component job first");
        }
        let article =
            super::schema::load(&blog).context("Write Article before building components")?;
        let horizontal = existing_file(args.horizontal.as_deref(), "horizontal")?;
        let vertical = existing_file(args.vertical.as_deref(), "vertical")?;
        if horizontal == vertical {
            bail!("Horizontal and vertical outputs must be distinct files");
        }
        let steps_path = existing_file(args.steps.as_deref(), "steps")?;
        let steps: Value = serde_json::from_slice(&std::fs::read(steps_path)?)?;
        let transcript = match &args.transcript {
            Some(path) => std::fs::read_to_string(path).context("reading transcript override")?,
            None => crate::longform::build(&session)
                .chapters
                .iter()
                .map(|chapter| chapter.transcript.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        let topic = json!({
            "version": 1, "sourceId": args.source_id, "title": article.title,
            "slug": article.slug, "destination": args.destination,
            "transcript": "transcript.txt", "videos": {"horizontal": horizontal, "vertical": vertical},
            "steps": steps,
        });
        export(&job, &topic, &transcript)?;
    }
    let node = std::env::var_os("CONTENT_NODE_BIN").unwrap_or_else(|| "node".into());
    let mut command = Command::new(&node);
    command.env("CONTENT_REPO_ROOT", &repo);
    command
        .arg(&runner)
        .arg(&args.action)
        .arg(&job)
        .current_dir(&repo);
    if args.action == "run" {
        command
            .arg("--")
            .arg(&node)
            .arg(runner.with_file_name("content-codex.mjs"));
    }
    let status = command
        .status()
        .context("launching component stage (Node.js required)")?;
    if !status.success() {
        bail!(
            "Component stage failed; review TASK.md and validation.json in {}",
            job.display()
        );
    }
    println!("Component job: {}", job.display());
    if args.action != "prepare" {
        println!(
            "Review components.json, REVIEW.md and validation.json before importing into Strapi."
        );
    }
    Ok(())
}

#[cfg(test)]
mod reading {
    use super::*;

    fn session(tag: &str) -> (PathBuf, Session) {
        let root =
            std::env::temp_dir().join(format!("blog-components-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let session = Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        };
        std::fs::create_dir_all(session.blog_dir()).unwrap();
        (root, session)
    }

    /// The normal article places no components, and asking about them must cost
    /// nothing and say nothing.
    #[test]
    fn a_project_with_no_steps_and_no_job_reads_as_empty() {
        let (root, session) = session("absent");
        assert!(requested(&session).is_empty());
        assert!(built(&session).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The briefs the article is written against, in the file `--steps` takes.
    #[test]
    fn the_requested_steps_are_read_with_their_briefs() {
        let (root, session) = session("steps");
        std::fs::write(
            session.blog_dir().join(STEPS_JSON),
            r#"[{"id":"retry_steps","brief":"  Show the back-off  "},
                {"id":"  ","brief":"no id"},
                {"id":"no_brief","brief":"   "}]"#,
        )
        .unwrap();
        let got = requested(&session);
        assert_eq!(got.len(), 1, "a half-filled step is not a request: {got:?}");
        assert_eq!(got[0].id, "retry_steps");
        assert_eq!(got[0].brief, "Show the back-off");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The block only. `stepId` is this side's bookkeeping and would reach the
    /// CMS as an unknown key on the component.
    #[test]
    fn the_built_blocks_are_keyed_by_step_and_carry_no_bookkeeping() {
        let (root, session) = session("built");
        let job = session.blog_dir().join(JOB_DIR);
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(
            job.join(RESULT_JSON),
            r#"{"version":1,"sourceId":"vd-42","blocks":[
                {"stepId":"retry_steps","block":{"__component":"content.embed",
                 "componentKey":"step_cards","componentVersion":1,"heading":"How",
                 "config":{"steps":[{"title":"Back off","body":"Wait."}]}}}]}"#,
        )
        .unwrap();
        let got = built(&session);
        assert_eq!(got.len(), 1);
        let block = &got["retry_steps"];
        assert_eq!(block["componentKey"], "step_cards");
        assert!(!block.as_object().unwrap().contains_key("stepId"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A half-written result reads as empty rather than as something. The
    /// publish then refuses the article that placed it, which is the same
    /// refusal an unbuilt component gets — and far better than a guess.
    #[test]
    fn a_malformed_result_reads_as_nothing_built() {
        let (root, session) = session("malformed");
        let job = session.blog_dir().join(JOB_DIR);
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join(RESULT_JSON), "{\"blocks\": [{\"stepId\":").unwrap();
        assert!(built(&session).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_preserve_human_inputs_and_reject_different_topics() {
        let root = std::env::temp_dir().join(format!(
            "component-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let job = root.join("job");
        export(&job, &json!({"sourceId":"topic-one"}), "Original").unwrap();
        std::fs::write(job.join("transcript.txt"), "Human revision").unwrap();
        export(&job, &json!({"sourceId":"topic-one"}), "New generation").unwrap();
        assert_eq!(
            std::fs::read_to_string(job.join("transcript.txt")).unwrap(),
            "Human revision"
        );
        assert!(export(&job, &json!({"sourceId":"topic-two"}), "Other").is_err());
    }
}
