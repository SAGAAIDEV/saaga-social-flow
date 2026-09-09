//! What the CMS will refuse, measured before anything is uploaded.
//!
//! Two kinds of limit, and the second is the one that hurt. A `maxLength` in
//! the `strapi-cms` schema is checked by Strapi and comes back as a `400`
//! naming the field. But a Strapi `string` is a `varchar(255)` column in
//! Postgres whether or not the schema declares a length, and a value past that
//! is refused by the database rather than by Strapi — a bare `500 Internal
//! Server Error` with nothing in it, after the thumbnail, the OG image and the
//! portrait poster had all gone up. A 326-character pull quote did exactly
//! that against `cms.saagasolve.com`, three publishes in a row, while the same
//! body was accepted by a local Strapi on SQLite, which does not enforce
//! column widths at all.
//!
//! So every string the pipeline writes into a `string` field is checked here
//! against the column, and every declared `maxLength` and closed enum against
//! the schema, and one violation stops the publish before the first upload.
//! The article on disk is left alone: shortening a quote is an edit to an
//! approved draft, and that is a decision, not a fix.
//!
//! The limits mirror `strapi-cms` as of the schema this was written against —
//! `video-post`, `content.quote`, `content.video`, `content.image`,
//! `content.embed`, `content.video-chapter`, `seo.keyword` and
//! `cta.magic-link`. A field this does not name is either `text` or `json`,
//! which Postgres does not bound. One limit is the house's rather than the
//! CMS's: [`LONG_DESCRIPTION`], on the draft only, because the page prints
//! that field under the heading.

use std::fmt;

use anyhow::{bail, Result};
use serde_json::Value;

use super::schema::{Article, Block};

/// The width of a Strapi `string` column in Postgres, declared or not.
pub const COLUMN: usize = 255;
/// `video-post.title` and `h1`: `maxLength: 200`, `minLength: 2`.
pub const TITLE: usize = 200;
/// `seo.keyword.term`.
pub const TERM: usize = 120;
/// `seo.keyword.notes`.
pub const NOTES: usize = 1000;
/// `video-post.canonicalUrl`.
pub const CANONICAL_URL: usize = 500;
/// Not a CMS limit — a house one. The field is `text` and Postgres would take a
/// page, but the site prints it under the heading as the standfirst, and a
/// 508-character paragraph there is the day this got a number. One sentence.
pub const LONG_DESCRIPTION: usize = 200;
pub const PRIORITIES: [&str; 2] = ["primary", "secondary"];
pub const INTENTS: [&str; 4] = [
    "informational",
    "commercial",
    "transactional",
    "navigational",
];
/// `content.embed.componentKey`'s closed enum. The component stage validates
/// against the landing repo's catalog; this is the CMS's copy of the same list.
pub const COMPONENT_KEYS: [&str; 4] = [
    "model_comparison",
    "model_pareto",
    "link_cards",
    "step_cards",
];
/// `video-post.keywordTargets` is `repeatable, max: 30`.
pub const TARGETS_MAX: usize = 30;
/// The JSON body Strapi's `strapi::body` middleware accepts, in bytes, with a
/// little headroom under koa-body's `1mb`. A long recording's word timings are
/// what gets a post there; the publish drops them before it sends.
pub const BODY_MAX: usize = 1_000_000;

/// One field the CMS would refuse, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// Where, in the reader's terms: `title`, `video.caption`,
    /// `block 5 (quote).quoteText` on the wire; `blocks[4] (quote).text` in
    /// the article, which is also how the model that wrote it named it.
    pub field: String,
    pub problem: String,
    /// The field in the draft this points at, when it is one a fix can be
    /// written to — every violation [`article`] finds, none of [`check`]'s.
    pub target: Option<Target>,
    /// The ceiling that was exceeded, for a length violation.
    pub limit: Option<usize>,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.field, self.problem)
    }
}

