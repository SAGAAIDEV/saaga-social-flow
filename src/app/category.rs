//! The Category card on the Project tab — see [`crate::category`].
//!
//! Picking a category is a file write and a repaint. Setting one up — a new
//! Strapi category, a YouTube playlist, the team template entry — is three
//! network calls, so it runs on a thread; when it lands the project it was
//! started from is filed under it, if that project is still the open one.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::App;
use crate::category::{self, Choice, SetUp};
use crate::ui::planning::CategoryView;

#[derive(Default)]
pub(super) struct CategoryState {
    /// The set-up in flight, and the project it was started from.
    job: Option<(PathBuf, Receiver<Result<SetUp, String>>)>,
    /// The card's line: what the last pick or set-up did.
    status: String,
}

impl App {
    pub(super) fn select_project_category(&mut self, root: &str, slug: &str) {
        if self.session.root.to_str() != Some(root) {
            return;
        }
        let library = crate::blog::library::load();
        let team = self.team_template();
        let choice = library
            .categories
            .iter()
            .find(|entry| entry.slug == slug)
            .map(|entry| Choice {
                slug: entry.slug.clone(),
                name: entry.name.clone(),
            })
            // A category the team set up that this Mac's list has not read yet.
            .or_else(|| {
                team.category(slug).map(|known| Choice {
                    slug: known.slug.clone(),
                    name: known.name.clone(),
                })
            });
        self.category.status = match category::save(&self.session.root, choice.as_ref()) {
            Ok(()) => match &choice {
                Some(choice) => {
                    let playlist = team
                        .category(&choice.slug)
                        .is_some_and(|known| !known.playlist_id.is_empty());
                    format!(
                        "Filed under {}.{}",
                        choice.name,
                        if playlist {
                            ""
                        } else {
                            " It has no YouTube playlist yet — press Save below to make one."
                        }
                    )
                }
                None => "No category for this project.".to_string(),
            },
            Err(err) => format!("Could not save the category: {err:#}"),
        };
        self.update_project_view();
        self.update_blog_view();
    }

    pub(super) fn set_up_category(&mut self, root: &str, slug: &str, name: &str, hashtags: &str) {
        if self.session.root.to_str() != Some(root) {
            return;
        }
        if self.category.job.is_some() {
            self.category.status = "Already setting a category up — a moment.".into();
            self.update_project_view();
            return;
        }
        let hashtags = match crate::team::parse_hashtags(hashtags) {
            Ok(tags) => tags,
            Err(err) => {
                self.category.status = format!("Not saved — {err}.");
                self.update_project_view();
                return;
            }
        };
        let existing = (!slug.is_empty())
            .then(|| {
                crate::blog::library::load()
                    .categories
                    .into_iter()
                    .find(|entry| entry.slug == slug)
            })
            .flatten();
        // A slug the list does not have is set up by its remembered name, which
        // finds the row rather than making a second.
        let name = match (&existing, name.trim()) {
            (Some(entry), _) => entry.name.clone(),
            (None, "") if !slug.is_empty() => crate::category::load(&self.session.root)
                .map(|choice| choice.name)
                .unwrap_or_default(),
            (None, name) => name.to_string(),
        };
        let (tx, rx) = mpsc::channel();
        let label = name.clone();
        match std::thread::Builder::new()
            .name("category-set-up".into())
            .spawn(move || {
                let result =
                    category::set_up(&name, existing, hashtags).map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            }) {
            Ok(_) => {
                self.category.job = Some((self.session.root.clone(), rx));
                self.category.status = format!("Setting up {label}…");
            }
            Err(err) => self.category.status = format!("Could not start: {err}"),
        }
        self.update_project_view();
    }

    pub(super) fn drain_category(&mut self) {
        let Some((root, rx)) = self.category.job.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("the category set-up stopped without saying why".into())
            }
        };
        let root = root.clone();
        self.category.job = None;
        self.category.status = match result {
            Ok(set_up) => {
                let choice = Choice {
                    slug: set_up.entry.slug.clone(),
                    name: set_up.entry.name.clone(),
                };
                self.adopt_team_template(set_up.team);
                let filed = match category::save(&root, Some(&choice)) {
                    Ok(()) if root == self.session.root => {
                        format!(" This project is filed under {}.", choice.name)
                    }
                    Ok(()) => " The project it was set up from is filed under it.".to_string(),
                    Err(err) => format!(" Could not file the project under it: {err:#}."),
                };
                let made = match set_up.made.as_slice() {
                    [] => "saved".to_string(),
                    made => format!("made the {}", made.join(" and the ")),
                };
                let warning = set_up
                    .warning
                    .map(|warning| format!(" But: {warning}."))
                    .unwrap_or_default();
                format!("{}: {made}.{filed}{warning}", choice.name)
            }
            Err(err) => format!("Not set up: {err}"),
        };
        self.update_project_view();
        self.update_blog_view();
    }

    /// What the Project tab's Category card shows.
    pub(super) fn category_view(&self) -> CategoryView {
        crate::ui::planning::category_view(
            &self.session.root,
            &crate::blog::library::load(),
            &self.team_template(),
            crate::config::load().blog_category_name,
            self.category.job.is_some(),
            self.team_is_saveable(),
            &self.category.status,
        )
    }
}
