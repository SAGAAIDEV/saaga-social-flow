//! What the Blog tab shows, decided in Rust.
//!
//! The published row outranks the draft: once a post is live, "where it is" is
//! the useful thing on the page, and the article that produced it is history.

use std::path::Path;

use serde::Serialize;

use super::schema::{self, Article, Block};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pane {
    /// Why nothing can be pressed, when nothing can.
    pub blocked: Option<String>,
    pub can_publish: bool,
    pub prompt: PromptView,
    /// The byline and taxonomy this would post under, so a wrong one is visible
    /// before the post goes live rather than after.
    pub author: String,
    pub category: String,
    /// The CMS's own lists, to pick from. Empty until the cache has been filled
    /// once, which is what `library_hint` explains.
    pub authors: Vec<Choice>,
    pub categories: Vec<Choice>,
    /// When the lists were last read, or why they are empty.
    pub library_hint: String,
    /// The live post, once there is one.
    pub posted: Option<PostedView>,
    pub article: Option<ArticleView>,
    pub article_path: Option<String>,
    /// The figures captured for this video, and whether any still need a blurb.
    ///
    /// On this tab rather than one of its own because a figure exists to
    /// illustrate the article below it — see [`crate::figure::pane`]. Built by
    /// the caller for the same reason the library and the byline are: it needs
    /// the display's backing scale to report sizes in pixels, and a pane that
    /// read the display itself could not be tested.
    pub figures: crate::figure::pane::FiguresView,
}

