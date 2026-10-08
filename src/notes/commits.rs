//! The Speaking notes, written from your GitHub commits since the last project.
//!
//! Prep for the take rather than a teleprompter from a rehearsal: press
//! Summarize Commits before recording and the pane shows what you shipped as
//! talking points. Written beside the transcript deck (`notes/commits/`), so
//! building one never overwrites the other. See [`crate::github`] for the login
//! and the search.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{bail, Result};

use super::{deck, NotesEvent};
use crate::session::Session;

/// Build the commits deck on a worker thread. Sends status lines, then the page.
pub fn spawn_commit_notes(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<NotesEvent>,
) {
    let unstarted = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("commit-notes".into())
        .spawn(
            move || match build(&session, &model, provider.as_deref(), &tx) {
                Ok(html) => {
                    eprintln!("stream-recorder: commit notes ready → {}", html.display());
                    let _ = tx.send(NotesEvent::Ready(html));
                }
                Err(err) => {
                    eprintln!("stream-recorder: commit notes failed: {err:#}");
                    let _ = tx.send(NotesEvent::Status(format!(
                        "Commit summary failed: {err:#}"
                    )));
                }
            },
        );
    if let Err(err) = spawned {
        let _ = unstarted.send(NotesEvent::Status(format!(
            "Could not start the commit summary: {err}"
        )));
    }
}

fn build(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<NotesEvent>,
) -> Result<PathBuf> {
    let status = |msg: String| {
        let _ = tx.send(NotesEvent::Status(msg));
    };
    let token = match crate::github::token() {
        Some(token) => token.value,
        None => match crate::github::login() {
            crate::github::Login::Unavailable(why) => bail!("{why}"),
            crate::github::Login::DeviceFlow { client_id } => {
                let code = crate::github::start_device_login(&client_id)?;
                status(format!(
                    "Sign in to GitHub: enter {} at {} (opened in your browser). Waiting…",
                    code.user_code, code.verification_uri
                ));
                let _ = std::process::Command::new("open")
                    .arg(&code.verification_uri)
                    .status();
                let token = crate::github::finish_device_login(&client_id, &code)?;
                // Saved on the main thread, which owns the environment.
                let _ = tx.send(NotesEvent::GitHubToken(token.clone()));
                token
            }
        },
    };
    let login = crate::github::whoami(&token)?;
    let since = crate::github::since_last_project(&session.root);
    let since_label = since.format("%Y-%m-%d %H:%M UTC").to_string();
    status(format!("Finding {login}'s commits since {since_label}…"));
    let emails = crate::github::author_emails(&token);
    let commits = crate::github::commits_since(&token, &login, &emails, since)?;
    if commits.is_empty() {
        bail!(
            "no commits by {login} since {since_label} on any default branch GitHub can see \
             — work on a feature branch shows up once it merges, and a repo that commits as \
             another email needs it under Settings → GitHub"
        );
    }
    status(format!(
        "Summarizing {} commit(s) via {model}…",
        commits.len()
    ));
    let prompt = crate::github::as_prompt(&login, &since_label, &commits);
    let title = format!("What I shipped since {}", since.format("%b %-d"));
    let (data, step) = crate::agent::notes::extract_commit_notes(
        &prompt,
        &title,
        model,
        provider,
        Some(&session.root),
    )?;
    let dir = session.notes_dir()?.join("commits");
    std::fs::create_dir_all(&dir)?;
    // The raw list beside the deck, so a summary can be checked against what
    // it was written from.
    std::fs::write(
        dir.join("commits.json"),
        serde_json::to_string_pretty(&commits)? + "\n",
    )?;
    crate::agent::trace::write_step(&dir, &session.root, &step)?;
    deck::write(&dir, &data)
}
