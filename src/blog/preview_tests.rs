//! What the preview shows, and — the part that matters — what it shows when
//! something is wrong.

use super::*;
use crate::blog::schema::{Article, Block};
use crate::figure::{Figure, Snipped};
use std::path::PathBuf;

fn figure(n: u32, caption: &str, alt: &str) -> Figure {
    Figure {
        n,
        file: PathBuf::from(format!("/tmp/a project/figures/figure-{n:02}.jpg")),
        at: "2026-09-04T10:22:31.000000Z".into(),
        chapter: Some(3),
        offset: Some(84.0),
        rect: Snipped { x: 0.0, y: 0.0, w: 640.0, h: 480.0 },
        width: 1280,
        height: 960,
        audio: None,
        transcribing: false,
        said: String::new(),
        caption: caption.into(),
        alt: alt.into(),
    }
}

fn article(blocks: Vec<Block>) -> Article {
    Article {
        title: "Why watermarking fails".into(),
        slug: "why-watermarking-fails".into(),
        description: "A complete promise of the argument.".into(),
        keywords: vec!["ai".into(), "watermarking".into()],
        caption: "Four minutes on enforcement economics".into(),
        blocks,
        ..Article::default()
    }
}

#[test]
fn the_page_carries_the_title_slug_and_body() {
    let html = page(
        &article(vec![Block::Text {
            html: "<h2>Where it broke</h2><p>Body.</p>".into(),
        }]),
        &[],
    );
    assert!(html.contains("<h1>Why watermarking fails</h1>"));
    assert!(html.contains("/blog/why-watermarking-fails"));
    assert!(html.contains("<h2>Where it broke</h2>"), "text blocks render as HTML");
    assert!(html.contains("ai, watermarking"));
    assert!(html.contains("Local preview"), "it says what it is not");
}

/// The heading and the FAQ are read here or nowhere: the pane shows that the
/// fields are populated, and only the page shows whether the questions are ones
/// anyone would ask.
#[test]
fn the_heading_and_the_faq_are_shown_on_the_page() {
    let mut draft = article(vec![Block::Text { html: "<p>Body.</p>".into() }]);
    draft.h1 = "Why watermarking fails, and what enforcement costs".into();
    draft.faq = vec![crate::blog::schema::Faq {
        question: "Does it scale?".into(),
        answer: "Not past the first retry.".into(),
    }];
    let html = page(&draft, &[]);
    assert!(html.contains("<h1>Why watermarking fails, and what enforcement costs</h1>"));
    // Both strings, because once they differ the difference is the thing to
    // check — a heading is not worth having if it says what the title said.
    assert!(html.contains("title: Why watermarking fails"));
    assert!(html.contains("<dt>Does it scale?</dt>"));
    assert!(html.contains("<dd>Not past the first retry.</dd>"));
}

/// The common case, and it must not leave furniture behind: no second title
/// line, no empty FAQ heading under the article.
#[test]
fn an_article_with_no_heading_and_no_faq_renders_neither() {
    let html = page(&article(vec![Block::Text { html: "<p>Body.</p>".into() }]), &[]);
    assert!(html.contains("<h1>Why watermarking fails</h1>"));
    assert!(!html.contains("title: "), "no second title line");
    assert!(!html.contains("class=\"faq\""), "no empty accordion");
}

/// Both search controls are invisible on the live page, and both are the kind
/// of mistake nobody notices — a post that quietly never gets indexed, or one
/// that hands its ranking away. So the preview says so loudly.
#[test]
fn the_search_controls_are_called_out_on_the_page() {
    let mut draft = article(vec![Block::Text { html: "<p>Body.</p>".into() }]);
    draft.no_index = true;
    draft.canonical_url = "https://example.com/original".into();
    let html = page(&draft, &[]);
    assert!(html.contains("noindex"));
    assert!(html.contains("out of the sitemap"));
    assert!(html.contains("https://example.com/original"));
    assert!(html.contains("class=\"controls\""));
}

/// The default, and it must leave no furniture: a banner drawn on every normal
/// article is a banner nobody reads on the one that needs it.
#[test]
fn an_article_with_no_search_controls_draws_no_banner() {
    let html = page(&article(vec![Block::Text { html: "<p>Body.</p>".into() }]), &[]);
    assert!(!html.contains("class=\"controls\""));
    assert!(!html.contains("noindex"));
}

