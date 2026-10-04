//! A project's category: the one choice that files the video everywhere.
//!
//! The category is a blog category in Strapi — that list is the taxonomy, and
//! `/blog/category/[slug]` is where it is read. Picking one on the Project tab
//! files the blog post under it, puts the YouTube upload in the category's
//! playlist, and gives the social posts its hashtags. The playlist and the
//! hashtags are the team's, in the team template (see [`crate::team::Category`]);
//! the choice is the project's, in `{project}/category.json`.
//!
//! Setting a category up — new or existing — is [`set_up`]: the Strapi row if
//! there is none, a public playlist if the category has none, and the team
//! template entry that ties them together.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::blog::library::Entry;
use crate::team::{self, Category, Loaded, Saved};

const FILE: &str = "category.json";

/// What a project is filed under. The slug is the key; the name is kept so the
/// tab can say something before the Strapi list has ever been read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub slug: String,
    pub name: String,
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// The project's category, or `None` when none is picked (or the file is
/// unreadable, which is the same thing to every caller: nothing to file under).
pub fn load(root: &Path) -> Option<Choice> {
    let text = std::fs::read_to_string(path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remembers the project's category; `None` clears it.
pub fn save(root: &Path, choice: Option<&Choice>) -> Result<()> {
    let path = path(root);
    match choice {
        Some(choice) => std::fs::write(
            &path,
            serde_json::to_string_pretty(choice).context("serializing the category")? + "\n",
        )
        .with_context(|| format!("writing {}", path.display())),
        None => match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(err).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        },
    }
}

/// The playlist the project's videos go into, if its category has one.
/// Blocking — it reads the team template from S3, falling back to the local copy.
pub fn playlist(root: &Path) -> Option<(String, String)> {
    let choice = load(root)?;
    let team = team::load();
    let category = team.template.category(&choice.slug)?;
    (!category.playlist_id.is_empty())
        .then(|| (category.playlist_id.clone(), category.name.clone()))
}

/// The team entry for the project's category, for the copywriter.
pub fn team_entry(root: &Path, template: &team::TeamTemplate) -> Option<Category> {
    let choice = load(root)?;
    Some(
        template
            .category(&choice.slug)
            .cloned()
            .unwrap_or(Category {
                slug: choice.slug,
                name: choice.name,
                ..Category::default()
            }),
    )
}

/// What [`set_up`] did.
#[derive(Debug)]
pub struct SetUp {
    pub entry: Entry,
    /// The team template as saved.
    pub team: Loaded,
    /// What was made, for the status line: "Strapi category", "YouTube playlist".
    pub made: Vec<&'static str>,
    /// A half that did not happen, said rather than failing the whole: a
    /// category with no playlist yet still files the blog post and the posts.
    pub warning: Option<String>,
}

/// Makes sure a category exists everywhere it drives, and saves its hashtags.
///
/// `existing` is the Strapi row when the category was picked from the list;
/// otherwise the name is looked up and only created when Strapi has no
/// category by that name or slug — "GTM" picked by typing it reuses the row.
/// The playlist is created only when the team template names none, so a second
/// press never makes a second playlist. Blocking — run it on a thread.
pub fn set_up(name: &str, existing: Option<Entry>, hashtags: Vec<String>) -> Result<SetUp> {
    let name = name.trim();
    let mut made = Vec::new();
    let entry = match existing {
        Some(entry) => entry,
        None => {
            if name.is_empty() {
                bail!("type a name for the category");
            }
            let client = crate::blog::strapi::Strapi::from_env()?;
            let slug = crate::blog::schema::slugify(name, 80);
            let library = crate::blog::library::refresh()?;
            match library
                .categories
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(name) || entry.slug == slug)
            {
                Some(entry) => entry.clone(),
                None => {
                    let entry = client.create_category(name)?;
                    made.push("Strapi category");
                    entry
                }
            }
        }
    };

    // Read fresh, so the playlist check sees a teammate's save from a minute ago.
    let loaded = team::load();
    if !matches!(loaded.source, team::Source::Team) {
        bail!(
            "the team template could not be read from S3, so the category cannot be saved for \
             the team — run aws sso login and try again"
        );
    }
    let mut warning = None;
    let playlist_id = match loaded.template.category(&entry.slug) {
        Some(known) if !known.playlist_id.is_empty() => known.playlist_id.clone(),
        _ => match crate::publish::youtube::access_token()
            .and_then(|token| crate::publish::youtube::create_playlist(&token, &entry.name, ""))
        {
            Ok(id) => {
                made.push("YouTube playlist");
                id
            }
            Err(err) => {
                warning = Some(format!(
                    "no YouTube playlist yet ({err:#}) — press Save on the category again once \
                     YouTube is connected"
                ));
                String::new()
            }
        },
    };
    let category = Category {
        slug: entry.slug.clone(),
        name: entry.name.clone(),
        playlist_id,
        hashtags,
    };

    // A teammate's save in between is merged rather than refused: the playlist
    // made above exists either way, and dropping its id would make the next
    // press create another.
    let mut base = loaded;
    for _ in 0..3 {
        let template = base.template.clone().with_category(category.clone());
        match team::save(template, base.etag.as_deref())? {
            Saved::Saved(team) => {
                // So the picker lists a category made here without a Refresh.
                if made.contains(&"Strapi category") {
                    if let Err(err) = crate::blog::library::refresh() {
                        eprintln!("stream-recorder: category list not re-read: {err:#}");
                    }
                }
                return Ok(SetUp {
                    entry,
                    team,
                    made,
                    warning,
                });
            }
            Saved::Conflict => base = team::load(),
        }
    }
    bail!(
        "the team template kept changing under the save — {} is set up but not saved for the \
         team{}; press Save again",
        entry.name,
        match category.playlist_id.as_str() {
            "" => String::new(),
            id => format!(" (its playlist is {id})"),
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-category-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_choice_round_trips_and_clears() {
        let dir = scratch("round-trip");
        let dir = dir.as_path();
        assert_eq!(load(dir), None);
        let seo = Choice {
            slug: "seo-agents".into(),
            name: "SEO Agents".into(),
        };
        save(dir, Some(&seo)).unwrap();
        assert_eq!(load(dir), Some(seo));
        save(dir, None).unwrap();
        assert_eq!(load(dir), None);
        save(dir, None).unwrap();
    }

    #[test]
    fn the_team_entry_falls_back_to_the_choice() {
        let dir = scratch("entry");
        let dir = dir.as_path();
        let template = team::TeamTemplate::default();
        assert_eq!(team_entry(dir, &template), None);
        save(
            dir,
            Some(&Choice {
                slug: "gtm".into(),
                name: "GTM".into(),
            }),
        )
        .unwrap();
        let bare = team_entry(dir, &template).unwrap();
        assert_eq!((bare.name.as_str(), bare.hashtags.len()), ("GTM", 0));
        let template = template.with_category(Category {
            slug: "gtm".into(),
            name: "GTM".into(),
            playlist_id: "PL9".into(),
            hashtags: vec!["#GTM".into()],
        });
        assert_eq!(team_entry(dir, &template).unwrap().playlist_id, "PL9");
    }
}
