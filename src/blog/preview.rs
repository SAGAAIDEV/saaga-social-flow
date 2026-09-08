//! The article as a page, on this machine, before anything is uploaded.
//!
//! Reading `article.json` in the pane tells you the fields are populated. It
//! does not tell you whether the piece reads well, whether a figure landed
//! beside the section it belongs to, or whether a caption says the same thing as
//! the paragraph above it. Those are the questions worth answering *before* a
//! permanent public URL exists, and they can only be answered by looking at the
//! thing.
//!
//! ## What it is faithful about, and what it is not
//!
//! Faithful: the words, the order of the blocks, the heading structure that
//! becomes the table of contents, which figure sits where, and every caption.
//! That is the whole set of decisions this stage actually makes.
//!
//! Not faithful: it is not the live template. No video embed, no site
//! typography, no navigation, and the column is a plain measure rather than the
//! real grid. Chasing those would mean keeping a copy of the site's CSS in this
//! crate and having it drift — and a preview that is *nearly* the site is worse
//! than one that is obviously not, because the difference is where you stop
//! trusting it.
//!
//! ## Figures come from disk
//!
//! `file://` paths straight to `{project}/figures/`, so a preview works with no
//! network and before any upload. It is also the one place a figure problem is
//! *shown* rather than refused: a block referencing a figure that was never
//! captured draws a marked gap here, where [`super::upload_figures`] stops the
//! publish outright.
//!
//! ## The HTML is trusted
//!
//! A text block is CKEditor HTML written by a model, and it is inserted here
//! verbatim rather than escaped — escaping it would show markup instead of a
//! page, which defeats the point. That is the same HTML the CMS is about to
//! publish, so this file is not where that trust boundary is decided. Plain-text
//! fields — the title, the description, every caption — are escaped.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::schema::{Article, Block};
use crate::figure::Figure;

pub const PREVIEW_HTML: &str = "preview.html";

/// SAAGA's palette, from `saaga-landing/src/app/globals.css`. Copied rather
/// than imported for the reason above: this page is deliberately not the site.
const ORANGE: &str = "#EB5201";
const BLACK: &str = "#1D1D1D";
const GREY: &str = "#626262";
const LIGHT: &str = "#D1D1CC";
const MUSTARD: &str = "#FFF9F6";

