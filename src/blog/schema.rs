//! The article draft, on disk and on the wire.
//!
//! Two shapes, deliberately: [`Article`] is what this stage writes and a human
//! can read, while the Strapi dynamic zone it becomes is assembled in
//! [`super::strapi`]. Keeping them apart means the CMS's field names —
//! `textBodyHtml`, `quoteTextHighlighted`, `__component` — never leak into the
//! generator or into the pane.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const ARTICLE_JSON: &str = "article.json";

/// One entry in the article body.
///
/// A flat enum rather than one struct per kind because the CMS zone is ordered
/// and mixed: the sequence text, text, quote, text is meaningful and a set of
/// parallel vectors would lose it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Block {
    /// CKEditor HTML. `<h2>` headings inside this become the page's table of
    /// contents, which is why the prompt cares what they read like.
    Text { html: String },
    Quote {
        text: String,
        /// Must be a substring of `text`; the reader renders it in brand orange
        /// and silently highlights nothing when it is not found.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        highlight: String,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// A built component, by the step id it was requested under.
    ///
    /// A **reference**, for the same reason [`Block::Figure`] is one: the block
    /// itself — its `componentKey`, its version, its whole `config` — is built
    /// and validated against the landing repo's catalog by the component stage,
    /// and lands in `component-job/components.json`. An article holding a copy
    /// would keep publishing whatever that config said the day it was written,
    /// and would let a `componentKey` reach the CMS without ever meeting the
    /// validator that knows which four keys exist.
    Embed { id: String },
    /// A captured figure, by its number in `figures.jsonl`.
    ///
    /// A **reference**, deliberately — not a copy of the caption. Rewriting a
    /// blurb is an append to the figure ledger, so an article holding its own
    /// copy would keep publishing the caption that was current when it was
    /// written. It also means the article carries no URL: the figure's `src`
    /// exists only after the upload, which is why [`super::payload::zone`]
    /// takes the resolved media separately.
    Figure { n: u32 },
}

/// One entry in the on-page FAQ.
///
/// Named for what it is, and renamed at the CMS boundary. The Strapi component
/// calls these fields `title` and `content`, not `question` and `answer` — a
/// trap the landing repo's own schema doc calls out, because the wrong pair is
/// what anyone writing this by hand reaches for first. See [`super::payload`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Faq {
    pub question: String,
    pub answer: String,
}

/// One term the page is written to rank for, with why.
///
/// The structured successor to a flat `keywords` list, which the site still
/// reads as a fallback. The difference that earns the extra shape is `priority`
/// and `intent`: a page has exactly one term it is *for*, and knowing whether
/// someone searching it wants to learn, compare or buy is what decides whether
/// the page answers them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeywordTarget {
    pub term: String,
    /// `primary` or `secondary`. Exactly one target is primary — see
    /// [`super::generate`], which enforces it rather than trusting it.
    pub priority: String,
    /// `informational`, `commercial`, `transactional` or `navigational`, or
    /// empty when it is not one of those. The CMS enum is closed, so a value
    /// invented here would be refused for the whole entry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Article {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<u32>,
    #[serde(default)]
    pub prompt_hash: String,
    pub title: String,
    /// The heading printed on the page, when it should read differently from
    /// `title`.
    ///
    /// Empty is the normal case and not a gap: the site falls back to `title`,
    /// and a heading that merely repeats it is one more string to keep in sync
    /// for no reader's benefit. It earns its place when `title` has been
    /// shortened for a search result and reads clipped on the page itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub h1: String,
    pub slug: String,
    /// The listing teaser, and the `<meta name="description">` when nothing
    /// longer is set. 140–160 characters, which is what a search snippet shows.
    pub description: String,
    /// The long description, for the pages that have room for one.
    ///
    /// Separate from `description` because the two do different jobs at
    /// different lengths: that one has to survive being cut to a snippet, this
    /// one is read whole. Empty is the normal case and the site falls back —
    /// which is why nothing here pads it out to fill the field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub long_description: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// The editorial brief the keywords are the flat version of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyword_targets: Vec<KeywordTarget>,
    /// Shown under the embed.
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// Questions the video answers, for the on-page accordion and the
    /// `FAQPage` structured data the search result is built from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faq: Vec<Faq>,
    /// Publish, but keep it out of the index: off `/blog`, out of the sitemap,
    /// and tagged `noindex`. For internal, duplicate or test content.
    ///
    /// On the article rather than decided at publish time, so it is a field you
    /// can see in the draft and in the preview before the page exists — the
    /// same reason every other decision here is on disk.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_index: bool,
    /// Credit another URL as the canonical version of this content.
    ///
    /// Blank is correct for everything original, which is almost everything.
    /// Set it only when this exact content is already published elsewhere and
    /// search should rank *that* page instead of this one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub canonical_url: String,
}

impl Article {
    /// Whether there is enough here to create an entry at all. `title`, `slug`
    /// and `description` are all required by the collection type, so a draft
    /// missing any of them would be rejected by Strapi rather than by us.
    pub fn is_publishable(&self) -> bool {
        !self.title.trim().is_empty()
            && !self.slug.trim().is_empty()
            && !self.description.trim().is_empty()
            && !self.blocks.is_empty()
    }

    /// The components this article places, in order, deduplicated.
    pub fn embeds(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for block in &self.blocks {
            if let Block::Embed { id } = block {
                if !seen.contains(id) {
                    seen.push(id.clone());
                }
            }
        }
        seen
    }

