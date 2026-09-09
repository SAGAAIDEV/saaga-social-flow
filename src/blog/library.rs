//! The CMS's own lists — authors and categories — cached on disk.
//!
//! These are properties of the Strapi instance, not of a project, so they live
//! beside [`crate::config`] at `~/.stream-recorder/` rather than under the
//! recording. Every project posts to the same CMS and would otherwise re-fetch
//! the same seven authors.
//!
//! Cached rather than fetched per repaint for two reasons. The Blog pane is
//! rebuilt on every stage change, and a network call on that path would stall
//! the UI thread each time. And the pane has to render with no network at all —
//! on a plane, or with the token unset — where the last known list is a far more
//! useful thing to show than an empty dropdown.
//!
//! The cache is therefore refreshed only when asked (the pane's Refresh button),
//! and it is never the authority on what exists: the id it hands to
//! [`super::payload`] is checked by Strapi on create, and a stale row fails
//! loudly there rather than silently posting under the wrong byline.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One selectable row: an author, or a category.
///
/// `id` is the numeric relation id, which is what a `video-post` actually
/// stores. `slug` is carried for display and for matching an env override, and
/// `detail` is the row's one-line context — `jobTitle` for an author,
/// `description` for a category — so the dropdown can distinguish two people
/// with the same name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Entry {
    /// What the dropdown shows. The detail disambiguates the duplicate names the
    /// live collection actually has.
    pub fn label(&self) -> String {
        match self.detail.as_deref() {
            Some(detail) if !detail.is_empty() => {
                // A bio-length description would blow out the dropdown, so it is
                // the first clause only.
                let short: String = detail.chars().take(48).collect();
                let short = short.trim_end();
                if detail.chars().count() > 48 {
                    format!("{} — {short}…", self.name)
                } else {
                    format!("{} — {short}", self.name)
                }
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Library {
    /// When this was last pulled, so a stale list is visible as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    /// The CMS these were read from.
    ///
    /// An id is a row in one database. This cache was once filled from a
    /// Strapi on `localhost:1337` — author 3 "Andrew", category 4
    /// "ai-seo-automation" — and then read, with `STRAPI_API_URL` unset again,
    /// to publish to `cms.saagasolve.com`, where author 3 is an unpublished row
    /// of someone else and category 4 a different category. So [`load`] keeps
    /// the rows only while the configured CMS is the one they came from; the
    /// pane says where they came from instead. Absent on a cache written before
    /// this was recorded, which is trusted as it always was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default)]
    pub authors: Vec<Entry>,
    /// The one taxonomy a video post carries, shared with `blog-articles`.
    /// Was `education_categories` until that collection was retired; a cache
    /// written before the rename simply reads back empty, which is what
    /// Refresh is for.
    #[serde(default)]
    pub categories: Vec<Entry>,
}

impl Library {
    pub fn author(&self, id: i64) -> Option<&Entry> {
        self.authors.iter().find(|entry| entry.id == id)
    }

    pub fn category(&self, id: i64) -> Option<&Entry> {
        self.categories.iter().find(|entry| entry.id == id)
    }

    /// The CMS this was read from, when it is not `base` — the one configured
    /// now. `None` when they agree, or when the cache predates recording it.
    pub fn read_elsewhere(&self, base: &str) -> Option<&str> {
        self.base.as_deref().filter(|from| !same_cms(from, base))
    }

    /// The id for a name, case-insensitively — how an env override or a config
    /// written before ids existed still resolves.
    ///
    /// `None` when the name is ambiguous as well as when it is absent: two
    /// authors share a name in the live CMS, and quietly picking the first is
    /// how a post ends up under the wrong byline.
    pub fn by_name<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let mut matches = entries.iter().filter(|entry| {
            entry.name.eq_ignore_ascii_case(name) || entry.slug.eq_ignore_ascii_case(name)
        });
        let first = matches.next()?;
        match matches.next() {
            None => Some(first),
            Some(_) => {
                eprintln!(
                    "stream-recorder: more than one entry named {name:?} in strapi — pick one \
                     explicitly on the Blog tab"
                );
                None
            }
        }
    }
}

fn path() -> Result<PathBuf> {
    let dir = dirs_home()
        .context("no home directory")?
        .join(".stream-recorder");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join("strapi-library.json"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The cache, or an empty one. Never an error: a missing or corrupt cache means
/// "press Refresh", not a broken tab.
pub fn load() -> Library {
    let Ok(path) = path() else {
        return Library::default();
    };
    let cached = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    quarantine(cached, &super::strapi::configured_base())
}

/// The cache with its rows removed when they came from another CMS.
///
/// The provenance stays, so the pane can say where the rows went rather than
/// showing an empty list with no reason; only the ids go, because an id from
/// elsewhere is the one thing here that is worse than nothing.
fn quarantine(mut cached: Library, base: &str) -> Library {
    if cached.read_elsewhere(base).is_some() {
        cached.authors.clear();
        cached.categories.clear();
    }
    cached
}

/// Whether two base URLs name the same CMS: the same host, however it was
/// typed. `http://localhost:1337/` and `http://localhost:1337` agree;
/// `https://cms.saagasolve.com` and `http://localhost:1337` do not.
pub fn same_cms(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

pub fn save(library: &Library) -> Result<()> {
    let path = path()?;
    let text = serde_json::to_string_pretty(library).context("serializing the strapi library")?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// Pulls both lists and replaces the cache.
///
/// Both or neither: a half-written cache — new authors beside last week's
/// categories, under one `fetched_at` — reads as fresher than it is.
pub fn refresh() -> Result<Library> {
    let client = super::strapi::Strapi::from_env()?;
    let authors = client.list_authors()?;
    let categories = client.list_categories()?;
    let library = Library {
        fetched_at: Some(crate::schedule::ledger::now_rfc3339()),
        base: Some(client.base().to_string()),
        authors,
        categories,
    };
    save(&library)?;
    Ok(library)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, name: &str, detail: Option<&str>) -> Entry {
        Entry {
            id,
            name: name.into(),
            slug: name.to_lowercase().replace(' ', "-"),
            detail: detail.map(str::to_string),
        }
    }

    /// The live CMS really does have two "Danish Rafique" rows. Resolving a
    /// byline by name has to refuse rather than guess, because both guesses look
    /// identical in the pane and only one is the person who wrote it.
    #[test]
    fn a_duplicated_name_does_not_resolve() {
        let authors = vec![
            entry(11, "Danish Rafique", Some("Senior SEO Content Strategist")),
            entry(25, "Danish Rafique", Some("SEO Content Strategist")),
            entry(12, "Ahmed Raza", None),
        ];
        assert!(Library::by_name(&authors, "Danish Rafique").is_none());
        assert_eq!(Library::by_name(&authors, "Ahmed Raza").unwrap().id, 12);
    }

    #[test]
    fn a_name_matches_case_insensitively_and_by_slug() {
        let rows = vec![entry(2, "Education", Some("Educational video content"))];
        assert_eq!(Library::by_name(&rows, "education").unwrap().id, 2);
        assert_eq!(Library::by_name(&rows, "EDUCATION").unwrap().id, 2);
        assert!(Library::by_name(&rows, "").is_none());
        assert!(Library::by_name(&rows, "Tutorials").is_none());
    }

    /// The dropdown has to fit a row: author bios in the live CMS run to several
    /// hundred characters and would otherwise be the whole line.
    #[test]
    fn a_long_detail_is_cut_but_a_short_one_is_not() {
        let short = entry(1, "Hamza Malik", Some("SEO Content Writer"));
        assert_eq!(short.label(), "Hamza Malik — SEO Content Writer");

        let long = entry(2, "A", Some(&"x".repeat(200)));
        assert!(long.label().ends_with('…'), "{}", long.label());
        assert!(long.label().chars().count() < 60);

        assert_eq!(entry(3, "No Detail", None).label(), "No Detail");
    }

    #[test]
    fn ids_are_looked_up_on_the_cached_lists() {
        let library = Library {
            fetched_at: None,
            base: None,
            authors: vec![entry(12, "Ahmed Raza", None)],
            categories: vec![entry(2, "AI Powered Marketing", None)],
        };
        assert_eq!(library.author(12).unwrap().name, "Ahmed Raza");
        assert!(library.author(99).is_none());
        assert_eq!(library.category(2).unwrap().name, "AI Powered Marketing");
    }
}

#[cfg(test)]
mod live {
    /// Hits the real CMS. Ignored by default; run with
    /// `cargo test blog::library::live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_lists_come_back_from_strapi() {
        crate::load_dotenv();
        let library = super::refresh().expect("refresh");
        println!("fetched_at {:?}", library.fetched_at);
        for author in &library.authors {
            println!("  author {:>3}  {}", author.id, author.label());
        }
        for category in &library.categories {
            println!("  category {:>3}  {}", category.id, category.label());
        }
        assert!(!library.authors.is_empty(), "the CMS has authors");
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    fn from(base: Option<&str>) -> Library {
        Library {
            fetched_at: Some("2026-09-08T17:55:01Z".into()),
            base: base.map(str::to_string),
            authors: vec![Entry {
                id: 3,
                name: "Andrew".into(),
                slug: "author".into(),
                detail: None,
            }],
            categories: vec![Entry {
                id: 4,
                name: "ai-seo-automation".into(),
                slug: "ai-seo-automation".into(),
                detail: None,
            }],
        }
    }

    /// The cache that caused it: read from a local Strapi, then consulted for
    /// a publish to production. The rows go; where they came from stays, so the
    /// pane can say so.
    #[test]
    fn rows_read_from_another_cms_are_dropped_but_their_origin_kept() {
        let kept = quarantine(
            from(Some("http://localhost:1337")),
            "https://cms.saagasolve.com",
        );
        assert!(kept.authors.is_empty() && kept.categories.is_empty());
        assert_eq!(
            kept.read_elsewhere("https://cms.saagasolve.com"),
            Some("http://localhost:1337")
        );
        assert!(kept.author(3).is_none());
    }

    #[test]
    fn rows_from_the_configured_cms_are_kept_however_the_url_was_typed() {
        let kept = quarantine(
            from(Some("https://cms.saagasolve.com/")),
            "https://CMS.saagasolve.com",
        );
        assert_eq!(kept.authors.len(), 1);
        assert!(kept.read_elsewhere("https://cms.saagasolve.com").is_none());
    }

    /// A cache written before the origin was recorded is trusted as before:
    /// refusing it would empty every existing install's dropdowns for nothing.
    #[test]
    fn a_cache_with_no_recorded_origin_is_kept() {
        let kept = quarantine(from(None), "https://cms.saagasolve.com");
        assert_eq!(kept.authors.len(), 1);
        assert!(kept.read_elsewhere("anything").is_none());
    }

    #[test]
    fn the_origin_round_trips_through_the_file_format() {
        let text = serde_json::to_string(&from(Some("http://localhost:1337"))).unwrap();
        assert!(
            text.contains("\"base\":\"http://localhost:1337\""),
            "{text}"
        );
        let back: Library = serde_json::from_str(&text).unwrap();
        assert_eq!(back.base.as_deref(), Some("http://localhost:1337"));
        // And the old shape, without it, still reads.
        let old: Library =
            serde_json::from_str(r#"{"fetched_at":"x","authors":[],"categories":[]}"#).unwrap();
        assert!(old.base.is_none());
    }
}
