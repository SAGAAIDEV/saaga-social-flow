//! Thumbnail stage: a still, a screen grab, words you wrote, one choice.
//!
//! Runs inside Render but alongside it, not after: a thumbnail needs the camera
//! still, which exists before any composition renders, so it has no reason to
//! wait for a pool slot it would only block.
//!
//! There is no language model in this stage. A [`Brief`] is two boxes you type
//! into, and Generate sends them straight to the image model with the still, the
//! screen grab and the active references. A drafted prompt is one you then have
//! to read, disagree with and edit — three steps to reach where typing it starts,
//! and every draft overwrote the edit before it.
//!
//! Each Generate press adds fresh candidates while retaining previous formats
//! and the active selection. Format and the finished prompt are part of their
//! identity; no recording or video re-render is needed.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

use crate::session::Session;

pub mod brief;
pub mod format;
mod generation;
pub mod image;
pub mod pane;
pub mod references;
pub mod schema;
pub mod still;

pub const BRIEF_JSON: &str = "thumbnails/brief.json";

pub use brief::Brief;

/// The brief as stored.
///
/// It carried a `prompt_version` when a language model wrote it — which overlay
/// produced the draft, for Reflect to read an edit against. Nothing drafts it
/// now, so there is no preamble to attribute it to.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedBrief {
    #[serde(default)]
    pub brief: Brief,
    #[serde(default)]
    pub written_at: String,
}

impl SavedBrief {
    /// Exactly what the image model is sent.
    pub fn prompt(&self) -> String {
        self.brief.render()
    }
}

pub enum ThumbnailEvent {
    Status(String),
    /// Generation finished: how many were drawn this run, and how many exist now.
    ///
    /// Both, because they answer different questions. `made` is whether the press
    /// did anything; `total` is what the pane is about to show. Reporting only the
    /// total made a run that skipped every candidate — the normal outcome when
    /// nothing about the brief changed — read exactly like one that drew a full
    /// set, which is indistinguishable from the button being broken.
    Generated { made: usize, total: usize },
    /// A dropped file, decoded and downscaled and ready to store.
    ///
    /// The library write deliberately does *not* happen on the worker: adding a
    /// reference also switches it on, and `selection.json` is read-modify-write.
    /// Dropping three files at once is three of these, and doing that write back
    /// on the main thread is what stops two of them from losing each other.
    Prepared { name: String, bytes: Vec<u8> },
    PortraitSaved { root: PathBuf },
    Failed(String),
}

pub fn brief_path(session: &Session) -> PathBuf {
    session.root.join(BRIEF_JSON)
}

/// This project's brief, if it has one. The project file and nothing else.
pub fn load_brief(session: &Session) -> Option<SavedBrief> {
    let text = std::fs::read_to_string(brief_path(session)).ok()?;
    serde_json::from_str(&text).ok()
}

/// This project's brief, or the standing default to start one from.
///
/// The project's own file wins whenever it exists — a thumbnail is about that
/// video. The fallback exists because the house style in a brief is not
/// per-video, and without it every new project began with two empty boxes and
/// the same paragraph retyped from memory.
///
/// Separate from [`load_brief`] rather than folded into it because that one
/// touches a single project file and this one needs a house default from
/// outside it.
///
/// `fallback` is passed in rather than read from global config here, so that
/// everything below this line is a pure function of its arguments. Reading
/// `~/.stream-recorder/config.json` in here made every test of a caller depend
/// on the developer's own config — the pane tests asserted an empty brief and
/// failed on any machine whose owner had ever saved a house style. The two
/// production callers read the config themselves; tests pass what they mean.
pub fn load_brief_or_default(session: &Session, fallback: Brief) -> Option<SavedBrief> {
    if let Some(saved) = load_brief(session) {
        return Some(saved);
    }
    (!fallback.is_empty()).then(|| SavedBrief {
        brief: fallback,
        // Deliberately blank: nothing has been written *for this project* yet.
        // Dating it from the config would make an untouched project look edited.
        written_at: String::new(),
    })
}

pub fn save_brief(session: &Session, saved: &SavedBrief) -> Result<()> {
    let path = brief_path(session);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(saved).context("serializing the brief")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))
}

/// Keeps this brief as the starting point for the next project.
///
/// Called by the app when a human saves one, not by [`save_brief`] — writing the
/// user's global config is a decision about a preference, and burying it in the
/// function that persists a project file would mean every test of that function
/// rewrote the config of whoever ran the suite.
///
/// Never fatal: the project's own copy is already on disk by the time this runs,
/// so failing to update a convenience default must not report the save as failed.
pub fn remember_brief(brief: &Brief) {
    if brief.is_empty() {
        return;
    }
    let mut cfg = crate::config::load();
    if cfg.thumbnail.brief == *brief {
        return;
    }
    cfg.thumbnail.brief = brief.clone();
    if let Err(err) = crate::config::save(&cfg) {
        eprintln!("stream-recorder: could not remember the thumbnail brief: {err:#}");
    }
}