/// A field of the draft that the CMS bounds and the model writes.
///
/// What a violation points at and what a fix is applied to, whether the fix
/// came from the model — see [`super::repair`] — or from a box on the Blog tab.
/// Indices are into `blocks` and `keyword_targets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Title,
    H1,
    Slug,
    Description,
    LongDescription,
    Caption,
    CanonicalUrl,
    QuoteText(usize),
    QuoteHighlight(usize),
    Term(usize),
    Notes(usize),
}

impl Target {
    /// The form-field name the Blog tab posts the edit back under.
    pub fn key(self) -> String {
        match self {
            Target::Title => "title".into(),
            Target::H1 => "h1".into(),
            Target::Slug => "slug".into(),
            Target::Description => "description".into(),
            Target::LongDescription => "long_description".into(),
            Target::Caption => "caption".into(),
            Target::CanonicalUrl => "canonical_url".into(),
            Target::QuoteText(i) => format!("quote_text:{i}"),
            Target::QuoteHighlight(i) => format!("quote_highlight:{i}"),
            Target::Term(i) => format!("term:{i}"),
            Target::Notes(i) => format!("notes:{i}"),
        }
    }

    /// The inverse of [`Target::key`]. `None` for anything else, which is how
    /// a stray form field is ignored rather than written somewhere.
    pub fn parse(key: &str) -> Option<Target> {
        let (name, index) = match key.trim().split_once(':') {
            Some((name, index)) => (name, Some(index.parse::<usize>().ok()?)),
            None => (key.trim(), None),
        };
        Some(match (name, index) {
            ("title", None) => Target::Title,
            ("h1", None) => Target::H1,
            ("slug", None) => Target::Slug,
            ("description", None) => Target::Description,
            ("long_description", None) => Target::LongDescription,
            ("caption", None) => Target::Caption,
            ("canonical_url", None) => Target::CanonicalUrl,
            ("quote_text", Some(i)) => Target::QuoteText(i),
            ("quote_highlight", Some(i)) => Target::QuoteHighlight(i),
            ("term", Some(i)) => Target::Term(i),
            ("notes", Some(i)) => Target::Notes(i),
            _ => return None,
        })
    }

    /// What the Blog tab calls it, numbered the way the Body list is.
    pub fn label(self) -> String {
        match self {
            Target::Title => "Title".into(),
            Target::H1 => "Heading".into(),
            Target::Slug => "Slug".into(),
            Target::Description => "Description".into(),
            Target::LongDescription => "Long description".into(),
            Target::Caption => "Caption".into(),
            Target::CanonicalUrl => "Canonical URL".into(),
            Target::QuoteText(i) => format!("Block {} · quote", i + 1),
            Target::QuoteHighlight(i) => format!("Block {} · quote highlight", i + 1),
            Target::Term(i) => format!("Keyword target {} · term", i + 1),
            Target::Notes(i) => format!("Keyword target {} · notes", i + 1),
        }
    }
}

/// The current text at `target`, or `None` when there is no such field.
pub fn value_of(article: &Article, target: Target) -> Option<&str> {
    Some(match target {
        Target::Title => &article.title,
        Target::H1 => &article.h1,
        Target::Slug => &article.slug,
        Target::Description => &article.description,
        Target::LongDescription => &article.long_description,
        Target::Caption => &article.caption,
        Target::CanonicalUrl => &article.canonical_url,
        Target::QuoteText(i) => match article.blocks.get(i)? {
            Block::Quote { text, .. } => text,
            _ => return None,
        },
        Target::QuoteHighlight(i) => match article.blocks.get(i)? {
            Block::Quote { highlight, .. } => highlight,
            _ => return None,
        },
        Target::Term(i) => &article.keyword_targets.get(i)?.term,
        Target::Notes(i) => &article.keyword_targets.get(i)?.notes,
    })
}

