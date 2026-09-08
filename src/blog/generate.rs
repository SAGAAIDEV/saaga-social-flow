//! Turning the longform's transcript into the article that sits beside it.
//!
//! A different job from [`crate::substack::generate`], which is why it is a
//! different prompt. Substack notes are beats someone types an essay from;
//! this is the finished prose, published by a machine, read by search engines,
//! and never retyped. The two cannot share a preamble — there is one overlay
//! file per prompt id, so tuning the article voice would retune the typing
//! notes.
//!
//! As everywhere else, the output *shape* is a `JsonSchema` on
//! [`ArticleExtraction`] rather than something the prompt describes, so an
//! overlay can rewrite every rule without being able to break parsing.
//!
//! Length rules stay prose in the prompt and are enforced in Rust afterwards.
//! The legacy Python learned this: models treat schema length bounds as
//! advisory, and a too-long string becomes a hard validation bounce instead of a
//! slightly long title.

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::schema::{slugify, Article, Block};
use crate::longform::Longform;

/// Strapi's own ceilings. `title` is capped at 200 and `slug` at 300 by the
/// collection type; the slug is kept far shorter than it allows because the live
/// library uses short bare slugs and a URL is permanent.
const TITLE_MAX: usize = 200;
const SLUG_MAX: usize = 80;
const DESCRIPTION_MAX: usize = 160;
/// Strapi's ceiling on the long description. Generous, because the field's
/// whole point is having room the snippet does not.
const LONG_DESCRIPTION_MAX: usize = 1000;
const CAPTION_MAX: usize = 120;
/// Not a CMS limit — an editorial one. Past five the accordion is longer than
/// the article and the questions stop being ones anyone asked.
const FAQ_MAX: usize = 5;

pub const SYSTEM_PROMPT: &str = r#"You write the article that accompanies a recorded technical video on saagasolve.com.

The video is embedded at the top of the page. Never open with "in this video" and
never address the reader as a viewer — the article has to stand on its own if the
embed fails, and it is what search engines read.

Work only from the transcript. Do not invent facts, numbers, product names or
outcomes that were not said. Treat the transcript as research rather than a
script: drop filler, false starts and repairs, and write in the speaker's voice
without reproducing their hesitations. If the material does not support a
section, write fewer sections.

title — under 200 characters, primary keyword near the front, no clickbait and
no clever-colon constructions. It is the search result, so it wants to read well
at around 60.

h1 — the heading printed on the page, only when it should read differently from
the title. The heading has no length limit and can say the thing properly where
a title clipped for a search result reads abrupt. Leave it empty whenever the
title already works as a heading; a duplicate is one more string to keep in sync
and buys the reader nothing.

description — 140 to 160 characters. It is the teaser on the listing page and
the search snippet, so it must read as a complete promise and end in a full
stop. Primary keyword inside the first 60 characters.

longDescription — 300 to 600 characters, or empty. The same promise with room to
keep it: what the video covers and who it is for, in two or three sentences read
whole rather than cut to a snippet. Leave it empty rather than padding the short
one out to length — the page falls back to `description`, and a longer string
that says no more is worse than the shorter one.

keywords — 5 to 10 phrases someone would actually type into a search box.

keywordTargets — the same thinking, structured, for the 3 to 6 terms that
actually matter. Each is a term, a priority and an intent:

  priority — exactly one target is "primary": the single term this page is for,
  and the one the title and opening are written around. Everything else is
  "secondary".

  intent — what someone typing it is trying to do. "informational" to
  understand, "commercial" to compare options before choosing, "transactional"
  to buy or sign up, "navigational" to reach a specific place. Use only these
  four; leave it out if none of them fits.

  notes — optional, one line, on what the page has to say to satisfy that term.

Ground every term in what the video actually covers. A term the video does not
answer is a page that ranks and then disappoints, which is worse than one that
never ranked.

caption — under 120 characters, shown under the embed. Say what the video shows,
not what the article argues.

faq — 0 to 5 questions the video genuinely answers, each with an answer of one to
three sentences taken from what was actually said. Ask what someone searching
would type, not what the article's own structure implies, and never write a
question the transcript does not answer. These are published as structured data,
so an invented answer is a claim made on the site's behalf. Fewer is better than
padded, and none is a fine result.

blocks — the body, in order, mixing three kinds:

  text — 3 to 6 of them, forming the spine: an opening that states the problem,
  two to four middle sections, and a close. Every section after the opening
  starts with an H2. Inside the html use only <p>, <h2>, <h3>, <ul>, <ol>, <li>,
  <strong>, <em> and <a href>. Never <h1> — the page supplies it — and no inline
  styles, classes or images. The H2 text becomes the page's table of contents, so
  each one has to make sense read on its own, out of order.

  quote — 0 to 2, for a line worth remembering. The highlight must be a literal
  substring of the quote text; it renders in brand orange, and a highlight that
  cannot be found renders nothing.

  table — at most one, and only where the material genuinely compares options or
  summarises structured data. Never force prose into a table. 2 to 5 columns, 2
  to 8 rows, cells under 60 characters, each cell starting with a capital letter,
  and no commas inside a cell — the values are joined on commas downstream.

  embed — an interactive component built for this article. Only the ones listed
  under "Components available" exist; each is named by an id and comes with the
  brief it is being built to. Set `id` to that id and nothing else — no html, no
  text, and never a component key or any config: what the component renders is
  decided by the brief, not here.

  Place it where the article reaches the thing the brief describes, and write the
  surrounding prose as though it is there — introduce it, then carry on past it.
  Use each id at most once, and use every one that is listed: each was requested
  deliberately, and one left unplaced is a component built for nothing. If none
  are listed, there are none; do not invent one.

  figure — a screenshot the author took while recording, placed in the article.
  Only the figures listed under "Figures available" exist; each is named by a
  number and comes with the moment it was taken and a caption already written
  for it. Set `n` to that number and nothing else — no html, no text.

  Place a figure immediately after the paragraph that discusses what it shows,
  and never repeat its caption in the prose: the caption is printed under the
  picture, so a sentence that says the same thing reads as a stutter. Use each
  figure at most once. Leave out any figure the article has no place for — an
  unplaced figure costs nothing, while a figure beside the wrong section is a
  wrong article. If no figures are listed, there are none; do not invent one.

