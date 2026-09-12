//! Everything about the finished longform that a writing stage works from.
//!
//! Gathered here rather than in each prompt builder so the two questions stay
//! apart: what the project knows, and how to ask a model about it. It sits at
//! the crate root because two stages now read it — [`crate::substack`] writes
//! typing notes from it and [`crate::blog`] writes the article — and a shared
//! input owned by one of its consumers is a sibling import waiting to happen.
//!
//! Deliberately not gated on the render, for the reason
//! [`crate::posts::collect_video_contexts`] documents — the words exist the
//! moment a chapter closes, and prose is written from words. The render only
//! adds timestamps, and a run without them is still usable.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::session::Session;

/// A place the video already lives, so a piece of writing can point at it.
///
/// Defined here rather than beside either stage's schema because both of them
/// carry it and neither owns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// A figure — see [`crate::figure`] — as a writing stage sees it: what the
/// reader will look at, and what the author said about it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FigureContext {
    pub n: u32,
    /// `ch 03 · 1:24`.
    pub moment: String,
    /// The blurb already written for it. Never empty: a figure with no caption
    /// is not offered, because it would publish under an empty `<figcaption>`.
    pub caption: String,
    /// What the author said over it, once transcribed. Empty when the figure was
    /// snipped without a break, or its aside never transcribed.
    pub said: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChapterContext {
    pub n: u32,
    pub title: String,
    pub points: Vec<String>,
    pub transcript: String,
    /// The figures taken at the end of this chapter, in capture order. A figure
    /// closes the chapter it is taken in — see `docs/figure-aside-plan.md` — so
    /// this is where it sits in the timeline: after these words, before the
    /// next chapter's.
    pub figures: Vec<FigureContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Longform {
    pub project_title: String,
    pub chapters: Vec<ChapterContext>,
    /// Where each chapter starts in the finished longform, `(n, "04:12")`.
    /// Empty until there is a cut to measure.
    pub timestamps: Vec<(u32, String)>,
    /// Where the video already is, so the essay can point at it.
    pub links: Vec<Link>,
    /// Figures no chapter here claims: taken before recording started, or at
    /// the end of a chapter that never transcribed. Still offered — they were
    /// taken on purpose — the writing stage just has no section to hang them on.
    pub loose_figures: Vec<FigureContext>,
}

pub fn build(session: &Session) -> Longform {
    let notes = session
        .notes_dir()
        .ok()
        .and_then(|dir| crate::notes::load_notes(&dir).ok());
    let titles = crate::titles::load(&session.titles_dir()).ok();

    let mut chapters: Vec<ChapterContext> = crate::notes::collect_completed(&session.dir)
        .into_iter()
        .map(|(n, transcript)| {
            let deck = notes
                .as_ref()
                .and_then(|data| data.chapters.get(n.saturating_sub(1) as usize));
            ChapterContext {
                n,
                // The approved card title first: it is the one a human has
                // already looked at, and the section headings should agree with
                // what the video puts on screen.
                title: titles
                    .as_ref()
                    .and_then(|manifest| manifest.title_for(n))
                    .map(str::to_string)
                    .or_else(|| deck.map(|chapter| chapter.title.clone()))
                    .unwrap_or_else(|| format!("Chapter {n}")),
                points: deck
                    .map(|chapter| chapter.points.clone())
                    .unwrap_or_default(),
                transcript,
                figures: Vec::new(),
            }
        })
        .collect();
    let loose_figures = attach_figures(&mut chapters, crate::figure::load(&session.root));

    Longform {
        // The title Titles wrote, which is what the video is actually called —
        // the folder's name is a slug with a ticket number in it.
        project_title: titles
            .as_ref()
            .and_then(|manifest| manifest.longform_title())
            .map(str::to_string)
            .unwrap_or_else(|| session.title()),
        chapters,
        timestamps: timestamps(&session.edit_dir()),
        links: links(session),
        loose_figures,
    }
}

/// Files each blurbed figure under the chapter it closed, in capture order, and
/// hands back the ones no chapter here claims.
///
/// Blurbed only — the rule [`crate::blog`]'s offers already apply: a figure with
/// no caption cannot be placed meaningfully and would publish under an empty
/// caption. A figure whose chapter is absent — never transcribed, or `None`
/// because nothing was recording — is loose rather than dropped, because it was
/// still taken on purpose.
pub fn attach_figures(
    chapters: &mut [ChapterContext],
    figures: Vec<crate::figure::Figure>,
) -> Vec<FigureContext> {
    let mut loose = Vec::new();
    for figure in figures.into_iter().filter(|figure| figure.has_blurb()) {
        let context = FigureContext {
            n: figure.n,
            moment: figure.moment(),
            caption: figure.caption.clone(),
            said: figure.said.clone(),
        };
        let owner = figure
            .chapter
            .and_then(|n| chapters.iter_mut().find(|chapter| chapter.n == n));
        match owner {
            Some(chapter) => chapter.figures.push(context),
            None => loose.push(context),
        }
    }
    loose
}

/// Where each chapter starts in the longform, read off the cuts it was built
/// from.
///
/// Returns nothing at all if any chapter cannot be measured, rather than a
/// partial list: offsets are cumulative, so one missing duration does not lose
/// one timestamp — it silently moves every later one, and a wrong timestamp is
/// worse than no timestamp when the whole point is checking a quote quickly.
fn timestamps(edit_dir: &Path) -> Vec<(u32, String)> {
    let mut durations = Vec::new();
    for n in 1..=99u32 {
        let cut = edit_dir
            .join(format!("chapter-{n:02}"))
            .join(format!("chapter-{n:02}-horizontal.mp4"));
        if !cut.is_file() {
            continue;
        }
        match crate::edit::cut::probe_duration_seconds(&cut) {
            Ok(seconds) => durations.push((n, seconds)),
            Err(err) => {
                eprintln!("stream-recorder: no chapter offsets — {err:#}");
                return Vec::new();
            }
        }
    }
    // No lead: the longform opens on chapter one's footage. The opening title
    // card that used to stand in front was one card long, and every marker
    // moved three seconds earlier when it went.
    offsets(&durations, crate::edit::compose::CARD_SECONDS, 0.0)
        .into_iter()
        .map(|(n, seconds)| (n, fmt_timestamp(seconds)))
        .collect()
}

/// The longform's shape, from [`crate::edit::compose::prepare_targets`]: chapter
/// one straight away, then a card in front of every chapter after it.
///
/// `lead` is whatever plays before chapter one — nothing, now that the opening
/// title card is gone — and `card` is one chapter card. Both are passed in
/// rather than assumed because this is the one function that has to agree with
/// how the video was actually assembled, and it has been wrong three times: when
/// chapter one's card was added, when it was taken away again, and when the
/// opening title came and went.
/// Either way the error is silent — every marker lands a few seconds off, which
/// is close enough to look right and far enough to quote the wrong sentence.
pub fn offsets(durations: &[(u32, f64)], card: f64, lead: f64) -> Vec<(u32, f64)> {
    let mut out = Vec::with_capacity(durations.len());
    let mut cursor = lead.max(0.0);
    for (index, (n, seconds)) in durations.iter().enumerate() {
        // Chapter one has no card of its own; the video opens on it.
        if index > 0 {
            cursor += card;
        }
        out.push((*n, cursor));
        cursor += seconds;
    }
    out
}

/// `04:12`, and `1:02:03` once there is an hour to show.
pub fn fmt_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Where the video already is. Absent links are absent, never placeholders.
fn links(session: &Session) -> Vec<Link> {
    let mut out = Vec::new();
    if let Some(upload) = crate::publish::longform(session) {
        out.push(Link {
            label: "Watch on YouTube".into(),
            url: upload.url.clone(),
        });
    }
    if let Ok(links) = crate::distribute::load(&session.distribute_dir()) {
        if let Some(url) = links.url_for("longform") {
            out.push(Link {
                label: "Longform mp4".into(),
                url: url.to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::figure::{Figure, Snipped};

    fn figure(n: u32, chapter: Option<u32>, caption: &str, said: &str) -> Figure {
        Figure {
            n,
            file: std::path::PathBuf::from(format!("/tmp/figure-{n:02}.jpg")),
            at: "2026-09-04T10:22:31.000000Z".into(),
            chapter,
            offset: Some(30.0),
            rect: Snipped {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 450.0,
            },
            width: 1600,
            height: 900,
            audio: None,
            transcribing: false,
            said: said.into(),
            caption: caption.into(),
            alt: String::new(),
        }
    }

    fn chapter(n: u32) -> ChapterContext {
        ChapterContext {
            n,
            title: format!("Chapter {n}"),
            points: Vec::new(),
            transcript: "words".into(),
            figures: Vec::new(),
        }
    }

    /// A figure closes the chapter it was taken in, so that is where it sits
    /// in the evidence — after that chapter's words and before the next.
    #[test]
    fn figures_are_filed_under_the_chapter_they_closed() {
        let mut chapters = vec![chapter(1), chapter(2)];
        let loose = attach_figures(
            &mut chapters,
            vec![
                figure(1, Some(2), "The queue.", "this is the retry storm"),
                figure(2, Some(1), "The config.", ""),
            ],
        );
        assert!(loose.is_empty());
        assert_eq!(chapters[0].figures.len(), 1);
        assert_eq!(chapters[0].figures[0].n, 2);
        assert_eq!(chapters[1].figures[0].caption, "The queue.");
        assert_eq!(chapters[1].figures[0].said, "this is the retry storm");
    }

    /// Unblurbed figures are not offered — they would publish captionless — and
    /// figures whose chapter is not here are kept loose rather than lost.
    #[test]
    fn figures_without_a_home_are_loose_and_without_a_blurb_are_absent() {
        let mut chapters = vec![chapter(1)];
        let loose = attach_figures(
            &mut chapters,
            vec![
                figure(1, None, "Before we started.", ""),
                figure(2, Some(7), "A chapter that never transcribed.", ""),
                figure(3, Some(1), "   ", "said but never captioned"),
            ],
        );
        assert_eq!(loose.iter().map(|f| f.n).collect::<Vec<_>>(), vec![1, 2]);
        assert!(chapters[0].figures.is_empty());
    }

    /// The regression this pins: the longform opens on chapter one, and only the
    /// chapters after it get a card. Computing these against either of the other
    /// layouts this has had — a card in front of chapter one, or an opening title
    /// card — puts every marker three seconds out.
    #[test]
    fn chapter_offsets_start_at_zero_and_add_a_card_before_each_later_chapter() {
        let durations = vec![(1, 60.0), (2, 120.0), (3, 30.0)];
        let got = offsets(&durations, 3.0, 0.0);
        assert_eq!(got[0], (1, 0.0), "chapter one opens the video");
        assert_eq!(got[1], (2, 63.0));
        assert_eq!(got[2], (3, 186.0));
    }

    /// A lead, should a layout ever have one again, only moves the start.
    #[test]
    fn a_lead_only_moves_the_start() {
        let got = offsets(&[(1, 60.0), (2, 30.0)], 3.0, 3.0);
        assert_eq!(got, vec![(1, 3.0), (2, 66.0)]);
    }

    #[test]
    fn offsets_of_nothing_are_nothing() {
        assert!(offsets(&[], 3.0, 3.0).is_empty());
        assert_eq!(offsets(&[(4, 10.0)], 3.0, 3.0), vec![(4, 3.0)]);
    }

    /// Chapter numbers are carried through rather than re-derived, so a project
    /// whose first chapter was deleted still labels the rest correctly.
    #[test]
    fn a_gap_in_the_chapter_numbers_keeps_the_numbers_it_was_given() {
        let got = offsets(&[(2, 10.0), (5, 20.0)], 3.0, 3.0);
        assert_eq!(got, vec![(2, 3.0), (5, 16.0)]);
    }

    #[test]
    fn timestamps_grow_an_hours_column_only_when_there_is_one() {
        assert_eq!(fmt_timestamp(0.0), "00:00");
        assert_eq!(fmt_timestamp(63.4), "01:03");
        assert_eq!(fmt_timestamp(3600.0), "1:00:00");
        assert_eq!(fmt_timestamp(3723.0), "1:02:03");
        // A negative offset is not reachable, but clamping is cheaper than a
        // panic in a format string on a repaint.
        assert_eq!(fmt_timestamp(-5.0), "00:00");
    }

    /// Nothing cut yet is not a failure — the notes are written from words.
    #[test]
    fn an_unrendered_project_simply_has_no_timestamps() {
        let dir = std::env::temp_dir().join(format!("longform-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(timestamps(&dir).is_empty());
    }
}
