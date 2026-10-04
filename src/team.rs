//! The team's template: the funnel links every post can send readers to, and
//! the footer every YouTube description ends with.
//!
//! ## Funnel links
//!
//! The published video and article are linked already — see
//! `posts::publication` — but those are this video's. These are the
//! standing destinations every post can point to: the site, the newsletter,
//! the template repo, the booking page. The copywriter is given them with what
//! each is for, told to send each post to the one that fits, and to copy the
//! URL exactly; [`unknown_urls`] then checks that it did.
//!
//! ## The YouTube footer
//!
//! Text added to the end of every generated YouTube description, with
//! `{links}` standing for the funnel links as `Label: URL` lines — so the
//! description carries the same funnel as the posts without anyone pasting it.
//! It is added when the copy is written, so what the YouTube tab shows is what
//! goes up, and an edit there is the final word.
//!
//! ## Shared through S3
//!
//! The template is the team's, not one machine's, so it lives in the team bucket
//! at [`KEY`] — outside the public prefix, sent with no ACL (see
//! `distribute::s3::read_team_object`). Everyone who can upload a render can
//! already read and write there, so there is nothing new to set up.
//!
//! A save is conditional on the file being what this machine last read, so
//! two people editing at once cannot silently overwrite each other: the second
//! is told to reload. A copy of the last list read is kept at
//! `~/.stream-recorder/team/templates.json`, so posts still get the links
//! with no network or an expired login.
//!
//! ## Typed as lines
//!
//! One link per line, `Label — https://… — when to use it`. The URL is found
//! wherever it is on the line, so `-`, `|` or no separator at all works too;
//! what comes before it is the label and what comes after is the note.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The team file's key, at the bucket root.
pub const KEY: &str = "team/templates.json";
/// The footer's placeholder for the links.
pub const LINKS_PLACEHOLDER: &str = "{links}";
/// A footer is a sign-off, not a second description.
pub const MAX_FOOTER_CHARS: usize = 1500;
/// What YouTube accepts in a description.
pub const MAX_DESCRIPTION_CHARS: usize = 5000;
/// Enough for a real funnel; past this the prompt is a link dump.
pub const MAX_LINKS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    pub url: String,
    /// When to send people here: "founders who want to try it", "newsletter
    /// sign-up at the end of a tutorial".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// One category, as the rest of the funnel sees it.
///
/// The blog category in Strapi is the category itself; this is what it drives
/// everywhere else — the YouTube playlist its videos go into, and the hashtags
/// its social posts carry. Keyed by the Strapi slug rather than the id: ids are
/// rows in one database, and a slug reads the same on every CMS.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Category {
    pub slug: String,
    pub name: String,
    /// The YouTube playlist id; empty for none yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub playlist_id: String,
    /// With the `#`, in the order typed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hashtags: Vec<String>,
}

/// Enough to say what a post is about; past this it reads as spam.
pub const MAX_HASHTAGS: usize = 5;

impl TeamTemplate {
    pub fn category(&self, slug: &str) -> Option<&Category> {
        self.categories
            .iter()
            .find(|category| category.slug.eq_ignore_ascii_case(slug))
    }

    /// The template with `category` added, or replacing the one with its slug.
    pub fn with_category(mut self, category: Category) -> TeamTemplate {
        match self
            .categories
            .iter_mut()
            .find(|existing| existing.slug.eq_ignore_ascii_case(&category.slug))
        {
            Some(existing) => *existing = category,
            None => self.categories.push(category),
        }
        self
    }
}

