//! Getting the captured figures from disk into the CMS.
//!
//! Two halves of the same journey, split from [`super`] because they are the
//! only part of this stage that reaches into [`crate::figure`]: what the article
//! prompt is *offered*, and what the publish *uploads* once the article has
//! chosen.
//!
//! The asymmetry between them is the point. [`offers`] is generous — it hands
//! over everything with a blurb and lets the model leave figures out. [`upload`]
//! is strict, and stops the publish over a figure it cannot resolve. Everything
//! else in this stage refuses damage quietly; this one cannot, because the
//! article was read and approved *with* that figure in it, and publishing it
//! silently short publishes something nobody approved.

use anyhow::{bail, Context, Result};

use super::{generate, payload, strapi};
use crate::session::Session;

/// The blurbed figures, as the article prompt describes them.
///
/// Only the ones with a blurb. A figure with no caption cannot be placed
/// meaningfully — the model would be choosing a position for a picture it has
/// been told nothing about — and it would publish with an empty `<figcaption>`.
pub fn offers(session: &Session) -> Vec<generate::FigureOffer> {
    crate::figure::load(&session.root)
        .into_iter()
        .filter(|figure| figure.has_blurb())
        .map(|figure| generate::FigureOffer {
            n: figure.n,
            moment: figure.moment(),
            caption: figure.caption.clone(),
        })
        .collect()
}

/// Uploads each figure the article places and gathers what the CMS needs.
///
/// A figure that is placed but missing from the ledger, or whose JPEG has been
/// deleted, stops the publish rather than being skipped. Everything else in this
/// stage refuses damage quietly, but this one is different: the article was read
/// and approved *with* that figure in it, so publishing it silently short is
/// publishing something nobody approved.
pub(super) fn upload(
    client: &strapi::Strapi,
    session: &Session,
    wanted: &[u32],
) -> Result<Resolved> {
    resolve(session, wanted, |figure, alt| {
        let uploaded = client
            .upload_figure(&figure.file, Some(alt))
            .with_context(|| format!("uploading figure {:02}", figure.n))?;
        if uploaded.url.trim().is_empty() {
            bail!("strapi took figure {:02} but named no URL for it", figure.n);
        }
        Ok(uploaded.url)
    })
}

/// The same figures, resolved to the files on this machine.
///
/// For the dry run only. Everything but `src` is what a real publish would
/// send — the placement, the captions, the pixel sizes — so the block array can
/// be read and validated before anything is uploaded. The `src` is the local
/// path, which is the one field a publish cannot know in advance, and it is
/// deliberately not a plausible-looking CMS URL: a body that has not been
/// uploaded should not read as though it has.
pub(super) fn local(session: &Session, wanted: &[u32]) -> Result<Resolved> {
    resolve(session, wanted, |figure, _| {
        Ok(format!("file://{}", figure.file.display()))
    })
}

/// What the payload needs, keyed by figure number.
pub(super) type Resolved = std::collections::BTreeMap<u32, payload::FigureMedia>;

/// Everything both paths share: find the figure, check its file is still there,
/// settle the alt text, and measure it. Only the `src` differs.
fn resolve(
    session: &Session,
    wanted: &[u32],
    mut src: impl FnMut(&crate::figure::Figure, &str) -> Result<String>,
) -> Result<Resolved> {
    let captured = crate::figure::load(&session.root);
    let scale = display_scale();
    let mut out = Resolved::new();
    for n in wanted {
        let figure = captured
            .iter()
            .find(|figure| figure.n == *n)
            .with_context(|| format!("the article places figure {n:02}, which was never captured"))?;
        if !figure.file.is_file() {
            bail!(
                "figure {n:02}'s image is missing from {}",
                figure.file.display()
            );
        }
        let alt = match figure.alt.trim().is_empty() {
            // Falls back to the caption rather than to nothing: an empty alt on
            // a content image is an accessibility failure that ships silently.
            true => figure.caption.clone(),
            false => figure.alt.clone(),
        };
        // The file's own size when the row recorded one; for rows from before
        // that, the snip rect at the display's scale — see `display_scale`.
        let (width, height) = figure.pixel_size().unwrap_or((
            (figure.rect.w * scale).round().max(1.0) as u32,
            (figure.rect.h * scale).round().max(1.0) as u32,
        ));
        out.insert(
            *n,
            payload::FigureMedia {
                src: src(figure, &alt)?,
                alt,
                caption: figure.caption.clone(),
                width,
                height,
            },
        );
    }
    Ok(out)
}

/// The backing scale figure sizes were measured at.
///
/// Read from the display here rather than stored per figure, because it is a
/// property of the screen and not of the snip. Falls back to 1.0 — a figure laid
/// out at half its real size is sharper than intended, where a failed publish
/// over an unavailable display is a lost article.
fn display_scale() -> f64 {
    crate::config::load()
        .screen_display_id
        .and_then(|uid| crate::capture::screen::display_geometry(&uid).ok())
        .map(|geometry| geometry.scale())
        .unwrap_or(1.0)
}
