use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{bail, Result};

pub mod generate;
mod publication;
pub mod schema;
pub mod view;

pub use generate::{collect_video_contexts, generate_posts};
pub use schema::{load_manifest, save_manifest, PostsManifest};
pub use view::PostsForm;

use crate::session::Session;

pub enum PostsEvent {
    Status(String),
    Ready(PathBuf, PostsManifest),
}

pub fn spawn_generate_posts(
    session: Session,
    model: String,
    provider: Option<String>,
    custom_prompt: Option<String>,
    tx: Sender<PostsEvent>,
) {
    if let Err(err) = thread::Builder::new().name("posts-gen".into()).spawn(move || {
        match run_posts_generation(&session, &model, provider.as_deref(), custom_prompt.as_deref(), &tx) {
            Ok((path, manifest)) => {
                eprintln!("stream-recorder: posts ready → {}", path.display());
                let _ = tx.send(PostsEvent::Ready(path, manifest));
            }
            Err(err) => {
                eprintln!("stream-recorder: posts generation failed: {err:#}");
                let _ = tx.send(PostsEvent::Status(format!("Post generation failed: {err:#}")));
            }
        }
    }) {
        eprintln!("stream-recorder: could not start posts generation thread: {err}");
    }
}

fn run_posts_generation(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    custom_prompt: Option<&str>,
    tx: &Sender<PostsEvent>,
) -> Result<(PathBuf, PostsManifest)> {
    let _ = tx.send(PostsEvent::Status("Gathering project transcripts & render outputs…".into()));

    let notes = session
        .notes_dir()
        .ok()
        .and_then(|dir| crate::notes::load_notes(&dir).ok());

    let mut contexts = collect_video_contexts(&session.dir, notes.as_ref());
    publication::enrich(session, &mut contexts);

    if contexts.is_empty() {
        bail!("no video or transcript contexts found in {}", session.dir.display());
    }

    let project_title = session
        .name()
        .or_else(|| notes.as_ref().map(|n| n.title.clone()))
        .unwrap_or_else(|| "Video Project".into());

    let _ = tx.send(PostsEvent::Status(format!(
        "Generating posts for {} video(s) via {model}…",
        contexts.len()
    )));

    let (manifest, step) = generate_posts(
        &contexts,
        &project_title,
        session.version,
        model,
        provider,
        custom_prompt,
        Some(&session.root),
        None,
    )?;

    let posts_dir = session.posts_dir();
    let json_path = save_manifest(&posts_dir, &manifest)?;
    crate::agent::trace::write_step(&posts_dir, &session.root, &step)?;

    Ok((json_path, manifest))
}
