//! Substack notes: a post step whose output nobody sends.
//!
//! Every other stage downstream of the render hands its work to a machine —
//! Buffer takes the short cuts, the YouTube Data API takes the longform. This
//! one hands its work to a person. There is no client, no channel resolution and
//! no ledger, because there is nothing that could post twice; the transport is
//! typing.
//!
//! Which is why it is a stage rather than a ninth platform inside `posts.json`.
//! Social copy is graded on being sendable and lives or dies on a character
//! count. These are graded on being typeable *from*, and the two cannot share
//! one preamble: there is a single overlay file per prompt id, so folding them
//! together would mean the Substack voice could never be tuned without also
//! retuning eight social platforms. See [`crate::publish`], which split off the
//! YouTube upload for the same shape of reason.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{bail, Result};

pub mod generate;
pub mod pane;
pub mod schema;

pub use schema::{save, SubstackNotes};

use crate::session::Session;

pub enum SubstackEvent {
    Status(String),
    Ready(PathBuf, SubstackNotes),
    /// Terminal failure. Distinct from a `Status` saying the same words: the app
    /// has to know the thread is gone so it can re-enable the button.
    Failed(String),
}

pub fn spawn_generate(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<SubstackEvent>,
) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("substack-notes".into())
        .spawn(move || match run(&session, &model, provider.as_deref(), &tx) {
            Ok((path, notes)) => {
                eprintln!("stream-recorder: substack notes → {}", path.display());
                let _ = tx.send(SubstackEvent::Ready(path, notes));
            }
            Err(err) => {
                eprintln!("stream-recorder: substack notes failed: {err:#}");
                let _ = tx.send(SubstackEvent::Failed(format!("Notes failed: {err:#}")));
            }
        })
    {
        eprintln!("stream-recorder: could not start the substack job: {err}");
        let _ = unstarted.send(SubstackEvent::Failed(format!(
            "Could not start the notes job: {err}"
        )));
    }
}

fn run(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<SubstackEvent>,
) -> Result<(PathBuf, SubstackNotes)> {
    let status = |msg: String| {
        let _ = tx.send(SubstackEvent::Status(msg));
    };

    status("Reading the longform's transcripts…".into());
    let longform = crate::longform::build(session);
    if longform.chapters.is_empty() {
        bail!(
            "no transcribed chapters in {} — record one and let it transcribe first",
            session.dir.display()
        );
    }
    status(format!(
        "Writing notes from {} chapter(s) via {model}…",
        longform.chapters.len()
    ));

    let (notes, step) = generate::generate_notes(
        &longform,
        session.version,
        model,
        provider,
        Some(&session.root),
    )?;

    let dir = session.substack_dir();
    let path = save(&dir, &notes)?;
    // Traced like every other generative step, so Reflect can read what this
    // prompt was given and what it returned.
    crate::agent::trace::write_step(&dir, &session.root, &step)?;
    Ok((path, notes))
}

/// Opens the standing prompt for editing, creating it from the builtin first if
/// it is not there yet.
///
/// Not seeded automatically on the first run, deliberately. Copying the builtin
/// into a file freezes it — later improvements to the shipped default stop
/// reaching whoever has one — which is exactly the failure `config::RETIRED`
/// exists to undo. Freezing it is a reasonable choice; it just has to be a
/// choice, so it costs a click.
pub fn edit_prompt() -> Result<PathBuf> {
    let path = crate::agent::prompt::ensure_library_overlay(
        crate::agent::prompt::SUBSTACK,
        generate::SYSTEM_PROMPT,
    )?;
    // `-t` is "the default text editor", so this never opens a .txt in whatever
    // last claimed the extension.
    let opened = std::process::Command::new("open")
        .arg("-t")
        .arg(&path)
        .status();
    if let Err(err) = opened {
        eprintln!("stream-recorder: could not open {}: {err}", path.display());
    }
    Ok(path)
}