Put each quote directly after the section it came out of. The order of blocks is
the order they are published in."#;

/// What the model returns.
///
/// One flat block list with a `kind` discriminator and every other field
/// optional, because structured-output modes across providers do not reliably
/// support discriminated unions — the same compromise the legacy Python made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ArticleExtraction {
    title: String,
    /// Empty whenever the title already reads as a heading, which is most of
    /// the time.
    #[serde(default)]
    h1: String,
    description: String,
    #[serde(default)]
    long_description: String,
    keywords: Vec<String>,
    #[serde(default)]
    keyword_targets: Vec<ExtractedTarget>,
    caption: String,
    blocks: Vec<ExtractedBlock>,
    #[serde(default)]
    faq: Vec<ExtractedFaq>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedTarget {
    term: String,
    /// `primary` or `secondary`.
    #[serde(default)]
    priority: String,
    /// One of the four the CMS enum allows, or empty.
    #[serde(default)]
    intent: String,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedFaq {
    question: String,
    answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedBlock {
    /// One of `text`, `quote`, `table`, `figure`, `embed`.
    kind: String,
    /// `text` blocks: CKEditor HTML.
    html: Option<String>,
    /// `quote` blocks.
    text: Option<String>,
    highlight: Option<String>,
    /// `table` blocks.
    headers: Option<Vec<String>>,
    rows: Option<Vec<Vec<String>>>,
    /// `figure` blocks: which captured figure to place.
    n: Option<u32>,
    /// `embed` blocks: which requested component to place.
    id: Option<String>,
}

impl ArticleExtraction {
    /// Cleans the output and stamps on what only this side knows.
    ///
    /// Every drop here is a block that would have published as visible damage:
    /// an empty text block is a gap in the article, a quote whose highlight is
    /// not a substring highlights nothing, and a table with no rows renders as
    /// an empty grid.
    fn into_article(
        self,
        version: Option<u32>,
        prompt: &crate::agent::prompt::Resolved,
        offered: &[FigureOffer],
        requested: &[EmbedOffer],
    ) -> Article {
        let available: Vec<u32> = offered.iter().map(|figure| figure.n).collect();
        let title = clipped(&self.title, TITLE_MAX);
        Article {
            version,
            prompt_version: prompt.version,
            prompt_hash: prompt.hash.clone(),
            slug: slugify(&title, SLUG_MAX),
            h1: heading(&self.h1, &title),
            title,
            description: clipped(&self.description, DESCRIPTION_MAX),
            long_description: clipped(&self.long_description, LONG_DESCRIPTION_MAX),
            // Neither is a decision a model gets to make. `noIndex` hides a page
            // from search and `canonicalUrl` hands another URL the credit for
            // it; both are answers to "where else does this exist", which is
            // something only the person publishing knows. They default off and
            // are edited into `article.json` by hand.
            no_index: false,
            canonical_url: String::new(),
            caption: clipped(&self.caption, CAPTION_MAX),
            keywords: lines(self.keywords),
            keyword_targets: targeted(self.keyword_targets),
            blocks: placed(self.blocks, &available, requested),
            faq: asked(self.faq),
        }
    }
}

/// The on-page heading, kept only when it is really a different heading.
///
/// A model asked for an optional field will fill it in, and what it fills in is
/// usually the title again — occasionally with the punctuation changed. Either
/// way the site would render the same words the fallback already gives it, off
/// a second string nobody is maintaining. Compared loosely, because "Why
/// watermarking fails." and "Why watermarking fails" are not two headings.
fn heading(raw: &str, title: &str) -> String {
    let heading = raw.trim();
    let same = |value: &str| {
        value
            .chars()
            .filter(|ch| ch.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    match heading.is_empty() || same(heading) == same(title) {
        true => String::new(),
        false => heading.to_string(),
    }
}

/// The four values the CMS enum accepts. Anything else is refused for the whole
/// entry, so an invented intent would fail the create rather than the field.
const INTENTS: [&str; 4] = ["informational", "commercial", "transactional", "navigational"];
/// Past six the brief stops being a brief.
const TARGETS_MAX: usize = 6;

/// The keyword targets, with exactly one primary.
///
/// Enforced rather than trusted, in both directions. A model asked for "exactly
/// one" returns two often enough to matter, and two primaries is a page with no
/// primary — while a list where every term is secondary says the page is not
/// really for any of them. So the first primary keeps the title and every later
/// one is demoted, and a list that named none promotes its first.
fn targeted(raw: Vec<ExtractedTarget>) -> Vec<super::schema::KeywordTarget> {
    let mut out: Vec<super::schema::KeywordTarget> = raw
        .into_iter()
        .filter_map(|target| {
            let term = target.term.trim().to_string();
            if term.is_empty() {
                return None;
            }
            let intent = target.intent.trim().to_lowercase();
            Some(super::schema::KeywordTarget {
                term,
                priority: match target.priority.trim().eq_ignore_ascii_case("primary") {
                    true => "primary".to_string(),
                    false => "secondary".to_string(),
                },
                intent: match INTENTS.contains(&intent.as_str()) {
                    true => intent,
                    // Dropped, not guessed: the enum is closed and a value
                    // outside it fails the whole create, while an absent one is
                    // simply a target nobody classified.
                    false => String::new(),
                },
                notes: target.notes.trim().to_string(),
            })
        })
        .take(TARGETS_MAX)
        .collect();

    let mut seen_primary = false;
    for target in out.iter_mut() {
        match target.priority == "primary" {
            true if seen_primary => target.priority = "secondary".to_string(),
            true => seen_primary = true,
            false => {}
        }
    }
    if !seen_primary {
        if let Some(first) = out.first_mut() {
            first.priority = "primary".to_string();
        }
    }
    out
}

/// The FAQ, with the entries that would publish as damage removed.
///
/// A half-filled pair is worse here than anywhere else in the article: these
/// become `FAQPage` structured data, so a question with no answer is a claim
/// made to a search engine on the site's behalf. The cap is enforced in Rust
/// for the reason the module header gives — a count in the prompt is advisory.
fn asked(raw: Vec<ExtractedFaq>) -> Vec<super::schema::Faq> {
    raw.into_iter()
        .filter_map(|entry| {
            let question = entry.question.trim().to_string();
            let answer = entry.answer.trim().to_string();
            (!question.is_empty() && !answer.is_empty())
                .then_some(super::schema::Faq { question, answer })
        })
        .take(FAQ_MAX)
        .collect()
}

/// The blocks, with each figure used at most once.
///
/// The de-dupe is not tidiness. A figure placed twice publishes the same picture
/// and the same caption in two places, which reads as an editing mistake — and
/// it is the failure a model makes when it wants to refer back to something it
/// has already shown.
fn placed(
    raw: Vec<ExtractedBlock>,
    available: &[u32],
    requested: &[EmbedOffer],
) -> Vec<Block> {
    let mut used: Vec<u32> = Vec::new();
    let mut placed_ids: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for extracted in raw {
        let Some(block) = block(extracted, available, requested) else {
            continue;
        };
        match &block {
            Block::Figure { n } => {
                if used.contains(n) {
                    eprintln!("stream-recorder: dropping figure {n:02}, already placed");
                    continue;
                }
                used.push(*n);
            }
            // Same de-dupe and the same reason: one component rendered twice on
            // one page reads as an editing mistake, and it is what a model does
            // when it wants to refer back to something it has already shown.
            Block::Embed { id } => {
                if placed_ids.contains(id) {
                    eprintln!("stream-recorder: dropping component {id:?}, already placed");
                    continue;
                }
                placed_ids.push(id.clone());
            }
            _ => {}
        }
        out.push(block);
    }
    out
}

/// One component the article may place, as the prompt describes it.
///
/// The brief, not the built block: at the moment the article is written nothing
/// has been built yet, and what the model needs in order to place it well is
/// what it is *for*.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedOffer {
    pub id: String,
    pub brief: String,
}

/// One figure the article may place, as the prompt describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct FigureOffer {
    pub n: u32,
    /// `ch 03 · 1:24`.
    pub moment: String,
    /// The blurb already written for it.
    pub caption: String,
}

fn block(raw: ExtractedBlock, available: &[u32], requested: &[EmbedOffer]) -> Option<Block> {
    match raw.kind.trim().to_lowercase().as_str() {
        "text" => {
            let html = raw.html.unwrap_or_default().trim().to_string();
            (!html.is_empty()).then_some(Block::Text { html })
        }
        "quote" => {
            let text = raw.text.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return None;
            }
            // Kept only when it is really in the quote. A highlight the reader
            // cannot find is silently invisible, which looks like a styling bug
            // rather than a bad field.
            let highlight = raw
                .highlight
                .unwrap_or_default()
                .trim()
                .to_string();
            let highlight = if !highlight.is_empty() && text.contains(&highlight) {
                highlight
            } else {
                String::new()
            };
            Some(Block::Quote { text, highlight })
        }
        "table" => {
            let headers = lines(raw.headers.unwrap_or_default());
            let rows: Vec<Vec<String>> = raw
                .rows
                .unwrap_or_default()
                .into_iter()
                .map(lines)
                .filter(|row| !row.is_empty())
                .collect();
            (!headers.is_empty() && !rows.is_empty()).then_some(Block::Table { headers, rows })
        }
        "embed" => {
            // An id nobody requested is the embed failure that would otherwise
            // stay invisible until the page was live: the CMS drops a block it
            // cannot resolve without complaining, leaving a gap in the argument
            // the prose still refers to.
            let id = raw.id?.trim().to_string();
            match !id.is_empty() && requested.iter().any(|offer| offer.id == id) {
                true => Some(Block::Embed { id }),
                false => {
                    eprintln!("stream-recorder: dropping component {id:?}, which was not requested");
                    None
                }
            }
        }
        "figure" => {
            // A number nobody captured is the one figure failure that would be
            // invisible until the page was live: the block would publish with
            // no `src` and render as a broken image.
            let n = raw.n?;
            match available.contains(&n) {
                true => Some(Block::Figure { n }),
                false => {
                    eprintln!("stream-recorder: dropping figure {n}, which was not offered");
                    None
                }
            }
        }
        other => {
            eprintln!("stream-recorder: dropping unknown blog block kind {other:?}");
            None
        }
    }
}

fn lines(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Character-wise so a multi-byte string is cut at a boundary, and on a word
/// boundary where there is one — a headline cut mid-word reads as a bug.
fn clipped(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    match cut.rsplit_once(' ') {
        Some((head, _)) if head.chars().count() >= max / 2 => head.trim_end().to_string(),
        _ => cut,
    }
}

pub fn generate_article(
    longform: &Longform,
    version: Option<u32>,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
    figures: &[FigureOffer],
    components: &[EmbedOffer],
) -> Result<(Article, crate::agent::trace::LlmStep)> {
    if longform.chapters.is_empty() {
        bail!("no transcribed chapters to write an article from");
    }

    let mut system =
        crate::agent::prompt::resolve(crate::agent::prompt::BLOG, SYSTEM_PROMPT, prompt_root);
    if let Some(note) = stale_preamble(&system, figures, components) {
        eprintln!("stream-recorder: {note}");
        system = crate::agent::prompt::builtin_version(SYSTEM_PROMPT);
    }
    let prompt = build_user_prompt(longform, figures, components);

    let (extracted, step) = crate::agent::extract::extract::<ArticleExtraction>(
        crate::agent::prompt::BLOG,
        "blog",
        &system,
        prompt,
        model,
        provider,
    )?;

    let article = extracted.into_article(version, &system, figures, components);
    if !article.is_publishable() {
        bail!("the model returned nothing publishable — no title, slug, description or body");
    }
    Ok((article, step))
}

/// Why the standing prompt cannot run this draft, or `None` when it can.
///
/// The builtin describes every block kind, but a standing overlay in the prompt
/// library is a copy of the builtin as it was the day it was seeded — see
/// [`crate::agent::prompt::ensure_library_overlay`] — and one seeded before
/// figures or components existed says nothing about them. The offers still
/// reach the model in the user prompt, but a kind the preamble never named is a
/// kind the model never emits, and every figure captured for the video is left
/// out of the article without a word. That is exactly what happened to the
/// first figure-bearing drafts, and why this exists: a preamble that does not
/// describe a kind this draft needs is not used, and the builtin runs instead.
///
/// Relative to what is offered, deliberately. A stale prompt costs nothing on a
/// video with no figures, and a house prompt someone tuned should keep running
/// for as long as it can.
pub fn stale_preamble(
    system: &crate::agent::prompt::Resolved,
    figures: &[FigureOffer],
    components: &[EmbedOffer],
) -> Option<String> {
    if system.is_builtin() {
        return None;
    }
    let mut missing = Vec::new();
    if !figures.is_empty() && !describes_block(&system.text, "figure") {
        missing.push("figure");
    }
    if !components.is_empty() && !describes_block(&system.text, "embed") {
        missing.push("embed");
    }
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "the standing blog prompt ({}) predates the {} block and would leave every offered \
         one out, so this draft runs on the builtin — Edit Prompt and re-seed it to keep your \
         changes",
        system.label(),
        missing.join(" and "),
    ))
}