/// Writes the preview beside `article.json` and returns its path.
pub fn write(dir: &Path, article: &Article, figures: &[Figure]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(PREVIEW_HTML);
    std::fs::write(&path, page(article, figures))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The whole page, as a string. Pure, so the structure can be asserted.
pub fn page(article: &Article, figures: &[Figure]) -> String {
    let body: String = article
        .blocks
        .iter()
        .map(|block| render(block, figures))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — preview</title>
<style>
  :root {{ color-scheme: light; }}
  body {{
    margin: 0; background: #fff; color: {BLACK};
    font: 17px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  }}
  .note {{
    background: {MUSTARD}; border-bottom: 1px solid {LIGHT};
    padding: 10px 20px; font-size: 13px; color: {GREY};
  }}
  .note strong {{ color: {BLACK}; }}
  main {{ max-width: 752px; margin: 0 auto; padding: 40px 20px 96px; }}
  h1 {{ font-size: 40px; line-height: 1.15; letter-spacing: -0.02em; margin: 0 0 12px; }}
  .desc {{ font-size: 19px; color: {GREY}; margin: 0 0 8px; }}
  .meta {{ font-size: 13px; color: {GREY}; border-bottom: 1px solid {LIGHT};
           padding-bottom: 24px; margin-bottom: 32px; }}
  h2 {{ font-size: 27px; line-height: 1.25; margin: 40px 0 12px; }}
  h3 {{ font-size: 21px; margin: 28px 0 8px; }}
  p, li {{ margin: 0 0 16px; }}
  a {{ color: {ORANGE}; }}
  blockquote {{
    margin: 32px 0; padding: 0 0 0 20px; border-left: 3px solid {ORANGE};
    font-size: 22px; line-height: 1.4;
  }}
  blockquote mark {{ background: none; color: {ORANGE}; }}
  table {{ border-collapse: collapse; width: 100%; margin: 28px 0; font-size: 15px; }}
  th, td {{ border: 1px solid {LIGHT}; padding: 8px 10px; text-align: left; }}
  th {{ background: {MUSTARD}; font-weight: 600; }}
  figure {{ margin: 32px 0; }}
  figure img {{ width: 100%; height: auto; border-radius: 8px; border: 1px solid {LIGHT}; }}
  figcaption {{ font-size: 14px; color: {GREY}; margin-top: 8px; }}
  .missing {{
    margin: 32px 0; padding: 16px 20px; border: 1px dashed {ORANGE};
    border-radius: 8px; color: {ORANGE}; font-size: 14px;
  }}
  .faq {{ margin: 48px 0 0; border-top: 1px solid {LIGHT}; padding-top: 8px; }}
  .faq h2 {{ font-size: 20px; }}
  .faq dt {{ font-weight: 600; margin-top: 20px; }}
  .faq dd {{ margin: 6px 0 0; color: {GREY}; }}
  .titled {{ font-size: 14px; color: {GREY}; margin: 0 0 4px; }}
  .controls {{
    margin: 20px 0 0; padding: 12px 16px; border-radius: 8px;
    background: {ORANGE}; color: #fff; font-size: 14px; line-height: 1.5;
  }}
  .embed {{
    margin: 32px 0; padding: 20px; border: 1px dashed {LIGHT}; border-radius: 8px;
    color: {GREY}; font-size: 14px; text-align: center;
  }}
</style>
<div class="note">
  <strong>Local preview.</strong> The words, the block order and the figures are
  what would publish. The video embed, the site typography and the page
  furniture are not shown — this is not the live template.
</div>
<main>
{titled}  <h1>{heading}</h1>
  <p class="desc">{description}</p>
  <div class="meta">/blog/{slug} · {summary}{keywords}</div>
{controls}
{body}
{faq}</main>
"#,
        // The search result and the page heading are two different strings once
        // `h1` is set, and the difference is the only thing worth checking about
        // it — so both are shown, and only when they really differ.
        titled = match article.h1.is_empty() {
            true => String::new(),
            false => format!(
                "  <p class=\"titled\">title: {}</p>\n",
                escape(&article.title)
            ),
        },
        heading = escape(match article.h1.is_empty() {
            true => &article.title,
            false => &article.h1,
        }),
        faq = faq(&article.faq),
        controls = controls(article),
        title = escape(&article.title),
        description = escape(&article.description),
        slug = escape(&article.slug),
        summary = escape(&summary(article)),
        keywords = match article.keywords.is_empty() {
            true => String::new(),
            false => format!(" · {}", escape(&article.keywords.join(", "))),
        },
    )
}

/// The two settings that change what search does with the page.
///
/// Loud, because they are invisible on the live page and both are the kind of
/// mistake nobody notices: a post that quietly never gets indexed, or one that
/// hands its ranking to another URL. Nothing is drawn when both are at their
/// defaults, which is almost always.
fn controls(article: &Article) -> String {
    let mut notes = Vec::new();
    if article.no_index {
        notes.push(
            "<strong>noindex</strong> — publishes and stays linkable, but drops out of \
                    /blog, out of the sitemap, and tells search engines not to index it"
                .to_string(),
        );
    }
    if !article.canonical_url.is_empty() {
        notes.push(format!(
            "<strong>canonical</strong> — search is told to credit {} for this content instead \
             of this page",
            escape(&article.canonical_url)
        ));
    }
    match notes.is_empty() {
        true => String::new(),
        false => format!("  <div class=\"controls\">{}</div>\n", notes.join("<br>")),
    }
}

