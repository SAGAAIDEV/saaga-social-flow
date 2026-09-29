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
    if let Err(err) = thread::Builder::new()
        .name("posts-gen".into())
        .spawn(move || {
            match run_posts_generation(
                &session,
                &model,
                provider.as_deref(),
                custom_prompt.as_deref(),
                &tx,
            ) {
                Ok((path, manifest, warning)) => {
                    eprintln!("stream-recorder: posts ready → {}", path.display());
                    let _ = tx.send(PostsEvent::Ready(path, manifest));
                    if let Some(warning) = warning {
                        eprintln!("stream-recorder: {warning}");
                        let _ = tx.send(PostsEvent::Status(warning));
                    }
                }
                Err(err) => {
                    eprintln!("stream-recorder: posts generation failed: {err:#}");
                    let _ = tx.send(PostsEvent::Status(format!(
                        "Post generation failed: {err:#}"
                    )));
                }
            }
        })
    {
        eprintln!("stream-recorder: could not start posts generation thread: {err}");
    }
}

fn run_posts_generation(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    custom_prompt: Option<&str>,
    tx: &Sender<PostsEvent>,
) -> Result<(PathBuf, PostsManifest, Option<String>)> {
    let _ = tx.send(PostsEvent::Status(
        "Gathering project transcripts & render outputs…".into(),
    ));

    let notes = session
        .notes_dir()
        .ok()
        .and_then(|dir| crate::notes::load_notes(&dir).ok());

    let mut contexts = collect_video_contexts(&session.dir, notes.as_ref());
    // The approved plan's CTA chapter is never rendered as a short, so it is
    // not given copy either: a post for it would sit in the schedule as "not on
    // S3 yet — press Upload to S3", and pressing it would never send one.
    if let Some(n) = crate::plan::cta_chapter_of(session) {
        let cta = format!("chapter-{n:02}");
        contexts.retain(|video| video.id != cta);
    }
    publication::enrich(session, &mut contexts);

    if contexts.is_empty() {
        bail!(
            "no video or transcript contexts found in {}",
            session.dir.display()
        );
    }

    let project_title = session
        .name()
        .or_else(|| notes.as_ref().map(|n| n.title.clone()))
        .unwrap_or_else(|| "Video Project".into());

    // Read fresh for every run, so a link a teammate saved a minute ago is
    // in this one; the local copy stands in when S3 cannot be read.
    let team = crate::team::load();
    let funnel = team.template.links.clone();
    let _ = tx.send(PostsEvent::Status(format!(
        "Generating posts for {} video(s) via {model}{}…",
        contexts.len(),
        match (&team.source, funnel.len()) {
            (_, 0) => String::new(),
            (crate::team::Source::Cached(_), n) =>
                format!(" with {n} funnel link(s) from this Mac's copy"),
            (_, n) => format!(" with {n} funnel link(s)"),
        }
    )));

    let (manifest, step) = generate_posts(
        &contexts,
        &project_title,
        session.version,
        model,
        provider,
        custom_prompt,
        &funnel,
        Some(&session.root),
        None,
    )?;

    let posts_dir = session.posts_dir();
    let json_path = save_manifest(&posts_dir, &manifest)?;
    crate::agent::trace::write_step(&posts_dir, &session.root, &step)?;

    // A URL the copy carries that nobody gave the model was invented or
    // mangled. Said after Ready, so it is the line left on screen.
    let allowed = allowed_urls(&contexts, &funnel);
    let unknown: Vec<String> = manifest
        .items
        .iter()
        .flat_map(|video| video.posts.iter())
        .flat_map(|post| crate::team::unknown_urls(&post.content, &allowed))
        .collect();
    let warning = (!unknown.is_empty()).then(|| {
        format!(
            "Posts saved, but check these links — they are not the team's or this video's: {}",
            unknown.join(", ")
        )
    });

    Ok((json_path, manifest, warning))
}

/// The URLs a post may carry: the team's funnel links, and the published
/// video and article URLs `publication::enrich` wrote into the points.
fn allowed_urls(contexts: &[generate::VideoContext], funnel: &[crate::team::Link]) -> Vec<String> {
    let mut allowed: Vec<String> = funnel.iter().map(|link| link.url.clone()).collect();
    for video in contexts {
        for point in &video.points {
            allowed.extend(
                point
                    .split_whitespace()
                    .filter(|word| word.starts_with("http://") || word.starts_with("https://"))
                    .map(str::to_string),
            );
        }
    }
    allowed
}
