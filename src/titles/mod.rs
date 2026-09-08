use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{bail, Result};

pub mod schema;

pub use schema::{load, save, TitlesManifest};

use crate::session::Session;

pub enum TitlesEvent {
    Status(String),
    Ready(PathBuf, TitlesManifest),
}

pub fn spawn_generate_titles(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<TitlesEvent>,
) {
    if let Err(err) = thread::Builder::new()
        .name("titles-gen".into())
        .spawn(
            move || match run(&session, &model, provider.as_deref(), &tx) {
                Ok((path, manifest)) => {
                    let _ = tx.send(TitlesEvent::Ready(path, manifest));
                }
                Err(err) => {
                    let _ = tx.send(TitlesEvent::Status(format!("Titles failed: {err:#}")));
                }
            },
        )
    {
        eprintln!("stream-recorder: could not start titles job: {err}");
    }
}

fn run(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<TitlesEvent>,
) -> Result<(PathBuf, TitlesManifest)> {
    let _ = tx.send(TitlesEvent::Status("Gathering chapter transcripts…".into()));
    let notes = session
        .notes_dir()
        .ok()
        .and_then(|dir| crate::notes::load_notes(&dir).ok());
    let closed = crate::notes::closed_chapter_numbers(&session.dir);
    if closed.is_empty() {
        bail!("no closed chapters to title");
    }
    let chapters: Vec<(u32, String, Option<String>)> = closed
        .into_iter()
        .filter_map(|n| {
            let text = crate::notes::collect_completed(&session.dir)
                .into_iter()
                .find(|(id, _)| *id == n)
                .map(|(_, t)| t)?;
            let hint = notes
                .as_ref()
                .and_then(|d| d.chapters.get(n.saturating_sub(1) as usize))
                .map(|c| c.title.clone());
            Some((n, text, hint))
        })
        .collect();
    if chapters.is_empty() {
        bail!("no completed transcripts to title");
    }
    let _ = tx.send(TitlesEvent::Status(format!(
        "Writing titles for {} chapter(s)…",
        chapters.len()
    )));
    // The given name if there is one — a bare folder timestamp would only be
    // noise in the prompt, so an unnamed project stays generic.
    let project = session.name().unwrap_or_else(|| "Video".into());
    let (manifest, step) = crate::agent::titles::extract_titles(
        &chapters,
        &project,
        session.version,
        model,
        provider,
        Some(&session.root),
    )?;
    let titles_dir = session.titles_dir();
    crate::agent::trace::write_step(&titles_dir, &session.root, &step)?;
    let path = save(&titles_dir, &manifest)?;
    Ok((path, manifest))
}