/// [`stale_preamble`] for a caller that has not resolved the prompt itself: the
/// status line a draft starts with, which should say up front that the standing
/// prompt is being bypassed rather than leave it to the log.
pub fn stale_preamble_note(
    prompt_root: Option<&std::path::Path>,
    figures: &[FigureOffer],
    components: &[EmbedOffer],
) -> Option<String> {
    let system =
        crate::agent::prompt::resolve(crate::agent::prompt::BLOG, SYSTEM_PROMPT, prompt_root);
    stale_preamble(&system, figures, components)
}

/// Whether `preamble` introduces `kind` as a block: a line that opens with the
/// kind's name and then a dash or a colon, the way the builtin's block list is
/// written. Deliberately not a bare word search — "embed" appears in any prompt
/// that mentions the video embed at the top of the page, and the overlay this
/// was written against mentioned it three times without describing the block.
fn describes_block(preamble: &str, kind: &str) -> bool {
    preamble.lines().any(|line| {
        line.trim_start()
            .strip_prefix(kind)
            .map(str::trim_start)
            .is_some_and(|rest| rest.starts_with(['—', '–', '-', ':']))
    })
}

/// The whole longform, chapter by chapter — the same shape the Substack and Post
/// stages send, so all three are reading the same evidence.
fn build_user_prompt(
    longform: &Longform,
    figures: &[FigureOffer],
    components: &[EmbedOffer],
) -> String {
    let mut out = format!("Video: {}\n", longform.project_title);
    // Before the transcript rather than after it. The transcript is long, and a
    // list of what may be illustrated is only useful while the material it
    // belongs to is still being read.
    if !figures.is_empty() {
        out.push_str("\nFigures available (reference by number):\n");
        for figure in figures {
            out.push_str(&format!(
                "- figure {} ({}): {}\n",
                figure.n, figure.moment, figure.caption
            ));
        }
    }
    if !components.is_empty() {
        out.push_str("\nComponents available (reference by id):\n");
        for offer in components {
            out.push_str(&format!("- {}: {}\n", offer.id, offer.brief));
        }
    }
    if !longform.links.is_empty() {
        out.push_str("\nAlready published at:\n");
        for link in &longform.links {
            out.push_str(&format!("- {}: {}\n", link.label, link.url));
        }
    }
    for chapter in &longform.chapters {
        out.push_str(&format!("\n<Chapter {}>\n", chapter.n));
        if !chapter.title.trim().is_empty() {
            out.push_str(&format!("Working title: {}\n", chapter.title.trim()));
        }
        if !chapter.points.is_empty() {
            out.push_str("Notes:\n");
            for point in &chapter.points {
                out.push_str(&format!("- {point}\n"));
            }
        }
        if !chapter.transcript.trim().is_empty() {
            out.push_str(&format!("Transcript:\n{}\n", chapter.transcript.trim()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::longform::ChapterContext;

    fn resolved() -> crate::agent::prompt::Resolved {
        crate::agent::prompt::Resolved {
            text: "guidance".into(),
            version: Some(3),
            hash: "feedfacefeedface".into(),
        }
    }

    fn text(html: &str) -> ExtractedBlock {
        ExtractedBlock {
            kind: "text".into(),
            html: Some(html.into()),
            text: None,
            highlight: None,
            headers: None,
            rows: None,
            n: None,
            id: None,
        }
    }

    fn figure_block(n: u32) -> ExtractedBlock {
        ExtractedBlock {
            kind: "figure".into(),
            html: None,
            text: None,
            highlight: None,
            headers: None,
            rows: None,
            n: Some(n),
            id: None,
        }
    }

    fn offered(numbers: &[u32]) -> Vec<FigureOffer> {
        numbers
            .iter()
            .map(|n| FigureOffer {
                n: *n,
                moment: format!("ch 01 · 0:{n:02}"),
                caption: format!("Figure {n} shows something."),
            })
            .collect()
    }

    fn quote(text_: &str, highlight: &str) -> ExtractedBlock {
        ExtractedBlock {
            kind: "quote".into(),
            html: None,
            text: Some(text_.into()),
            highlight: Some(highlight.into()),
            headers: None,
            rows: None,
            n: None,
            id: None,
        }
    }

    fn extraction(blocks: Vec<ExtractedBlock>) -> ArticleExtraction {
        ArticleExtraction {
            title: "  Why watermarking fails  ".into(),
            h1: String::new(),
            description: "  A complete promise of the argument.  ".into(),
            long_description: String::new(),
            keywords: vec!["ai".into(), "   ".into(), " watermarking ".into()],
            keyword_targets: Vec::new(),
            caption: " Four minutes on enforcement economics ".into(),
            blocks,
            faq: Vec::new(),
        }
    }

    fn asks(question: &str, answer: &str) -> ExtractedFaq {
        ExtractedFaq { question: question.into(), answer: answer.into() }
    }

    /// The heading is optional and a model asked for an optional field fills it
    /// in anyway — usually with the title, occasionally with the title and a
    /// full stop. Both are the fallback the site already applies, off a second
    /// string nobody maintains.
    #[test]
    fn a_heading_that_is_only_the_title_again_is_dropped() {
        assert_eq!(heading("Why watermarking fails", "Why watermarking fails"), "");
        assert_eq!(heading("  Why watermarking fails.  ", "Why watermarking fails"), "");
        assert_eq!(heading("why watermarking FAILS", "Why watermarking fails"), "");
        assert_eq!(heading("   ", "Why watermarking fails"), "");
    }

    /// A heading that really is a different heading survives, untrimmed of its
    /// length: the field has no ceiling and the whole point is that it can say
    /// what a 60-character title cannot.
    #[test]
    fn a_heading_that_says_something_else_is_kept() {
        let got = heading(
            "  Why watermarking fails, and what the enforcement bill actually looks like  ",
            "Why watermarking fails",
        );
        assert_eq!(
            got,
            "Why watermarking fails, and what the enforcement bill actually looks like"
        );
    }

    /// These publish as `FAQPage` structured data, so a half-filled pair is a
    /// claim made to a search engine with nothing behind it.
    #[test]
    fn a_faq_entry_missing_either_half_is_dropped() {
        let got = asked(vec![
            asks("  Does it scale?  ", "  Not past the first retry.  "),
            asks("What about cost?", "   "),
            asks("", "An answer to nothing."),
        ]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].question, "Does it scale?");
        assert_eq!(got[0].answer, "Not past the first retry.");
    }

    /// The count in the prompt is advisory — models treat bounds that way, which
    /// is why every other length rule here is enforced in Rust too.
    #[test]
    fn the_faq_is_capped_however_many_come_back() {
        let many: Vec<_> = (0..9).map(|n| asks(&format!("Q{n}?"), "Yes.")).collect();
        let got = asked(many);
        assert_eq!(got.len(), FAQ_MAX);
        assert_eq!(got[0].question, "Q0?", "the first ones, not a random five");
    }

    /// Written into the draft, so the preview can show them and the publish can
    /// send them — the two places every other generated field has to reach.
    #[test]
    fn the_heading_and_the_faq_reach_the_article() {
        let mut raw = extraction(vec![text("<p>Body.</p>")]);
        raw.h1 = "Why watermarking fails, and what enforcement really costs".into();
        raw.faq = vec![asks("Does it scale?", "Not past the first retry.")];
        let article = raw.into_article(None, &resolved(), &[], &[]);
        assert!(article.h1.starts_with("Why watermarking fails, and what"));
        assert_eq!(article.faq.len(), 1);
        assert_eq!(article.faq[0].answer, "Not past the first retry.");
    }

    fn requests(ids: &[&str]) -> Vec<EmbedOffer> {
        ids.iter()
            .map(|id| EmbedOffer {
                id: (*id).into(),
                brief: format!("A component showing {id}"),
            })
            .collect()
    }

    fn embed(id: &str) -> ExtractedBlock {
        ExtractedBlock {
            kind: "embed".into(),
            html: None,
            text: None,
            highlight: None,
            headers: None,
            rows: None,
            n: None,
            id: Some(id.into()),
        }
    }

    /// A reference, never a copy: the article carries the id and nothing else,
    /// because the block itself is built and validated in the landing repo.
    #[test]
    fn a_requested_component_is_placed_by_id() {
        let article = extraction(vec![text("<p>Body.</p>"), embed("retry_steps")])
            .into_article(None, &resolved(), &[], &requests(&["retry_steps"]));
        assert_eq!(article.blocks.len(), 2);
        assert!(matches!(&article.blocks[1], Block::Embed { id } if id == "retry_steps"));
        assert_eq!(article.embeds(), vec!["retry_steps".to_string()]);
    }

    /// An id nobody requested is nothing that will ever be built, and the CMS
    /// drops a block it cannot resolve without complaining — leaving a gap the
    /// surrounding prose still refers to.
    #[test]
    fn a_component_nobody_requested_is_dropped() {
        let article = extraction(vec![embed("invented_thing")])
            .into_article(None, &resolved(), &[], &requests(&["retry_steps"]));
        assert!(article.blocks.is_empty(), "{:?}", article.blocks);
        assert!(article.embeds().is_empty());
    }

    /// Same de-dupe as a figure, and the same reason: one component rendered
    /// twice on one page reads as an editing mistake.
    #[test]
    fn a_component_placed_twice_is_placed_once() {
        let article = extraction(vec![
            embed("retry_steps"),
            text("<p>Body.</p>"),
            embed("retry_steps"),
        ])
        .into_article(None, &resolved(), &[], &requests(&["retry_steps"]));
        assert_eq!(article.blocks.len(), 2);
        assert_eq!(article.embeds().len(), 1);
    }

    /// The briefs go in the prompt, before the transcript, for the reason the
    /// figures do: a list of what may be shown is only useful while the material
    /// it belongs to is still being read.
    #[test]
    fn the_prompt_lists_the_components_with_their_briefs() {
        let prompt = build_user_prompt(&one_chapter(), &[], &requests(&["retry_steps"]));
        assert!(prompt.contains("Components available (reference by id):"));
        assert!(prompt.contains("- retry_steps: A component showing retry_steps"));
        let at = prompt.find("Components available").expect("the list");
        assert!(at < prompt.find("Transcript:").unwrap_or(usize::MAX), "before the transcript");
    }

    fn target(term: &str, priority: &str, intent: &str) -> ExtractedTarget {
        ExtractedTarget {
            term: term.into(),
            priority: priority.into(),
            intent: intent.into(),
            notes: String::new(),
        }
    }

    /// Two primaries is a page with no primary, and a model asked for "exactly
    /// one" returns two often enough to matter. The first keeps it.
    #[test]
    fn a_second_primary_target_is_demoted() {
        let got = targeted(vec![
            target("ai watermarking", "primary", "informational"),
            target("watermark removal", "PRIMARY", "commercial"),
            target("provenance", "secondary", ""),
        ]);
        assert_eq!(got[0].priority, "primary");
        assert_eq!(got[1].priority, "secondary", "two primaries is none");
        assert_eq!(got[2].priority, "secondary");
    }

    /// A list where everything is secondary says the page is not really for any
    /// of them, so the first term is promoted.
    #[test]
    fn a_brief_with_no_primary_promotes_its_first_term() {
        let got = targeted(vec![target("ai watermarking", "secondary", "")]);
        assert_eq!(got[0].priority, "primary");
        assert!(targeted(Vec::new()).is_empty(), "and nothing is still nothing");
    }

    /// The enum is closed: a value outside it is refused for the whole entry,
    /// so an invented intent has to become an absent one rather than travel.
    #[test]
    fn an_intent_outside_the_enum_is_dropped_not_guessed() {
        let got = targeted(vec![
            target("a", "primary", "  Informational  "),
            target("b", "secondary", "purchase-intent"),
        ]);
        assert_eq!(got[0].intent, "informational", "trimmed and lowercased");
        assert_eq!(got[1].intent, "", "not one of the four");
        for intent in INTENTS {
            assert_eq!(targeted(vec![target("t", "primary", intent)])[0].intent, intent);
        }
    }

    #[test]
    fn a_brief_is_capped_and_a_termless_target_is_dropped() {
        let many: Vec<_> = (0..9).map(|n| target(&format!("term {n}"), "secondary", "")).collect();
        assert_eq!(targeted(many).len(), TARGETS_MAX);
        assert!(targeted(vec![target("   ", "primary", "")]).is_empty());
    }

    /// The two optional SEO fields, and the one rule that keeps each of them
    /// from being filled in for its own sake: a heading is only worth having
    /// when it differs, and a question is only worth asking when the video
    /// answers it.
    #[test]
    fn the_builtin_prompt_asks_for_the_heading_and_the_faq_on_terms() {
        assert!(SYSTEM_PROMPT.contains("h1 —"));
        assert!(SYSTEM_PROMPT.contains("Leave it empty"));
        assert!(SYSTEM_PROMPT.contains("faq —"));
        assert!(SYSTEM_PROMPT.contains("never write a\nquestion the transcript does not answer"));
        assert!(SYSTEM_PROMPT.contains("none is a fine result"));
    }

    /// The one instruction that separates this prompt from `substack.notes`:
    /// finished prose, not beats. Losing it makes the two prompts the same job.
    #[test]
    fn the_builtin_prompt_asks_for_a_standalone_article() {
        assert!(SYSTEM_PROMPT.contains("stand on its own"));
        assert!(SYSTEM_PROMPT.contains("Never open with \"in this video\""));
        assert!(!SYSTEM_PROMPT.to_lowercase().contains("beats, not"));
    }

    /// The shape is enforced by `JsonSchema`, so the prompt must never describe
    /// it — an overlay that rewrote a described schema could break parsing.
    #[test]
    fn the_builtin_prompt_carries_no_output_schema() {
        let lowered = SYSTEM_PROMPT.to_lowercase();
        for word in ["json", "schema", "\"title\":", "array of"] {
            assert!(!lowered.contains(word), "prompt describes output: {word}");
        }
    }

    /// Three rules that are not style — each is a real downstream constraint, and
    /// an overlay author who drops one produces damage that renders silently.
    #[test]
    fn the_builtin_prompt_states_the_downstream_constraints() {
        // The landing builds its table of contents by regexing the H2s.
        assert!(SYSTEM_PROMPT.contains("table of contents"));
        // A highlight not found in the quote renders nothing.
        assert!(SYSTEM_PROMPT.contains("substring of the quote"));
        // Table cells are flattened by joining on commas.
        assert!(SYSTEM_PROMPT.contains("no commas inside a cell"));
        // The page supplies the H1; a second one is an SEO fault.
        assert!(SYSTEM_PROMPT.contains("Never <h1>"));
    }

    /// A highlight the reader cannot find in the quote is invisible, which reads
    /// as a styling bug rather than a bad field — so it is dropped here.
    #[test]
    fn a_highlight_that_is_not_in_the_quote_is_dropped() {
        let got = extraction(vec![
            quote("It could not.", "could not"),
            quote("It could not.", "never said this"),
        ])
        .into_article(None, &resolved(), &[], &[]);
        assert_eq!(
            got.blocks[0],
            Block::Quote { text: "It could not.".into(), highlight: "could not".into() }
        );
        assert_eq!(
            got.blocks[1],
            Block::Quote { text: "It could not.".into(), highlight: String::new() }
        );
    }

    #[test]
    fn an_empty_block_is_dropped_rather_than_published_as_a_gap() {
        let got = extraction(vec![
            text("<p>Real.</p>"),
            text("   "),
            quote("  ", "x"),
            ExtractedBlock {
                kind: "table".into(),
                html: None,
                text: None,
                highlight: None,
                headers: Some(vec!["A".into(), "B".into()]),
                rows: Some(Vec::new()),
                n: None,
                id: None,
            },
        ])
        .into_article(None, &resolved(), &[], &[]);
        assert_eq!(got.blocks.len(), 1);
        assert_eq!(got.blocks[0], Block::Text { html: "<p>Real.</p>".into() });
    }

    #[test]
    fn an_unknown_block_kind_is_dropped() {
        let mut odd = text("<p>x</p>");
        odd.kind = "video".into();
        let got = extraction(vec![odd]).into_article(None, &resolved(), &[], &[]);
        assert!(got.blocks.is_empty());
        assert!(!got.is_publishable(), "a body of nothing is not publishable");
    }

    /// The order of blocks is the order they publish in, so a quote stays under
    /// the section it came out of.
    #[test]
    fn the_block_order_is_preserved() {
        let got = extraction(vec![
            text("<h2>One</h2>"),
            quote("Said it.", ""),
            text("<h2>Two</h2>"),
        ])
        .into_article(None, &resolved(), &[], &[]);
        assert!(matches!(got.blocks[0], Block::Text { .. }));
        assert!(matches!(got.blocks[1], Block::Quote { .. }));
        assert!(matches!(got.blocks[2], Block::Text { .. }));
    }

    #[test]
    fn the_slug_is_derived_from_the_trimmed_title() {
        let got = extraction(vec![text("<p>x</p>")]).into_article(None, &resolved(), &[], &[]);
        assert_eq!(got.title, "Why watermarking fails");
        assert_eq!(got.slug, "why-watermarking-fails");
        assert_eq!(got.keywords, vec!["ai", "watermarking"]);
        assert_eq!(got.caption, "Four minutes on enforcement economics");
    }

    /// Strapi rejects an overlong title outright, so it is cut before the
    /// request — and cut at a space, because a headline severed mid-word reads
    /// as a bug rather than an edit.
    #[test]
    fn an_overlong_title_is_cut_on_a_word_boundary() {
        let long = "word ".repeat(80);
        let mut raw = extraction(vec![text("<p>x</p>")]);
        raw.title = long;
        let got = raw.into_article(None, &resolved(), &[], &[]);
        assert!(got.title.chars().count() <= TITLE_MAX);
        assert!(!got.title.ends_with(' '));
        assert!(got.slug.chars().count() <= SLUG_MAX);
    }

    #[test]
    fn a_multibyte_field_is_cut_without_panicking() {
        let mut raw = extraction(vec![text("<p>x</p>")]);
        raw.description = "é".repeat(400);
        let got = raw.into_article(None, &resolved(), &[], &[]);
        assert_eq!(got.description.chars().count(), DESCRIPTION_MAX);
    }

    /// Which prompt wrote it travels with the article, so Reflect can tell a good
    /// run from a bad one without guessing.
    #[test]
    fn the_article_records_which_prompt_wrote_it() {
        let got = extraction(vec![text("<p>x</p>")]).into_article(Some(5), &resolved(), &[], &[]);
        assert_eq!(got.version, Some(5));
        assert_eq!(got.prompt_version, Some(3));
        assert_eq!(got.prompt_hash, "feedfacefeedface");
    }

    fn one_chapter() -> Longform {
        Longform {
            project_title: "A video".into(),
            chapters: vec![ChapterContext {
                n: 1,
                title: "Where it broke".into(),
                points: Vec::new(),
                transcript: "the table had no ceiling".into(),
                figures: Vec::new(),
            }],
            timestamps: Vec::new(),
            loose_figures: Vec::new(),
            links: Vec::new(),
        }
    }

    /// A figure block is a reference and nothing else, so this is the whole
    /// contract: the number survives and no caption is copied into the article.
    #[test]
    fn an_offered_figure_is_placed_by_number() {
        let got = extraction(vec![text("<p>Body.</p>"), figure_block(2)])
            .into_article(None, &resolved(), &offered(&[1, 2]), &[]);
        assert_eq!(got.blocks.len(), 2);
        assert_eq!(got.blocks[1], Block::Figure { n: 2 });
        assert_eq!(got.figures(), vec![2]);
    }

    /// A number nobody captured would publish as a `content.image` with no
    /// `src` — a broken image on a live page, and the one figure failure that
    /// is invisible until then.
    #[test]
    fn a_figure_that_was_never_offered_is_dropped() {
        let got = extraction(vec![text("<p>Body.</p>"), figure_block(9)])
            .into_article(None, &resolved(), &offered(&[1, 2]), &[]);
        assert_eq!(got.blocks.len(), 1);
        assert!(got.figures().is_empty());
    }

    /// The same picture and caption twice reads as an editing mistake, and it is
    /// what a model does when it wants to refer back to something.
    #[test]
    fn a_figure_placed_twice_is_placed_once() {
        let got = extraction(vec![figure_block(1), text("<p>Body.</p>"), figure_block(1)])
            .into_article(None, &resolved(), &offered(&[1]), &[]);
        assert_eq!(got.figures(), vec![1]);
        assert_eq!(
            got.blocks.len(),
            2,
            "the text block was collateral: {:?}",
            got.blocks
        );
    }

    /// A `figure` with no number at all is not a placement.
    #[test]
    fn a_figure_block_with_no_number_is_dropped() {
        let mut raw = figure_block(1);
        raw.n = None;
        let got = extraction(vec![raw]).into_article(None, &resolved(), &offered(&[1]), &[]);
        assert!(got.figures().is_empty());
    }

    /// The manifest is what makes placement possible — without it the model is
    /// choosing positions for pictures it has been told nothing about.
    #[test]
    fn the_prompt_lists_the_figures_and_their_moments() {
        let prompt = build_user_prompt(&one_chapter(), &offered(&[3]), &[]);
        assert!(prompt.contains("Figures available"), "{prompt}");
        assert!(
            prompt.contains("- figure 3 (ch 01 · 0:03): Figure 3 shows something."),
            "{prompt}"
        );
        // Ahead of the transcript, which is the long part: a list of what may be
        // illustrated is only useful while the material is still being read.
        let manifest_at = prompt.find("Figures available").expect("a manifest");
        let chapter_at = prompt.find("<Chapter").expect("a chapter");
        assert!(
            manifest_at < chapter_at,
            "the manifest is buried under the transcript"
        );
    }

    /// The failure this guards against, as it happened: a library overlay seeded
    /// before figures existed lists text, quote and table; the user prompt
    /// offers three figures; and the model, never told a `figure` block exists,
    /// places none. Such a preamble is not used for a draft that offers them.
    #[test]
    fn a_preamble_that_predates_figures_is_not_used_when_figures_are_offered() {
        let stale = crate::agent::prompt::Resolved {
            text: "blocks — the body, in order, mixing three kinds:\n\n  text — the spine.\n\n  \
                   quote — a line worth remembering.\n\n  table — at most one.\n\nThe video is \
                   embedded at the top; the article has to stand on its own if the\nembed fails."
                .into(),
            version: None,
            hash: "6f1467aaeaaabf7e".into(),
        };
        let component = EmbedOffer { id: "steps".into(), brief: "How the retry works.".into() };

        let note = stale_preamble(&stale, &offered(&[1]), &[]).expect("a stale prompt is noticed");
        assert!(note.contains("predates the figure block"), "{note}");
        assert!(note.contains("unversioned (6f1467aa"), "names the prompt that ran: {note}");
        assert!(note.contains("Edit Prompt"), "says what to do about it: {note}");

        let note = stale_preamble(&stale, &[], std::slice::from_ref(&component)).unwrap();
        assert!(note.contains("predates the embed block"), "{note}");
        assert!(!note.contains("figure"), "nothing was said about figures: {note}");

        let both = stale_preamble(&stale, &offered(&[1]), std::slice::from_ref(&component)).unwrap();
        assert!(both.contains("figure and embed block"), "{both}");
    }

    /// Stale only relative to what is offered. A video with nothing to place
    /// runs on whatever prompt is standing, and a prompt that does describe the
    /// kinds — the builtin, or a house prompt seeded from it and tuned — is left
    /// alone however it is versioned: the check reads the text, not the label.
    #[test]
    fn a_preamble_is_only_stale_relative_to_what_is_offered() {
        let stale = crate::agent::prompt::Resolved {
            text: "text — the spine.".into(),
            version: None,
            hash: "abc".into(),
        };
        assert_eq!(stale_preamble(&stale, &[], &[]), None);

        let component = EmbedOffer { id: "steps".into(), brief: "b".into() };
        let builtin = crate::agent::prompt::builtin_version(SYSTEM_PROMPT);
        assert_eq!(
            stale_preamble(&builtin, &offered(&[1]), std::slice::from_ref(&component)),
            None
        );

        let house = crate::agent::prompt::Resolved {
            text: SYSTEM_PROMPT.replace("saagasolve.com", "example.com"),
            version: None,
            hash: "def".into(),
        };
        assert_eq!(
            stale_preamble(&house, &offered(&[1]), std::slice::from_ref(&component)),
            None
        );
    }

    /// The word alone is not enough: every article prompt mentions the video
    /// embed, and the old overlay did, without describing an `embed` block.
    #[test]
    fn a_block_is_described_by_its_own_line_not_by_a_passing_mention() {
        assert!(describes_block("  embed — an interactive component.", "embed"));
        assert!(describes_block("figure: a screenshot", "figure"));
        assert!(describes_block("figure - a screenshot", "figure"));
        assert!(!describes_block("if the\nembed fails, the article stands alone", "embed"));
        assert!(!describes_block("Use each figure at most once.", "figure"));
        assert!(!describes_block("figures — the plural is a different word", "figure"));
    }

    /// Most videos have no figures, and the prompt must not imply otherwise.
    #[test]
    fn a_video_with_no_figures_has_no_manifest() {
        let prompt = build_user_prompt(&one_chapter(), &[], &[]);
        assert!(!prompt.contains("Figures available"), "{prompt}");
    }

    #[test]
    fn the_user_prompt_labels_every_chapter_and_carries_its_words() {
        let longform = Longform {
            project_title: "A video".into(),
            chapters: vec![
                ChapterContext {
                    n: 1,
                    title: "Where it broke".into(),
                    points: vec!["no ceiling".into()],
                    transcript: "the table had no ceiling".into(),
                    figures: Vec::new(),
                },
                ChapterContext {
                    n: 2,
                    title: "The fix".into(),
                    points: Vec::new(),
                    transcript: "we added one".into(),
                    figures: Vec::new(),
                },
            ],
            timestamps: Vec::new(),
            loose_figures: Vec::new(),
            links: vec![crate::longform::Link {
                label: "Watch on YouTube".into(),
                url: "https://y/1".into(),
            }],
        };
        let prompt = build_user_prompt(&longform, &[], &[]);
        assert!(prompt.contains("<Chapter 1>"));
        assert!(prompt.contains("<Chapter 2>"));
        assert!(prompt.contains("the table had no ceiling"));
        assert!(prompt.contains("we added one"));
        assert!(prompt.contains("- no ceiling"));
        assert!(prompt.contains("https://y/1"));
    }

    #[test]
    fn nothing_transcribed_is_refused_before_spending_a_call() {
        let empty = Longform {
            project_title: "A video".into(),
            chapters: Vec::new(),
            timestamps: Vec::new(),
            loose_figures: Vec::new(),
            links: Vec::new(),
        };
        let err = generate_article(&empty, None, "m", None, None, &[], &[]).unwrap_err();
        assert!(err.to_string().contains("no transcribed chapters"));
    }
}