/// One option in the author or category dropdown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choice {
    /// The relation id as a string, because that is what the dropdown sends
    /// back. Empty for the "none" option.
    pub id: String,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PromptView {
    pub label: String,
    pub path: String,
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PostedView {
    pub url: String,
    pub admin_url: String,
    pub slug: String,
    pub published: bool,
    pub created_at: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArticleView {
    pub title: String,
    pub slug: String,
    pub description: String,
    /// Character count, because the field has a 140–160 target and the whole
    /// point of showing it is checking that.
    pub description_length: usize,
    pub caption: String,
    pub keywords: Vec<String>,
    pub blocks: Vec<BlockView>,
    /// "4 section(s) · 1 quote(s) · 0 table(s)".
    pub summary: String,
    /// The on-page heading, when it differs from the title. Empty is the normal
    /// case — the site falls back to the title.
    pub h1: String,
    /// The FAQ, as `question — answer` lines. Shown in full rather than counted:
    /// these publish as structured data, and a question the video does not
    /// answer is the one kind of damage a count would hide.
    pub faq: Vec<String>,
    /// The long description, when one was written. Empty falls back to the
    /// teaser above it.
    pub long_description: String,
    /// The keyword brief, as `primary · term — intent` lines. Shown rather than
    /// counted: which term is primary is the one editorial decision in it.
    pub keyword_targets: Vec<String>,
    /// A warning line when the draft carries a search control, empty when it
    /// does not. Both are invisible on the live page and both are the kind of
    /// mistake nobody notices.
    pub control_warning: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockView {
    pub index: usize,
    pub kind: &'static str,
    /// Headings pulled out of a text block's HTML — these become the page's
    /// table of contents, so they are worth reading before publishing.
    pub headings: Vec<String>,
    pub body: String,
}

/// `root` is the project (the prompt and the ledger), `dir` the version's blog
/// folder (the draft).
///
/// `cfg` and `library` are passed rather than loaded here. Both live in `$HOME`
/// rather than under a path this function is given, so loading them internally
/// would make the pane — and every test of it — depend on whatever the machine
/// happens to have picked.
#[allow(clippy::too_many_arguments)]
pub fn build(
    root: &Path,
    dir: &Path,
    can_publish: bool,
    blocked: Option<String>,
    posted: Option<super::Post>,
    cfg: &crate::config::Config,
    library: &super::library::Library,
    figures: crate::figure::pane::FiguresView,
) -> Pane {
    let article = schema::load(dir).ok();
    let author = super::chosen_author(cfg, library);
    let category = super::chosen_category(cfg, library);
    Pane {
        blocked: blocked.or_else(|| {
            article
                .is_none()
                .then(|| "No article yet — press Create Draft to write one.".to_string())
        }),
        can_publish,
        prompt: prompt_view(root, crate::agent::prompt::library_dir().as_deref()),
        authors: choices(&library.authors, author.id),
        categories: choices(&library.categories, category.id),
        library_hint: library_hint(library),
        author: author.label,
        category: category.label,
        posted: posted.map(|row| PostedView {
            url: row.url,
            admin_url: row.admin_url,
            slug: row.slug,
            published: row.published,
            created_at: row.created_at,
            warning: row.warning,
        }),
        article_path: article
            .as_ref()
            .map(|_| dir.join(schema::ARTICLE_JSON).display().to_string()),
        article: article.map(article_view),
        figures,
    }
}

/// The cached list as dropdown options, with an explicit "none" at the top.
///
/// "None" is offered rather than implied by an empty list: posting with no
/// byline is a real, if poor, choice, and a dropdown you cannot clear is one you
/// cannot correct after a mis-click.
fn choices(entries: &[super::library::Entry], selected: Option<i64>) -> Vec<Choice> {
    std::iter::once(Choice {
        id: String::new(),
        label: "— none —".to_string(),
        selected: selected.is_none(),
    })
    .chain(entries.iter().map(|entry| Choice {
        id: entry.id.to_string(),
        label: entry.label(),
        selected: selected == Some(entry.id),
    }))
    .collect()
}

/// Says how old the lists are, or why there are none.
///
/// A cold cache is the common first-run state and it is not an error, so it
/// reads as an instruction rather than a failure.
fn library_hint(library: &super::library::Library) -> String {
    match &library.fetched_at {
        Some(when) if !library.authors.is_empty() => format!(
            "{} author(s), {} category(ies), read {when}",
            library.authors.len(),
            library.categories.len()
        ),
        Some(when) => format!("Strapi returned no authors when read {when}"),
        None => "Press Refresh to read the authors and categories from Strapi.".to_string(),
    }
}

fn article_view(article: Article) -> ArticleView {
    let (mut quotes, mut tables, mut figures) = (0, 0, 0);
    let blocks = article
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let (kind, headings, body) = match block {
                Block::Text { html } => ("text", headings_in(html), html.clone()),
                Block::Quote { text, highlight } => {
                    quotes += 1;
                    let body = if highlight.is_empty() {
                        text.clone()
                    } else {
                        format!("{text}  (highlight: {highlight})")
                    };
                    ("quote", Vec::new(), body)
                }
                Block::Table { headers, rows } => {
                    tables += 1;
                    (
                        "table",
                        Vec::new(),
                        format!("{} column(s) × {} row(s)", headers.len(), rows.len()),
                    )
                }
                // Named by number rather than by caption. The caption lives in
                // the figure ledger and is shown once, in the strip above —
                // repeating it here would be two copies of the same words that
                // can disagree.
                Block::Figure { n } => {
                    figures += 1;
                    ("figure", Vec::new(), format!("figure {n:02}"))
                }
                // Named by step id, like a figure is named by number: the block
                // itself lives in `components.json` and is shown there.
                Block::Embed { id } => ("embed", Vec::new(), id.clone()),
            };
            BlockView {
                index: index + 1,
                kind,
                headings,
                body,
            }
        })
        .collect();

    ArticleView {
        summary: format!(
            "{} section(s) · {quotes} quote(s) · {tables} table(s) · {figures} figure(s)",
            article.section_count()
        ),
        description_length: article.description.chars().count(),
        faq: article
            .faq
            .iter()
            .map(|entry| format!("{} — {}", entry.question, entry.answer))
            .collect(),
        control_warning: match (article.no_index, article.canonical_url.is_empty()) {
            (true, true) => "noindex — this post will not appear in /blog or the sitemap".into(),
            (true, false) => format!(
                "noindex, and canonical to {} — hidden from search and crediting another page",
                article.canonical_url
            ),
            (false, false) => {
                format!(
                    "canonical to {} — search will credit that page",
                    article.canonical_url
                )
            }
            (false, true) => String::new(),
        },
        keyword_targets: article
            .keyword_targets
            .iter()
            .map(|target| {
                let intent = match target.intent.is_empty() {
                    true => String::new(),
                    false => format!(" — {}", target.intent),
                };
                format!("{} · {}{intent}", target.priority, target.term)
            })
            .collect(),
        long_description: article.long_description,
        h1: article.h1,
        title: article.title,
        slug: article.slug,
        description: article.description,
        caption: article.caption,
        keywords: article.keywords,
        blocks,
    }
}

/// The `<h2>`/`<h3>` text in a block, read the same way the landing reads it: a
/// regex over the HTML. Done here so the pane shows the table of contents the
/// page will actually build, rather than a guess at it.
fn headings_in(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<h") {
        rest = &rest[start + 2..];
        let Some(level) = rest.chars().next().filter(|c| ('1'..='3').contains(c)) else {
            continue;
        };
        let Some(open_end) = rest.find('>') else {
            break;
        };
        let close = format!("</h{level}>");
        let after = &rest[open_end + 1..];
        let Some(close_at) = after.find(&close) else {
            rest = after;
            continue;
        };
        let text = strip_tags(&after[..close_at]);
        if !text.is_empty() {
            out.push(text);
        }
        rest = &after[close_at + close.len()..];
    }
    out
}