    /// The figures this article places, in order, deduplicated.
    ///
    /// What the publish has to upload and the preview has to resolve. Order is
    /// the article's, not the ledger's: it is the order they appear on the page.
    pub fn figures(&self) -> Vec<u32> {
        let mut seen = Vec::new();
        for block in &self.blocks {
            if let Block::Figure { n } = block {
                if !seen.contains(n) {
                    seen.push(*n);
                }
            }
        }
        seen
    }

    pub fn section_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| matches!(block, Block::Text { .. }))
            .count()
    }
}

pub fn save(dir: &Path, article: &Article) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(ARTICLE_JSON);
    let json = serde_json::to_string_pretty(article).context("serializing the article")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load(dir: &Path) -> Result<Article> {
    let path = dir.join(ARTICLE_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// A URL-safe slug: lowercase, words joined by single hyphens, nothing else.
///
/// `slug` is a Strapi `uid` field with a 300-character ceiling, and the live
/// library uses short bare slugs (`/education/ai-memory-tool`), so this trims to
/// something of that shape rather than transliterating a whole headline.
pub fn slugify(value: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in value.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    // Cut on a word boundary so the slug does not end mid-word — but only trim
    // back when the cut actually landed mid-word. A cut whose next character is
    // the separator already ends on a whole word, and trimming there threw a
    // perfectly good word away.
    let cut: String = trimmed.chars().take(max).collect();
    if trimmed.chars().nth(max) == Some('-') {
        return cut.trim_matches('-').to_string();
    }
    match cut.rsplit_once('-') {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In article order, not ledger order, and each one once — this is what the
    /// publish uploads and what the preview resolves.
    #[test]
    fn the_figures_are_listed_in_the_order_they_appear() {
        let mut draft = article();
        draft.blocks = vec![
            Block::Figure { n: 3 },
            Block::Text { html: "<p>Body.</p>".into() },
            Block::Figure { n: 1 },
            Block::Figure { n: 3 },
        ];
        assert_eq!(draft.figures(), vec![3, 1]);
        assert_eq!(article().figures(), Vec::<u32>::new(), "most articles have none");
    }

    fn article() -> Article {
        Article {
            title: "Why watermarking fails".into(),
            slug: "why-watermarking-fails".into(),
            description: "A short, complete promise of what the piece argues.".into(),
            keywords: vec!["ai".into()],
            caption: "Four minutes on enforcement economics".into(),
            blocks: vec![
                Block::Text { html: "<h2>Where it broke</h2><p>Body.</p>".into() },
                Block::Quote { text: "It could not.".into(), highlight: "could not".into() },
            ],
            ..Article::default()
        }
    }

    #[test]
    fn an_article_round_trips() {
        let dir = std::env::temp_dir().join(format!("blog-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save(&dir, &article()).unwrap();
        assert_eq!(load(&dir).unwrap(), article());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The block order carries meaning, so it has to survive the round trip —
    /// a quote belongs after the section it came out of.
    #[test]
    fn the_block_order_survives_the_round_trip() {
        let dir = std::env::temp_dir().join(format!("blog-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save(&dir, &article()).unwrap();
        let back = load(&dir).unwrap();
        assert!(matches!(back.blocks[0], Block::Text { .. }));
        assert!(matches!(back.blocks[1], Block::Quote { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Strapi requires title, slug and description, so a draft missing one is
    /// caught here rather than by a 400 after the thumbnail has been uploaded.
    #[test]
    fn a_draft_missing_a_required_field_is_not_publishable() {
        assert!(article().is_publishable());
        for spoil in [
            |a: &mut Article| a.title = "  ".into(),
            |a: &mut Article| a.slug = String::new(),
            |a: &mut Article| a.description = " ".into(),
            |a: &mut Article| a.blocks.clear(),
        ] {
            let mut broken = article();
            spoil(&mut broken);
            assert!(!broken.is_publishable());
        }
    }

    #[test]
    fn slugs_are_lowercase_hyphenated_and_bare() {
        assert_eq!(slugify("Why Watermarking Fails!", 80), "why-watermarking-fails");
        assert_eq!(slugify("  AI & the arms race  ", 80), "ai-the-arms-race");
        assert_eq!(slugify("Hello -- world", 80), "hello-world");
    }

    /// A slug is a permanent URL, so an overlong title is cut at a word rather
    /// than mid-word.
    #[test]
    fn an_overlong_slug_is_cut_on_a_word_boundary() {
        let got = slugify("alpha beta gamma delta epsilon", 16);
        assert!(got.chars().count() <= 16, "{got}");
        assert!(!got.ends_with('-'));
        assert_eq!(got, "alpha-beta-gamma");
    }

    /// The off-by-one this pins: a cut landing exactly on the separator already
    /// ends on a whole word, and trimming back there silently dropped one.
    #[test]
    fn a_cut_that_lands_on_a_separator_keeps_the_whole_last_word() {
        assert_eq!(slugify("alpha beta gamma delta", 16), "alpha-beta-gamma");
        // Landing mid-word still trims back to the previous boundary.
        assert_eq!(slugify("alpha beta gammatron", 18), "alpha-beta");
    }

    #[test]
    fn a_title_of_punctuation_slugs_to_nothing_rather_than_hyphens() {
        assert_eq!(slugify("!!! ???", 80), "");
        assert!(!slugify("!!! ???", 80).contains('-'));
    }

    #[test]
    fn sections_are_the_text_blocks_only() {
        assert_eq!(article().section_count(), 1);
    }
}