/// Hashtags as typed — spaces or commas between, `#` optional — normalised to
/// `#Word` with nothing but letters, digits and underscores, duplicates dropped.
pub fn parse_hashtags(text: &str) -> std::result::Result<Vec<String>, String> {
    let mut tags: Vec<String> = Vec::new();
    for word in text.split([',', ' ', '\n', '\t']) {
        let word = word.trim().trim_start_matches('#');
        if word.is_empty() {
            continue;
        }
        if !word.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
            return Err(format!(
                "#{word} — a hashtag is one word: letters, digits and _ only"
            ));
        }
        let tag = format!("#{word}");
        if !tags.iter().any(|seen| seen.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
    if tags.len() > MAX_HASHTAGS {
        return Err(format!(
            "{} hashtags — keep it to {MAX_HASHTAGS}",
            tags.len()
        ));
    }
    Ok(tags)
}

/// The copywriter's section for the project's category, or nothing without
/// hashtags. In the user prompt for the reason [`prompt_section`] gives.
pub fn hashtag_section(category: Option<&Category>) -> String {
    let Some(category) = category.filter(|category| !category.hashtags.is_empty()) else {
        return String::new();
    };
    format!(
        "Category: {}. End every post with these hashtags, exactly as written and in this \
         order: {}. Add no other hashtags. Where a platform's length limit is tight, shorten \
         the post, not the hashtags.\n\n",
        category.name,
        category.hashtags.join(" ")
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TeamTemplate {
    #[serde(default)]
    pub links: Vec<Link>,
    /// Added to the end of every generated YouTube description — see the
    /// module docs. Empty for none.
    #[serde(default)]
    pub youtube_footer: String,
    /// What each blog category drives beyond the blog — see [`Category`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<Category>,
    /// Who saved it last, and when, so the pane can say.
    #[serde(default)]
    pub updated_by: String,
    #[serde(default)]
    pub updated_at: String,
}

/// The team's links as last read, and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub template: TeamTemplate,
    /// The ETag they were read with; `None` when S3 had no file yet, or when
    /// they came from the local copy.
    pub etag: Option<String>,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Read from S3 just now.
    Team,
    /// S3 could not be read; this is the local copy, and why.
    Cached(String),
    /// Neither S3 nor the copy had any.
    Empty,
}

/// How a save went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    Saved(Loaded),
    /// A teammate saved since this machine read the list. Nothing was written.
    Conflict,
}

/// Parse the box: one link per line, blank lines ignored. Every bad line is
/// reported at once, by number, so a paste of ten is fixed in one pass.
pub fn parse(text: &str) -> std::result::Result<Vec<Link>, Vec<String>> {
    let mut links: Vec<Link> = Vec::new();
    let mut errors = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let n = i + 1;
        let Some(start) = line.find("https://").or_else(|| line.find("http://")) else {
            errors.push(format!("line {n} has no http(s) link"));
            continue;
        };
        let end = line[start..]
            .find(char::is_whitespace)
            .map_or(line.len(), |len| start + len);
        let url = line[start..end]
            .trim_end_matches([',', ';', ')'])
            .to_string();
        if url.len() <= "https://".len() || !url.contains('.') {
            errors.push(format!("line {n}: {url} is not a full link"));
            continue;
        }
        let separators: &[char] = &['—', '–', '-', '|', ':', ' ', '\t'];
        let label = line[..start].trim().trim_matches(separators).trim();
        let note = line[end..].trim().trim_matches(separators).trim();
        if links.iter().any(|link| link.url == url) {
            errors.push(format!("line {n}: {url} is already listed"));
            continue;
        }
        links.push(Link {
            label: if label.is_empty() {
                host(&url).to_string()
            } else {
                label.to_string()
            },
            url,
            note: note.to_string(),
        });
    }
    if links.len() > MAX_LINKS {
        errors.push(format!(
            "{} links — keep it to {MAX_LINKS}, the ones worth sending people to",
            links.len()
        ));
    }
    if errors.is_empty() {
        Ok(links)
    } else {
        Err(errors)
    }
}