/// Writes `text` at `target`, trimmed. `false` when there is no such field — a
/// block index past the end, or one that is not a quote — so a stale form
/// cannot write into the wrong block.
///
/// A quote's highlight has to stay a substring of its text or the page
/// highlights nothing, so a new text that no longer contains it clears it, and
/// a new highlight the text does not contain is dropped.
pub fn set(article: &mut Article, target: Target, text: &str) -> bool {
    let text = text.trim();
    match target {
        Target::Title => article.title = text.to_string(),
        Target::H1 => article.h1 = text.to_string(),
        Target::Slug => article.slug = text.to_string(),
        Target::Description => article.description = text.to_string(),
        Target::LongDescription => article.long_description = text.to_string(),
        Target::Caption => article.caption = text.to_string(),
        Target::CanonicalUrl => article.canonical_url = text.to_string(),
        Target::QuoteText(i) => match article.blocks.get_mut(i) {
            Some(Block::Quote {
                text: quote,
                highlight,
            }) => {
                *quote = text.to_string();
                if !quote.contains(highlight.as_str()) {
                    highlight.clear();
                }
            }
            _ => return false,
        },
        Target::QuoteHighlight(i) => match article.blocks.get_mut(i) {
            Some(Block::Quote {
                text: quote,
                highlight,
            }) => {
                *highlight = match quote.contains(text) {
                    true => text.to_string(),
                    false => String::new(),
                };
            }
            _ => return false,
        },
        Target::Term(i) => match article.keyword_targets.get_mut(i) {
            Some(target) => target.term = text.to_string(),
            None => return false,
        },
        Target::Notes(i) => match article.keyword_targets.get_mut(i) {
            Some(target) => target.notes = text.to_string(),
            None => return false,
        },
    }
    true
}