/// The FAQ accordion, flat — the real page collapses it, and a preview that
/// hid the answers would hide the half worth reading.
fn faq(entries: &[super::schema::Faq]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let items: String = entries
        .iter()
        .map(|entry| {
            format!(
                "    <dt>{}</dt>\n    <dd>{}</dd>\n",
                escape(&entry.question),
                escape(&entry.answer)
            )
        })
        .collect();
    format!(
        "  <section class=\"faq\">\n    <h2>FAQ</h2>\n    <dl>\n{items}    </dl>\n  </section>\n"
    )
}

fn summary(article: &Article) -> String {
    format!(
        "{} section(s), {} figure(s)",
        article.section_count(),
        article.figures().len()
    )
}

fn render(block: &Block, figures: &[Figure]) -> String {
    match block {
        // Verbatim — see the module docs.
        Block::Text { html } => html.clone(),
        Block::Quote { text, highlight } => {
            format!(
                "  <blockquote>{}</blockquote>",
                highlighted(text, highlight)
            )
        }
        Block::Table { headers, rows } => table(headers, rows),
        // Named, not rendered. The component is React in the landing repo and
        // this page has no build of it; drawing an approximation would be the
        // "nearly the site" failure the module docs refuse. What can be checked
        // here is the one thing that matters — whether it sits in the right
        // place in the argument.
        Block::Embed { id } => format!(
            "  <div class=\"embed\">component <strong>{}</strong> renders here</div>",
            escape(id)
        ),
        Block::Figure { n } => match figures.iter().find(|figure| figure.n == *n) {
            Some(figure) => figure_html(figure),
            // Shown, not skipped: a preview exists to surface exactly this.
            None => format!(
                "  <div class=\"missing\">figure {n:02} is placed here but was never \
                 captured — the publish will refuse it.</div>"
            ),
        },
    }
}

/// The quote with its highlight in brand orange, the way the reader renders it.
///
/// Escaped first and matched second, so a highlight containing an ampersand
/// still lines up with the escaped text it is supposed to be inside.
fn highlighted(text: &str, highlight: &str) -> String {
    let text = escape(text);
    let highlight = escape(highlight);
    if highlight.is_empty() {
        return text;
    }
    match text.split_once(highlight.as_str()) {
        Some((before, after)) => format!("{before}<mark>{highlight}</mark>{after}"),
        // The generator already drops a highlight that is not a substring, so
        // reaching here means a hand-edited article.json. Renders plain rather
        // than pretending.
        None => text,
    }
}

fn table(headers: &[String], rows: &[Vec<String>]) -> String {
    let head: String = headers
        .iter()
        .map(|cell| format!("<th>{}</th>", escape(cell)))
        .collect();
    let body: String = rows
        .iter()
        .map(|row| {
            let cells: String = row
                .iter()
                .map(|cell| format!("<td>{}</td>", escape(cell)))
                .collect();
            format!("      <tr>{cells}</tr>\n")
        })
        .collect();
    format!("  <table>\n    <thead><tr>{head}</tr></thead>\n    <tbody>\n{body}    </tbody>\n  </table>")
}

fn figure_html(figure: &Figure) -> String {
    let alt = match figure.alt.trim().is_empty() {
        true => figure.caption.clone(),
        false => figure.alt.clone(),
    };
    let caption = match figure.caption.trim().is_empty() {
        // A figure placed before its blurb was written. Says so rather than
        // rendering an empty caption bar, which reads as a styling bug.
        true => format!("figure {:02} — no blurb yet", figure.n),
        false => escape(&figure.caption),
    };
    format!(
        "  <figure>\n    <img src=\"{}\" alt=\"{}\">\n    <figcaption>{caption}</figcaption>\n  </figure>",
        file_url(&figure.file),
        escape(&alt),
    )
}

/// A `file://` URL. Same encoding as [`crate::figure::pane`]'s, and here for the
/// same reason: a project folder with a space in its name would otherwise
/// produce a URL the browser cannot load, and the preview would show a broken
/// image with nothing explaining it.
fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut out = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            byte if byte.is_ascii_alphanumeric() => out.push(*byte as char),
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