/// The box's text for `links`, in the form [`parse`] reads back.
pub fn to_lines(links: &[Link]) -> String {
    links
        .iter()
        .map(|link| {
            if link.note.is_empty() {
                format!("{} — {}", link.label, link.url)
            } else {
                format!("{} — {} — {}", link.label, link.url, link.note)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

fn cache_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".stream-recorder")
        .join("team")
        .join("templates.json"))
}

fn read_cache() -> Option<TeamTemplate> {
    let text = std::fs::read_to_string(cache_path().ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(links: &TeamTemplate) {
    let Ok(path) = cache_path() else {
        return;
    };
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::write(
                &path,
                serde_json::to_string_pretty(links).unwrap_or_default() + "\n",
            )
        });
    if let Err(err) = written {
        eprintln!("stream-recorder: could not keep a copy of the team links: {err}");
    }
}

/// The team's template: from S3, falling back to the local copy with the reason.
/// Blocking — call it off the main thread.
pub fn load() -> Loaded {
    match crate::distribute::read_team_object(KEY) {
        Ok(Some(object)) => match serde_json::from_slice::<TeamTemplate>(&object.body) {
            Ok(template) => {
                write_cache(&template);
                Loaded {
                    template,
                    etag: Some(object.etag),
                    source: Source::Team,
                }
            }
            Err(err) => cached(format!("the team file does not parse: {err}")),
        },
        Ok(None) => Loaded {
            template: TeamTemplate::default(),
            etag: None,
            source: Source::Team,
        },
        Err(err) => cached(format!("{err:#}")),
    }
}

fn cached(why: String) -> Loaded {
    match read_cache() {
        Some(template) => Loaded {
            template,
            etag: None,
            source: Source::Cached(why),
        },
        None => Loaded {
            template: TeamTemplate::default(),
            etag: None,
            source: Source::Empty,
        },
    }
}

/// Save `template` for the team, if the file is still what was read with
/// `etag` (`None`: it did not exist). Blocking — call it off the main thread.
/// Who saved it and when are stamped here.
///
/// A list read from the local copy has no ETag and so is refused when a team
/// file exists: saving it would overwrite whatever the team has now with an
/// old copy.
pub fn save(template: TeamTemplate, etag: Option<&str>) -> Result<Saved> {
    if template.youtube_footer.chars().count() > MAX_FOOTER_CHARS {
        bail!("keep the YouTube footer under {MAX_FOOTER_CHARS} characters");
    }
    let team = TeamTemplate {
        updated_by: std::env::var("USER").unwrap_or_default(),
        updated_at: chrono::Local::now().to_rfc3339(),
        ..template
    };
    let body = serde_json::to_vec_pretty(&team).context("serializing the team template")?;
    match crate::distribute::write_team_object(KEY, body, etag)? {
        crate::distribute::TeamWrite::Saved { etag } => {
            write_cache(&team);
            Ok(Saved::Saved(Loaded {
                template: team,
                etag: Some(etag),
                source: Source::Team,
            }))
        }
        crate::distribute::TeamWrite::Conflict => Ok(Saved::Conflict),
    }
}

/// The copywriter's section for `links`, or nothing without any.
///
/// Carried in the user prompt rather than the system prompt, so a tuned
/// `posts.social` overlay cannot drop it.
pub fn prompt_section(links: &[Link]) -> String {
    if links.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Funnel links — where the team wants readers to go next. For each post, pick \
         the one that fits that post's audience and message, and point readers to it with \
         a short reason to click. Copy the URL exactly as written; never shorten, change \
         or invent one. Instagram and TikTok captions cannot hold a working link: name the \
         destination and say it is in the bio instead. The published video and article \
         links above still come first where they fit.\n",
    );
    for link in links {
        out.push_str(&format!("- {}: {}", link.label, link.url));
        if !link.note.is_empty() {
            out.push_str(&format!(" — {}", link.note));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Every URL in `text` that is not in `allowed` — a link the model invented
/// or mangled. Compared without a trailing slash or trailing punctuation.
pub fn unknown_urls(text: &str, allowed: &[String]) -> Vec<String> {
    let norm = |url: &str| {
        url.trim_end_matches(['.', ',', ';', ')', '!', '?', '/'])
            .to_string()
    };
    let allowed: Vec<String> = allowed.iter().map(|url| norm(url)).collect();
    text.split_whitespace()
        .filter_map(|word| {
            let start = word.find("https://").or_else(|| word.find("http://"))?;
            Some(norm(&word[start..]))
        })
        .filter(|url| !allowed.contains(url))
        .collect()
}

/// Checked before a save goes out: nothing to send is refused here rather
/// than written as an empty team list by a mistaken select-all and delete.
pub fn ensure_not_emptied(links: &[Link], had: usize) -> Result<()> {
    if links.is_empty() && had > 0 {
        bail!("the box is empty — that would remove all {had} team links; add one back, or reload");
    }
    Ok(())
}

/// The footer with `{links}` filled in, trimmed; empty when there is none.
pub fn render_footer(template: &TeamTemplate) -> String {
    let lines = template
        .links
        .iter()
        .map(|link| format!("{}: {}", link.label, link.url))
        .collect::<Vec<_>>()
        .join("\n");
    template
        .youtube_footer
        .replace(LINKS_PLACEHOLDER, &lines)
        .trim()
        .to_string()
}

/// `description` ending with the team footer, once. A description that
/// already carries it — regenerated copy, or copy edited around it — is left
/// alone, and the description is what gives way when the two would pass
/// YouTube's limit: the footer is the funnel.
pub fn with_footer(description: &str, template: &TeamTemplate) -> String {
    let footer = render_footer(template);
    let description = description.trim_end();
    if footer.is_empty() || description.contains(&footer) {
        return description.to_string();
    }
    let room = MAX_DESCRIPTION_CHARS.saturating_sub(footer.chars().count() + 2);
    let body: String = description.chars().take(room).collect();
    if body.trim().is_empty() {
        footer
    } else {
        format!("{}\n\n{footer}", body.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_label_url_and_note_whatever_separates_them() {
        let links = parse(
            "Newsletter — https://saaga.dev/news — end of a tutorial\n\
             \n\
             CI template | https://github.com/saaga/ci, founders trying it\n\
             https://saaga.dev/book\n\
             Site - https://saaga.dev",
        )
        .unwrap();
        assert_eq!(links.len(), 4);
        assert_eq!(links[0].label, "Newsletter");
        assert_eq!(links[0].url, "https://saaga.dev/news");
        assert_eq!(links[0].note, "end of a tutorial");
        assert_eq!(links[1].url, "https://github.com/saaga/ci");
        assert_eq!(links[1].note, "founders trying it");
        assert_eq!(links[2].label, "saaga.dev", "no label is the host");
        assert_eq!(links[3].label, "Site");
        assert_eq!(links[3].note, "");
    }

    #[test]
    fn every_bad_line_is_reported_by_number() {
        let errors = parse(
            "Newsletter — saaga.dev/news\n\
             Site — https://saaga.dev\n\
             Again — https://saaga.dev\n\
             Broken — https://",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].starts_with("line 1 has no http(s) link"));
        assert!(errors[1].contains("line 3") && errors[1].contains("already listed"));
        assert!(errors[2].contains("line 4"));
    }

    #[test]
    fn too_many_links_is_refused() {
        let text: String = (0..=MAX_LINKS)
            .map(|i| format!("L{i} — https://saaga.dev/{i}\n"))
            .collect();
        let errors = parse(&text).unwrap_err();
        assert!(errors[0].contains(&format!("keep it to {MAX_LINKS}")));
    }

    #[test]
    fn the_box_round_trips() {
        let links =
            parse("Newsletter — https://saaga.dev/news — tutorials\nSite — https://saaga.dev")
                .unwrap();
        assert_eq!(parse(&to_lines(&links)).unwrap(), links);
    }

    #[test]
    fn the_prompt_names_each_link_and_says_how_to_use_them() {
        assert_eq!(prompt_section(&[]), "");
        let section =
            prompt_section(&parse("Newsletter — https://saaga.dev/news — tutorials").unwrap());
        assert!(section.contains("- Newsletter: https://saaga.dev/news — tutorials"));
        assert!(section.contains("Copy the URL exactly"));
        assert!(section.contains("in the bio"));
    }

    #[test]
    fn an_invented_or_mangled_url_is_caught() {
        let allowed = vec![
            "https://saaga.dev/news".to_string(),
            "https://youtu.be/abc".to_string(),
        ];
        let text = "Read more: https://saaga.dev/news/. Watch https://youtu.be/abc! \
                    Or https://saaga.dev/newsletter";
        assert_eq!(
            unknown_urls(text, &allowed),
            ["https://saaga.dev/newsletter"]
        );
    }

    #[test]
    fn emptying_a_list_that_had_links_is_refused() {
        assert!(ensure_not_emptied(&[], 3).is_err());
        assert!(ensure_not_emptied(&[], 0).is_ok());
    }

    fn template(footer: &str) -> TeamTemplate {
        TeamTemplate {
            links: parse(
                "Newsletter — https://saaga.dev/news\nCI template — https://github.com/saaga/ci",
            )
            .unwrap(),
            youtube_footer: footer.into(),
            ..TeamTemplate::default()
        }
    }

    #[test]
    fn the_footer_fills_in_the_links() {
        let footer = render_footer(&template("Start here:\n{links}\n\n#devtools"));
        assert_eq!(
            footer,
            "Start here:\nNewsletter: https://saaga.dev/news\nCI template: https://github.com/saaga/ci\n\n#devtools"
        );
        assert_eq!(render_footer(&template("  ")), "");
    }

    #[test]
    fn a_description_gets_the_footer_once() {
        let team = template("More: {links}");
        let once = with_footer("What the video shows.\n", &team);
        assert!(once.starts_with("What the video shows.\n\nMore: Newsletter"));
        assert_eq!(with_footer(&once, &team), once, "not added twice");
        assert_eq!(with_footer("Plain.", &template("")), "Plain.");
    }

    #[test]
    fn hashtags_are_normalised_and_policed() {
        assert_eq!(
            parse_hashtags("SEO, #AIAgents  seo\n#GEO").unwrap(),
            ["#SEO", "#AIAgents", "#GEO"]
        );
        assert_eq!(parse_hashtags("  ").unwrap(), Vec::<String>::new());
        assert!(parse_hashtags("#go-to-market")
            .unwrap_err()
            .contains("one word"));
        assert!(parse_hashtags("a b c d e f")
            .unwrap_err()
            .contains("keep it to 5"));
    }

    #[test]
    fn a_category_is_found_and_replaced_by_slug() {
        let seo = Category {
            slug: "seo-agents".into(),
            name: "SEO Agents".into(),
            playlist_id: "PL1".into(),
            hashtags: vec!["#SEO".into()],
        };
        let team = TeamTemplate::default().with_category(seo.clone());
        assert_eq!(team.category("SEO-Agents"), Some(&seo));
        let team = team.with_category(Category {
            playlist_id: "PL2".into(),
            ..seo
        });
        assert_eq!(team.categories.len(), 1);
        assert_eq!(team.category("seo-agents").unwrap().playlist_id, "PL2");
    }

    #[test]
    fn the_prompt_carries_the_category_hashtags() {
        assert_eq!(hashtag_section(None), "");
        let gtm = Category {
            slug: "gtm".into(),
            name: "GTM".into(),
            ..Category::default()
        };
        assert_eq!(hashtag_section(Some(&gtm)), "", "no hashtags, no section");
        let gtm = Category {
            hashtags: vec!["#GTM".into(), "#B2B".into()],
            ..gtm
        };
        let section = hashtag_section(Some(&gtm));
        assert!(section.contains("Category: GTM."), "{section}");
        assert!(section.contains("#GTM #B2B"), "{section}");
    }

    #[test]
    fn the_description_gives_way_to_the_footer_at_the_limit() {
        let team = template("{links}");
        let long = "x".repeat(MAX_DESCRIPTION_CHARS);
        let joined = with_footer(&long, &team);
        assert!(joined.chars().count() <= MAX_DESCRIPTION_CHARS);
        assert!(joined.ends_with("CI template: https://github.com/saaga/ci"));
    }
}