/// Every violation in the `data` object of a create body, in field order.
pub fn check(data: &Value) -> Vec<Violation> {
    let mut c = Checker::default();
    let text = |object: &Value, field: &str| -> Option<String> {
        object
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    c.required("", "title", text(data, "title").as_deref());
    c.at_least("", "title", text(data, "title").as_deref(), 2);
    c.at_most("", "title", text(data, "title").as_deref(), TITLE);
    c.required("", "slug", text(data, "slug").as_deref());
    c.at_most("", "slug", text(data, "slug").as_deref(), COLUMN);
    c.at_least("", "h1", text(data, "h1").as_deref(), 2);
    c.at_most("", "h1", text(data, "h1").as_deref(), TITLE);
    c.required(
        "",
        "shortAndMetaDescription",
        text(data, "shortAndMetaDescription").as_deref(),
    );
    c.at_most(
        "",
        "canonicalUrl",
        text(data, "canonicalUrl").as_deref(),
        CANONICAL_URL,
    );

    for field in ["video", "videoVertical"] {
        if let Some(video) = data.get(field).filter(|value| value.is_object()) {
            c.required(field, "url", text(video, "url").as_deref());
            c.at_most(field, "caption", text(video, "caption").as_deref(), COLUMN);
            c.at_most(
                field,
                "externalId",
                text(video, "externalId").as_deref(),
                64,
            );
        }
    }

    for (index, block) in rows(data, "content").enumerate() {
        let kind = block["__component"].as_str().unwrap_or("");
        let at = format!(
            "block {} ({})",
            index + 1,
            kind.strip_prefix("content.").unwrap_or(kind)
        );
        match kind {
            "content.quote" => {
                c.required(&at, "quoteText", text(block, "quoteText").as_deref());
                c.at_most(
                    &at,
                    "quoteText",
                    text(block, "quoteText").as_deref(),
                    COLUMN,
                );
                c.at_most(
                    &at,
                    "quoteTextHighlighted",
                    text(block, "quoteTextHighlighted").as_deref(),
                    COLUMN,
                );
            }
            "content.image" => {
                c.required(&at, "src", text(block, "src").as_deref());
                c.at_most(&at, "alt", text(block, "alt").as_deref(), 200);
                c.at_most(&at, "caption", text(block, "caption").as_deref(), COLUMN);
            }
            "content.embed" => {
                c.required(&at, "componentKey", text(block, "componentKey").as_deref());
                c.one_of(
                    &at,
                    "componentKey",
                    text(block, "componentKey").as_deref(),
                    &COMPONENT_KEYS,
                );
                c.at_most(&at, "heading", text(block, "heading").as_deref(), 200);
            }
            "content.table" => {
                c.required(&at, "tableHeaders", text(block, "tableHeaders").as_deref());
                c.required(&at, "tableContent", text(block, "tableContent").as_deref());
            }
            "content.text" => {}
            other => c.found.push(Violation {
                field: at,
                problem: format!("names {other:?}, which is not a component the zone accepts"),
                target: None,
                limit: None,
            }),
        }
    }

    for (index, chapter) in rows(data, "videoChapters").enumerate() {
        let at = format!("chapter {}", index + 1);
        c.required(&at, "name", text(chapter, "name").as_deref());
        c.at_most(&at, "name", text(chapter, "name").as_deref(), 200);
    }

    let targets = rows(data, "keywordTargets").count();
    if targets > TARGETS_MAX {
        c.found.push(Violation {
            field: "keywordTargets".into(),
            problem: format!("has {targets} entries and the CMS holds {TARGETS_MAX}"),
            target: None,
            limit: None,
        });
    }
    for (index, target) in rows(data, "keywordTargets").enumerate() {
        let at = format!("keyword target {}", index + 1);
        c.required(&at, "term", text(target, "term").as_deref());
        c.at_most(&at, "term", text(target, "term").as_deref(), TERM);
        c.one_of(
            &at,
            "priority",
            text(target, "priority").as_deref(),
            &PRIORITIES,
        );
        c.one_of(&at, "intent", text(target, "intent").as_deref(), &INTENTS);
        c.at_most(&at, "notes", text(target, "notes").as_deref(), NOTES);
    }

    if let Some(cta) = data.get("magicLinkCta").filter(|value| value.is_object()) {
        c.required("magicLinkCta", "url", text(cta, "url").as_deref());
        c.at_most(
            "magicLinkCta",
            "label",
            text(cta, "label").as_deref(),
            COLUMN,
        );
        c.at_most(
            "magicLinkCta",
            "description",
            text(cta, "description").as_deref(),
            280,
        );
    }

    // Postgres refuses U+0000 in `text` and `jsonb` alike, and Strapi passes
    // it straight through: another bare 500, from anywhere in the body.
    nul_in(data, "data", &mut c.found);

    c.found
}

/// The same limits, on the draft rather than the wire — everything in an
/// [`Article`] that the model writes and the CMS bounds.
///
/// Each violation names its [`Target`], so a fix can be written straight back,
/// and its field the way the model's own JSON names it (`blocks[4]
/// (quote).text`, `keyword_targets[1].term`), because the list goes to the
/// model as the thing to fix — see [`super::repair`]. The fields the article
/// does not carry — media, chapters, the component blocks — are measured on
/// the wire by [`check`] instead.
pub fn article(article: &Article) -> Vec<Violation> {
    let mut c = Checker::default();
    fn present(value: &str) -> Option<&str> {
        (!value.is_empty()).then_some(value)
    }

    c.at(Target::Title);
    c.required("", "title", Some(&article.title));
    c.at_least("", "title", Some(&article.title), 2);
    c.at_most("", "title", Some(&article.title), TITLE);
    c.at(Target::Slug);
    c.required("", "slug", Some(&article.slug));
    c.at_most("", "slug", Some(&article.slug), COLUMN);
    c.at(Target::H1);
    c.at_least("", "h1", present(&article.h1), 2);
    c.at_most("", "h1", present(&article.h1), TITLE);
    c.at(Target::Description);
    c.required("", "description", Some(&article.description));
    c.at(Target::LongDescription);
    c.at_most(
        "",
        "long_description",
        present(&article.long_description),
        LONG_DESCRIPTION,
    );
    c.at(Target::Caption);
    c.at_most("", "caption", Some(&article.caption), COLUMN);
    c.at(Target::CanonicalUrl);
    c.at_most(
        "",
        "canonical_url",
        present(&article.canonical_url),
        CANONICAL_URL,
    );

    for (index, block) in article.blocks.iter().enumerate() {
        if let Block::Quote { text, highlight } = block {
            let at = format!("blocks[{index}] (quote)");
            c.at(Target::QuoteText(index));
            c.required(&at, "text", Some(text));
            c.at_most(&at, "text", Some(text), COLUMN);
            c.at(Target::QuoteHighlight(index));
            c.at_most(&at, "highlight", present(highlight), COLUMN);
        }
    }

    for (index, target) in article.keyword_targets.iter().enumerate() {
        let at = format!("keyword_targets[{index}]");
        c.at(Target::Term(index));
        c.required(&at, "term", Some(&target.term));
        c.at_most(&at, "term", Some(&target.term), TERM);
        c.current = None;
        c.one_of(&at, "priority", Some(&target.priority), &PRIORITIES);
        c.one_of(&at, "intent", present(&target.intent), &INTENTS);
        c.at(Target::Notes(index));
        c.at_most(&at, "notes", present(&target.notes), NOTES);
    }

    c.found
}

/// The create body, or the reason the CMS would refuse it.
///
/// Takes the whole `{ "data": … }` body — what [`super::payload`] builds and
/// what goes on the wire — so what is measured is exactly what would be sent.
pub fn ensure(body: &Value) -> Result<()> {
    let found = check(&body["data"]);
    if found.is_empty() {
        return Ok(());
    }
    bail!(
        "the CMS would refuse this post — {}. The Blog tab lists the fields: edit them there, \
         or press Shorten with the model",
        listed(&found)
    );
}

/// The violations as one line: `a; b; c`.
pub fn listed(found: &[Violation]) -> String {
    found
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Whether `text` fits a field of `max`, counted the way the column counts.
pub fn fits(text: &str, max: usize) -> bool {
    width(text) <= max
}

/// Removes U+0000 from every string under `value`. A NUL carries no meaning in
/// prose or a transcript, and Postgres refuses the whole row over one.
pub fn scrub(value: &mut Value) {
    match value {
        Value::String(text) if text.contains('\0') => text.retain(|ch| ch != '\0'),
        Value::Array(items) => items.iter_mut().for_each(scrub),
        Value::Object(fields) => fields.values_mut().for_each(scrub),
        _ => {}
    }
}

/// The body as Strapi would receive it, in bytes.
pub fn bytes(body: &Value) -> usize {
    serde_json::to_vec(body)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

/// Every string under `value` that carries a NUL, by path.
fn nul_in(value: &Value, path: &str, found: &mut Vec<Violation>) {
    match value {
        Value::String(text) if text.contains('\0') => found.push(Violation {
            field: path.to_string(),
            problem: "contains a NUL character (U+0000), which Postgres refuses".into(),
            target: None,
            limit: None,
        }),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                nul_in(item, &format!("{path}[{index}]"), found);
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields {
                nul_in(item, &format!("{path}.{key}"), found);
            }
        }
        _ => {}
    }
}

fn rows<'a>(data: &'a Value, field: &str) -> impl Iterator<Item = &'a Value> {
    data[field].as_array().into_iter().flatten()
}