/// A figure renders as a real figure/figcaption, pointing at the file on disk —
/// which is what makes the preview work before any upload.
#[test]
fn a_placed_figure_points_at_the_local_file() {
    let html = page(
        &article(vec![Block::Figure { n: 2 }]),
        &[figure(2, "The retry storm.", "A log of 429s")],
    );
    assert!(html.contains("<figure>"));
    assert!(
        html.contains(r#"src="file:///tmp/a%20project/figures/figure-02.jpg""#),
        "the space in the project name was not encoded: {html}"
    );
    assert!(html.contains(r#"alt="A log of 429s""#));
    assert!(html.contains("<figcaption>The retry storm.</figcaption>"));
    assert!(html.contains("1 figure(s)"));
}

/// The one case the preview handles differently from the publish: it draws the
/// problem instead of refusing. Finding it here is the entire point.
#[test]
fn a_figure_that_was_never_captured_is_shown_as_a_gap() {
    let html = page(&article(vec![Block::Figure { n: 7 }]), &[]);
    assert!(html.contains("figure 07 is placed here but was never captured"), "{html}");
    assert!(!html.contains("<img"), "no broken image is drawn");
}

/// A figure placed before Write Blurbs has run. An empty caption bar reads as a
/// styling bug, so it says what is actually missing.
#[test]
fn a_figure_with_no_blurb_says_so() {
    let html = page(&article(vec![Block::Figure { n: 1 }]), &[figure(1, "", "")]);
    assert!(html.contains("figure 01 — no blurb yet"), "{html}");
}

/// With no alt text the caption stands in — an empty alt on a content image is
/// an accessibility failure that ships silently.
#[test]
fn the_caption_stands_in_for_missing_alt_text() {
    let html = page(
        &article(vec![Block::Figure { n: 1 }]),
        &[figure(1, "The retry storm.", "  ")],
    );
    assert!(html.contains(r#"alt="The retry storm.""#), "{html}");
}

#[test]
fn a_quote_highlights_in_brand_orange() {
    let html = page(
        &article(vec![Block::Quote {
            text: "It could not have worked.".into(),
            highlight: "could not".into(),
        }]),
        &[],
    );
    assert!(html.contains("<blockquote>It <mark>could not</mark> have worked.</blockquote>"), "{html}");
}

/// A hand-edited article.json can carry a highlight that is not in the quote.
/// The reader renders nothing in that case; so does this, rather than guessing.
#[test]
fn a_highlight_that_is_not_in_the_quote_renders_plain() {
    let html = page(
        &article(vec![Block::Quote {
            text: "It could not.".into(),
            highlight: "never said".into(),
        }]),
        &[],
    );
    assert!(html.contains("<blockquote>It could not.</blockquote>"), "{html}");
    assert!(!html.contains("<mark>"));
}

#[test]
fn a_table_renders_as_a_table() {
    let html = page(
        &article(vec![Block::Table {
            headers: vec!["Option".into(), "Cost".into()],
            rows: vec![vec!["Watermark".into(), "High".into()]],
        }]),
        &[],
    );
    assert!(html.contains("<th>Option</th><th>Cost</th>"), "{html}");
    assert!(html.contains("<td>Watermark</td><td>High</td>"), "{html}");
}

/// Plain-text fields are escaped even though text blocks are not. A title with
/// an ampersand is ordinary; a title that closes the tag it is inside is a
/// broken page.
#[test]
fn plain_text_fields_are_escaped() {
    let mut draft = article(vec![]);
    draft.title = "Trust & <safety>".into();
    let html = page(&draft, &[]);
    assert!(html.contains("<h1>Trust &amp; &lt;safety&gt;</h1>"), "{html}");
}

/// A caption is a plain-text field too, and it arrives from a model.
#[test]
fn a_caption_is_escaped() {
    let html = page(
        &article(vec![Block::Figure { n: 1 }]),
        &[figure(1, "Ampersands & <angles>", "alt")],
    );
    assert!(html.contains("<figcaption>Ampersands &amp; &lt;angles&gt;</figcaption>"), "{html}");
}

#[test]
fn the_preview_lands_beside_the_article() {
    let dir = std::env::temp_dir().join(format!(
        "stream-recorder-preview-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = write(&dir, &article(vec![]), &[]).unwrap();
    assert_eq!(path.file_name().unwrap(), PREVIEW_HTML);
    assert!(std::fs::read_to_string(&path).unwrap().contains("<h1>"));
    let _ = std::fs::remove_dir_all(&dir);
}