fn strip_tags(value: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in value.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// `library` is where a standing house prompt would live, passed in rather than
/// read from `$HOME` so this is testable on a machine that has one.
fn prompt_view(root: &Path, library: Option<&Path>) -> PromptView {
    let prompt_id = crate::agent::prompt::BLOG;
    let live = crate::agent::prompt::live_in(prompt_id, Some(root), library);
    let builtin = live.as_ref().is_none_or(|resolved| resolved.is_builtin());
    PromptView {
        label: match live {
            Some(resolved) if resolved.is_builtin() => "v0 (builtin)".to_string(),
            Some(resolved) => resolved.label(),
            None => "v0 (builtin)".to_string(),
        },
        path: library
            .map(|dir| dir.join(format!("{prompt_id}.txt")).display().to_string())
            .unwrap_or_default(),
        builtin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-blog-pane-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn written(dir: &Path) {
        schema::save(
            dir,
            &Article {
                title: "Why watermarking fails".into(),
                slug: "why-watermarking-fails".into(),
                description: "A complete promise of the argument, ending in a stop.".into(),
                caption: "Four minutes on enforcement economics".into(),
                keywords: vec!["ai".into()],
                blocks: vec![
                    Block::Text {
                        html: "<h2>Where it broke</h2><p>Body.</p><h3>Detail</h3>".into(),
                    },
                    Block::Quote {
                        text: "It could not.".into(),
                        highlight: "could not".into(),
                    },
                ],
                ..Article::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn a_project_with_no_draft_invites_the_first_one() {
        let dir = temp("empty");
        let pane = build(
            &dir,
            &dir,
            true,
            None,
            None,
            &unset(),
            &no_library(),
            figures(&dir),
        );
        assert!(pane.blocked.unwrap().contains("press Create Draft"));
        assert!(pane.article.is_none());
        assert!(pane.posted.is_none());
    }

    /// A real blocking reason outranks the invitation.
    #[test]
    fn a_gate_reason_wins_over_the_invitation() {
        let dir = temp("gated");
        let pane = build(
            &dir,
            &dir,
            false,
            Some("Not on YouTube yet".into()),
            None,
            &unset(),
            &no_library(),
            figures(&dir),
        );
        assert_eq!(pane.blocked.as_deref(), Some("Not on YouTube yet"));
        assert!(!pane.can_publish);
    }

    #[test]
    fn a_written_draft_shows_its_blocks_and_counts() {
        let dir = temp("written");
        written(&dir);
        let pane = build(
            &dir,
            &dir,
            true,
            None,
            None,
            &unset(),
            &no_library(),
            figures(&dir),
        );
        let article = pane.article.expect("a draft");
        assert_eq!(
            article.summary,
            "1 section(s) · 1 quote(s) · 0 table(s) · 0 figure(s)"
        );
        assert_eq!(article.blocks[0].kind, "text");
        assert_eq!(article.blocks[0].index, 1, "numbered for the reader");
        assert_eq!(article.blocks[1].kind, "quote");
        assert!(article.blocks[1].body.contains("highlight: could not"));
        assert!(pane.article_path.unwrap().ends_with("article.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The description has a 140–160 target and is the one field a human should
    /// check, so its length is shown rather than left to be counted by eye.
    #[test]
    fn the_description_reports_its_own_length() {
        let dir = temp("length");
        written(&dir);
        let article = build(
            &dir,
            &dir,
            true,
            None,
            None,
            &unset(),
            &no_library(),
            figures(&dir),
        )
        .article
        .unwrap();
        assert_eq!(
            article.description_length,
            article.description.chars().count()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pane shows the table of contents the page will build, read the same
    /// way the page reads it — so a heading that only makes sense in context is
    /// visible as a problem before publishing.
    #[test]
    fn the_headings_shown_are_the_ones_the_page_will_list() {
        let dir = temp("toc");
        written(&dir);
        let article = build(
            &dir,
            &dir,
            true,
            None,
            None,
            &unset(),
            &no_library(),
            figures(&dir),
        )
        .article
        .unwrap();
        assert_eq!(article.blocks[0].headings, vec!["Where it broke", "Detail"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn headings_survive_attributes_and_nested_tags() {
        let got =
            headings_in("<h2 id=\"a\">Where <strong>it</strong> broke</h2><p>x</p><h2>Second</h2>");
        assert_eq!(got, vec!["Where it broke", "Second"]);
    }

    #[test]
    fn a_block_with_no_headings_lists_none() {
        assert!(headings_in("<p>Just prose.</p>").is_empty());
        assert!(headings_in("").is_empty());
        // An unterminated tag must not loop or panic.
        assert!(headings_in("<h2>unclosed").is_empty());
    }

    /// Once it is live, where it is beats what produced it.
    #[test]
    fn a_published_post_reports_both_of_its_urls() {
        let dir = temp("posted");
        written(&dir);
        let pane = build(
            &dir,
            &dir,
            false,
            None,
            Some(super::super::Post {
                document_id: "doc".into(),
                slug: "why-watermarking-fails".into(),
                url: "https://saagasolve.com/education/why-watermarking-fails".into(),
                admin_url: "https://cms.saagasolve.com/admin/x".into(),
                video_id: "vid-1".into(),
                published: true,
                created_at: "2026-08-18T12:00:00Z".into(),
                warning: Some("author could not be set".into()),
            }),
            &unset(),
            &no_library(),
            figures(&dir),
        );
        let posted = pane.posted.expect("the live row");
        assert!(posted.url.ends_with("/education/why-watermarking-fails"));
        assert!(posted.admin_url.contains("/admin/"));
        assert!(posted.published);
        assert_eq!(posted.warning.as_deref(), Some("author could not be set"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing is chosen until it is chosen. This is the change: the byline used
    /// to default to a name that is not in the CMS, so the pane read as settled
    /// while the post it would make had no author at all.
    #[test]
    fn an_unconfigured_pane_says_nothing_is_chosen() {
        let dir = temp("byline-none");
        let pane = build(
            &dir,
            &dir,
            true,
            None,
            None,
            &unset(),
            &no_library(),
            figures(&dir),
        );
        assert_eq!(pane.author, "none chosen");
        assert_eq!(pane.category, "none chosen");
        assert!(pane.library_hint.contains("Press Refresh"));
        // Only "none" to pick from, and it is what is selected.
        assert_eq!(pane.authors.len(), 1);
        assert!(pane.authors[0].selected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The byline and taxonomy are shown, because a wrong one is much cheaper to
    /// catch before the post is live than after — and the dropdown highlights
    /// the row it would post under.
    #[test]
    fn the_pane_names_the_byline_it_would_post_under() {
        let dir = temp("byline");
        let pane = build(
            &dir,
            &dir,
            true,
            None,
            None,
            &picked(12, 2),
            &library(),
            figures(&dir),
        );
        assert_eq!(pane.author, "Ahmed Raza");
        assert_eq!(pane.category, "AI Powered Marketing");
        assert!(pane.library_hint.contains("read 2026-08-29T00:00:00Z"));

        let selected: Vec<_> = pane.authors.iter().filter(|c| c.selected).collect();
        assert_eq!(selected.len(), 1, "exactly one row is highlighted");
        assert_eq!(selected[0].id, "12");
        // Two authors really do share a name in the live CMS, so the job title
        // is what tells them apart in the list.
        assert!(
            pane.authors
                .iter()
                .any(|c| c.label.contains("Senior SEO Content Strategist")),
            "{:?}",
            pane.authors.iter().map(|c| &c.label).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No figures, and no display to scale them against — every test here is
    /// about the article, not the pictures beside it.
    fn figures(dir: &Path) -> crate::figure::pane::FiguresView {
        crate::figure::pane::build(dir, None)
    }

    fn unset() -> crate::config::Config {
        crate::config::Config::default()
    }

    fn picked(author: i64, category: i64) -> crate::config::Config {
        crate::config::Config {
            blog_author_id: Some(author),
            blog_author_name: Some("Ahmed Raza".into()),
            blog_category_id: Some(category),
            blog_category_name: Some("AI Powered Marketing".into()),
            ..Default::default()
        }
    }

    fn no_library() -> super::super::library::Library {
        super::super::library::Library::default()
    }

    /// Shaped like the live CMS, duplicate name included.
    fn library() -> super::super::library::Library {
        let entry = |id, name: &str, detail: Option<&str>| super::super::library::Entry {
            id,
            name: name.into(),
            slug: name.to_lowercase().replace(' ', "-"),
            detail: detail.map(str::to_string),
        };
        super::super::library::Library {
            fetched_at: Some("2026-08-29T00:00:00Z".into()),
            authors: vec![
                entry(12, "Ahmed Raza", Some("Senior SEO Content Strategist")),
                entry(11, "Danish Rafique", Some("Senior SEO Content Strategist")),
                entry(25, "Danish Rafique", Some("SEO Content Strategist")),
            ],
            categories: vec![entry(
                2,
                "AI Powered Marketing",
                Some("Marketing, automated"),
            )],
        }
    }

    #[test]
    fn the_prompt_view_names_the_library_file_while_it_is_still_the_builtin() {
        let dir = temp("prompt");
        std::fs::create_dir_all(&dir).unwrap();
        let view = prompt_view(&dir, Some(&dir.join("library")));
        assert!(view.builtin);
        assert_eq!(view.label, "v0 (builtin)");
        assert!(view.path.ends_with("blog.article.txt"), "{}", view.path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