/// What Strapi and Postgres count: UTF-16 units, which is never fewer than
/// characters, so a value that passes here passes there.
fn width(text: &str) -> usize {
    text.encode_utf16().count()
}

#[derive(Default)]
struct Checker {
    found: Vec<Violation>,
    /// The draft field the next violations point at. `None` on the wire.
    current: Option<Target>,
}

impl Checker {
    fn at(&mut self, target: Target) {
        self.current = Some(target);
    }

    fn push(&mut self, at: &str, field: &str, problem: String, limit: Option<usize>) {
        let field = match at.is_empty() {
            true => field.to_string(),
            false => format!("{at}.{field}"),
        };
        self.found.push(Violation {
            field,
            problem,
            target: self.current,
            limit,
        });
    }

    /// Present and not blank. Strapi's `required` is exactly that: an empty
    /// string is refused the same as a missing one.
    fn required(&mut self, at: &str, field: &str, value: Option<&str>) {
        if value.is_none_or(|text| text.trim().is_empty()) {
            self.push(at, field, "is required and empty".to_string(), None);
        }
    }

    fn at_most(&mut self, at: &str, field: &str, value: Option<&str>, max: usize) {
        if let Some(text) = value {
            let len = width(text);
            if len > max {
                self.push(
                    at,
                    field,
                    format!("is {len} characters and the CMS holds {max}"),
                    Some(max),
                );
            }
        }
    }