/// Draws candidates for the brief on disk.
#[tracing::instrument(skip_all)]
pub fn spawn(
    session: Session,
    models: Vec<image::ModelSpec>,
    per_model: usize,
    tx: Sender<ThumbnailEvent>,
) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("thumbnail".into())
        .spawn(move || {
            let outcome = generation::run(&session, &models, per_model, &tx);
            match outcome {
                Ok((made, total)) => {
                    eprintln!("stream-recorder: {made} new thumbnail candidate(s), {total} in all");
                    let _ = tx.send(ThumbnailEvent::Generated { made, total });
                }
                Err(err) => {
                    eprintln!("stream-recorder: thumbnail failed: {err:#}");
                    let _ = tx.send(ThumbnailEvent::Failed(format!("Thumbnail failed: {err:#}")));
                }
            }
        })
    {
        eprintln!("stream-recorder: could not start thumbnail job: {err}");
        // Kept back from the closure so a thread that never starts still clears
        // the busy flag — otherwise the button stays off until a restart.
        let _ = unstarted.send(ThumbnailEvent::Failed(format!(
            "Could not start the thumbnail job: {err}"
        )));
    }
}

/// Decodes and downscales a dropped image away from the main thread.
///
/// A 10 MB drop is a Core Image decode, a scale and a JPEG encode, and it used to
/// run inside the event handler — so dropping a photo froze the window, and it
/// froze it hardest when a render and a thumbnail job were already competing for
/// the machine.
pub fn spawn_prepare_reference(name: String, data_url: String, tx: Sender<ThumbnailEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("thumbnail-reference".into())
        .spawn(move || {
            let prepared = image::decode_data_url(&data_url)
                .and_then(|bytes| references::prepare(&name, &bytes));
            let _ = match prepared {
                Ok(bytes) => tx.send(ThumbnailEvent::Prepared { name, bytes }),
                Err(err) => tx.send(ThumbnailEvent::Failed(format!(
                    "{name} could not be read: {err:#}"
                ))),
            };
        })
    {
        eprintln!("stream-recorder: could not start reference job: {err}");
        let _ = unstarted.send(ThumbnailEvent::Failed(format!(
            "Could not start the reference job: {err}"
        )));
    }
}

/// Decode and store an imported portrait without blocking the UI thread.
pub fn spawn_portrait(root: PathBuf, data: String, tx: Sender<ThumbnailEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new().name("portrait-import".into()).spawn(move || {
        let result = image::decode_data_url(&data)
            .and_then(|bytes| still::shrink(&bytes, 2048.0))
            .and_then(|bytes| {
                let path = still::write_bytes(&root, &bytes)?;
                // Selecting a previously imported photo makes it newest again.
                std::fs::write(&path, bytes)?;
                Ok(())
            });
        let event = match result {
            Ok(()) => ThumbnailEvent::PortraitSaved { root },
            Err(err) => ThumbnailEvent::Failed(format!("Could not import photo: {err:#}")),
        };
        let _ = tx.send(event);
    }) { let _ = unstarted.send(ThumbnailEvent::Failed(format!("Could not start photo import: {err}"))); }
}

/// Records that a candidate is now the live thumbnail.
pub fn activate(session: &Session, id: &str) -> Result<()> {
    schema::append(
        &session.root,
        &schema::Row::Activated {
            id: id.to_string(),
            at: crate::schedule::ledger::now_rfc3339(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> Brief {
        Brief {
            title: "SHIP IT".into(),
            description: "Presenter in a studio, editor behind".into(),
        }
    }

    #[test]
    fn a_brief_round_trips_with_its_provenance() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-brief-{}-rt",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        };
        assert!(load_brief(&session).is_none());

        let saved = SavedBrief {
            brief: brief(),
            written_at: "2026-08-15T09:00:00Z".into(),
        };
        save_brief(&session, &saved).unwrap();
        let back = load_brief(&session).unwrap();
        assert_eq!(back, saved);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Editing the brief must change what the candidates are keyed on, or a
    /// regenerate would silently return the old pictures.
    #[test]
    fn editing_the_brief_changes_every_candidate_id() {
        // Hashed the way `run` does it: from the finished prompt, so this test
        // moves with the code rather than agreeing with a copy of it.
        let hash = |brief: &Brief| {
            crate::agent::prompt::hash_of(
                &SavedBrief {
                    brief: brief.clone(),
                    written_at: "now".into(),
                }
                .prompt(),
            )
        };
        let before = hash(&brief());
        let mut edited = brief();
        edited.description = "Something else entirely".into();
        let after = hash(&edited);
        assert_ne!(before, after);
        assert_ne!(
            schema::candidate_id("m", &before, "still", "", "refs", 0),
            schema::candidate_id("m", &after, "still", "", "refs", 0)
        );
    }

    /// `load_brief` reads the project and nothing else, which is what keeps every
    /// test of it — and of the pane above it — independent of whoever's machine
    /// is running the suite.
    #[test]
    fn loading_this_projects_brief_never_consults_global_config() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-brief-scope-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let session = Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        };
        assert!(
            load_brief(&session).is_none(),
            "a project with no brief has none, whatever the config says"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An empty brief is not a default worth keeping, and the guard returns
    /// before it would read or write global config — which is why this is safe
    /// to assert at all.
    #[test]
    fn an_empty_brief_is_never_remembered() {
        remember_brief(&Brief::default());
        remember_brief(&Brief { title: "  ".into(), description: "\n".into() });
    }
}