    /// A floor that applies only once something is there: an omitted optional
    /// field is not a short one.
    fn at_least(&mut self, at: &str, field: &str, value: Option<&str>, min: usize) {
        if let Some(text) = value {
            let len = width(text);
            if len > 0 && len < min {
                self.push(
                    at,
                    field,
                    format!("is {len} character(s) and the CMS wants at least {min}"),
                    None,
                );
            }
        }
    }

    fn one_of(&mut self, at: &str, field: &str, value: Option<&str>, allowed: &[&str]) {
        if let Some(text) = value {
            if !allowed.contains(&text) {
                self.push(
                    at,
                    field,
                    format!("is {text:?}; the CMS accepts one of {}", allowed.join(", ")),
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({ "data": {
            "title": "SaaS Go-To-Market Strategy",
            "slug": "saas-go-to-market-strategy",
            "shortAndMetaDescription": "How the whole system works.",
            "video": { "url": "https://www.youtube.com/watch?v=nFWK8eJjIhc", "caption": "The walkthrough.",
                       "provider": "youtube", "externalId": "nFWK8eJjIhc" },
            "content": [
                { "__component": "content.text", "textBodyHtml": "<p>Body.</p>" },
                { "__component": "content.quote", "quoteText": "A line worth remembering." }
            ],
            "videoChapters": [{ "name": "Chapter 1", "startOffset": 3, "endOffset": 9 }],
            "keywordTargets": [{ "term": "go-to-market strategy", "priority": "primary", "intent": "informational" }],
        }})
    }

    /// The body that failed three times on the live CMS: a pull quote past the
    /// column, and nothing else wrong with it. The message has to name the
    /// block, because "quoteText" alone does not say which of two quotes.
    #[test]
    fn a_quote_past_the_column_is_named_by_block() {
        let mut body = body();
        body["data"]["content"][1]["quoteText"] = json!("x".repeat(326));
        let found = check(&body["data"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].field, "block 2 (quote).quoteText");
        assert!(found[0].problem.contains("326"), "{}", found[0].problem);
        assert!(found[0].problem.contains("255"), "{}", found[0].problem);

        let err = ensure(&body).unwrap_err().to_string();
        assert!(err.contains("block 2 (quote).quoteText"), "{err}");
        assert!(err.contains("The Blog tab lists the fields"), "{err}");
    }

    /// Exactly the column width is fine — the check is on what Postgres
    /// refuses, not one short of it.
    #[test]
    fn a_body_within_every_limit_passes() {
        let mut body = body();
        body["data"]["content"][1]["quoteText"] = json!("y".repeat(COLUMN));
        assert!(ensure(&body).is_ok());
    }

    /// Strapi's declared limits, not just the column: a title the schema caps
    /// at 200 is refused at 201 even though the column would take it.
    #[test]
    fn declared_maximums_are_enforced_too() {
        let mut body = body();
        body["data"]["title"] = json!("t".repeat(201));
        body["data"]["video"]["externalId"] = json!("i".repeat(65));
        body["data"]["videoChapters"][0]["name"] = json!("n".repeat(201));
        let fields: Vec<_> = check(&body["data"]).into_iter().map(|v| v.field).collect();
        assert_eq!(
            fields,
            ["title", "video.externalId", "chapter 1.name"],
            "{fields:?}"
        );
    }

    /// A closed enum: the CMS refuses the whole entry for one value it does
    /// not know, and says "invalid" without naming which of thirty targets.
    #[test]
    fn an_enum_value_outside_the_schema_is_refused() {
        let mut body = body();
        body["data"]["keywordTargets"][0]["priority"] = json!("high");
        let found = check(&body["data"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].field, "keyword target 1.priority");
        assert!(
            found[0].problem.contains("primary, secondary"),
            "{}",
            found[0].problem
        );
    }

    /// Required means non-blank. A blank title is the one thing Strapi refuses
    /// that is also cheap to say before the thumbnail is up.
    #[test]
    fn a_blank_required_field_is_refused() {
        let mut body = body();
        body["data"]["title"] = json!("   ");
        body["data"]["content"][1]["quoteText"] = json!("");
        let fields: Vec<_> = check(&body["data"]).into_iter().map(|v| v.field).collect();
        assert_eq!(fields, ["title", "block 2 (quote).quoteText"], "{fields:?}");
    }

    /// A component the zone does not list is refused by the CMS as a whole.
    /// Here, so the block is named rather than the request.
    #[test]
    fn an_unknown_component_is_refused() {
        let mut body = body();
        body["data"]["content"][0]["__component"] = json!("content.video");
        let found = check(&body["data"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].field, "block 1 (video)");
    }

    /// Absent optionals are not violations: a body with no `h1`, no vertical
    /// cut, no CTA and no targets is the common one.
    #[test]
    fn omitted_optional_fields_are_not_measured() {
        let body = json!({ "data": {
            "title": "Short", "slug": "short", "shortAndMetaDescription": "d",
            "video": { "url": "https://x" }, "content": [],
        }});
        assert!(ensure(&body).is_ok());
    }

    /// Counted the way the column counts, so a string of multi-byte characters
    /// is not passed here and refused there.
    #[test]
    fn width_counts_characters_not_bytes() {
        assert_eq!(width("é".repeat(255).as_str()), 255);
        assert_eq!(width("日本語"), 3);
    }

    /// The draft side of the same check: the model's own field names, so the
    /// list can go straight back to it.
    #[test]
    fn a_draft_is_measured_in_the_models_terms() {
        use crate::blog::schema::{Article, Block, KeywordTarget};
        let draft = Article {
            title: "A title".into(),
            slug: "a-title".into(),
            description: "A promise.".into(),
            blocks: vec![
                Block::Text {
                    html: "<p>x</p>".into(),
                },
                Block::Quote {
                    text: "q".repeat(326),
                    highlight: String::new(),
                },
            ],
            keyword_targets: vec![KeywordTarget {
                term: "t".repeat(121),
                priority: "primary".into(),
                intent: String::new(),
                notes: String::new(),
            }],
            ..Article::default()
        };
        let fields: Vec<_> = article(&draft).into_iter().map(|v| v.field).collect();
        assert_eq!(
            fields,
            ["blocks[1] (quote).text", "keyword_targets[0].term"],
            "{fields:?}"
        );

        let mut fine = draft.clone();
        fine.blocks[1] = Block::Quote {
            text: "Short.".into(),
            highlight: "Short".into(),
        };
        fine.keyword_targets[0].term = "short term".into();
        assert!(article(&fine).is_empty(), "{:?}", article(&fine));
    }

    #[test]
    fn fits_is_the_same_count_the_column_uses() {
        assert!(fits(&"é".repeat(COLUMN), COLUMN));
        assert!(!fits(&"é".repeat(COLUMN + 1), COLUMN));
    }

    /// The keys the Blog tab posts edits back under have to survive the trip.
    #[test]
    fn a_target_key_round_trips_and_a_stray_one_is_ignored() {
        for target in [
            Target::Title,
            Target::H1,
            Target::QuoteText(4),
            Target::QuoteHighlight(4),
            Target::Term(1),
            Target::Notes(2),
            Target::Caption,
            Target::LongDescription,
        ] {
            assert_eq!(Target::parse(&target.key()), Some(target), "{target:?}");
        }
        assert_eq!(Target::parse("quote_text:x"), None);
        assert_eq!(Target::parse("password"), None);
        assert_eq!(Target::QuoteText(4).label(), "Block 5 · quote");
    }

    /// A violation the draft check finds says which field, so the fix can be
    /// written straight back; one the wire check finds does not, because the
    /// wire is built from more than the draft.
    #[test]
    fn draft_violations_point_at_their_field_and_carry_the_limit() {
        use crate::blog::schema::{Article, Block};
        let draft = Article {
            title: "t".repeat(201),
            slug: "s".into(),
            description: "d".into(),
            blocks: vec![Block::Quote {
                text: "q".repeat(300),
                highlight: String::new(),
            }],
            ..Article::default()
        };
        let found = article(&draft);
        assert_eq!(found[0].target, Some(Target::Title));
        assert_eq!(found[0].limit, Some(TITLE));
        assert_eq!(found[1].target, Some(Target::QuoteText(0)));
        assert_eq!(found[1].limit, Some(COLUMN));
        assert!(check(&body()["data"]).iter().all(|v| v.target.is_none()));
    }

    /// Writing a shorter quote keeps the highlight only while it is still in
    /// the text; a stale index writes nothing rather than into another block.
    #[test]
    fn setting_a_quote_keeps_its_highlight_only_while_it_still_fits() {
        use crate::blog::schema::{Article, Block};
        let mut draft = Article {
            blocks: vec![
                Block::Text {
                    html: "<p>x</p>".into(),
                },
                Block::Quote {
                    text: "We could not do it.".into(),
                    highlight: "could not".into(),
                },
            ],
            ..Article::default()
        };
        assert!(set(&mut draft, Target::QuoteText(1), "  We could not.  "));
        assert!(matches!(
            &draft.blocks[1],
            Block::Quote { text, highlight } if text == "We could not." && highlight == "could not"
        ));
        assert!(set(&mut draft, Target::QuoteText(1), "Something else."));
        assert!(matches!(&draft.blocks[1], Block::Quote { highlight, .. } if highlight.is_empty()));
        assert!(!set(&mut draft, Target::QuoteText(0), "not a quote"));
        assert!(!set(&mut draft, Target::QuoteText(9), "no such block"));
        assert_eq!(
            value_of(&draft, Target::QuoteText(1)),
            Some("Something else.")
        );
        assert_eq!(value_of(&draft, Target::QuoteText(0)), None);
    }

    /// Postgres refuses a NUL anywhere; the check names where, and the scrub
    /// removes it before the body is built.
    #[test]
    fn a_nul_anywhere_is_named_and_scrubbed() {
        let mut body = body();
        body["data"]["content"][0]["textBodyHtml"] = json!("<p>a\u{0}b</p>");
        let found = check(&body["data"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].field, "data.content[0].textBodyHtml");
        scrub(&mut body);
        assert_eq!(body["data"]["content"][0]["textBodyHtml"], "<p>ab</p>");
        assert!(check(&body["data"]).is_empty());
    }

    #[test]
    fn a_component_key_outside_the_catalog_is_refused() {
        let mut body = body();
        body["data"]["content"].as_array_mut().unwrap().push(json!({
            "__component": "content.embed", "componentKey": "carousel", "componentVersion": 1
        }));
        let found = check(&body["data"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].field, "block 3 (embed).componentKey");
    }

    /// The standfirst that prompted the house limit: a 508-character paragraph
    /// under the heading. Flagged on the draft with its field, so the card and
    /// the repair both reach it; not on the wire, where the CMS takes it.
    #[test]
    fn a_long_description_past_the_house_limit_is_flagged_on_the_draft_only() {
        use crate::blog::schema::Article;
        let draft = Article {
            title: "Title".into(),
            slug: "title".into(),
            description: "d".into(),
            long_description: "w".repeat(508),
            ..Article::default()
        };
        let found = article(&draft);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].target, Some(Target::LongDescription));
        assert_eq!(found[0].limit, Some(LONG_DESCRIPTION));
        assert_eq!(found[0].field, "long_description");

        let mut body = body();
        body["data"]["description"] = json!("w".repeat(508));
        assert!(check(&body["data"]).is_empty());
    }
}
