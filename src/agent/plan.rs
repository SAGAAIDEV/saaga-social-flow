//! The extractor behind [`crate::plan`]: an idea talked into the mic (or typed,
//! or rehearsed) in, a plan for recording it out.
//!
//! The model is asked for the plan's *shape* — the hook, then the outline, then
//! the body chapters, then the call to action — and [`clean`] enforces it
//! rather than trusting it, because recording chapter *n* is plan chapter *n*
//! and a hook that drifted to position two would put every card title one
//! chapter off. The layouts are part of the shape: the hook is always a
//! talking head, the outline is always the outline layout listing the body
//! chapters by title, and every other chapter gets a layout that fits what is
//! on screen when the model left it out.

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::layouts::Pair;
use crate::plan::schema::{ChapterKind, Cta, Hook, Plan, PlanBody, PlanChapter};

use super::prompt;

/// The most body chapters a plan keeps — as many as the outline card lists
/// (see [`crate::outline::MAX_POINTS`]). Past this it is two videos.
pub const MAX_BODY_CHAPTERS: usize = crate::outline::MAX_POINTS;
/// The outline chapter's title when the model gives none.
const OUTLINE_TITLE: &str = "What we'll cover";
/// The most points a chapter keeps — what the deck shows at a glance.
pub const MAX_POINTS: usize = 6;
pub const MAX_OUTLINE: usize = 8;
pub const MAX_INSTRUCTIONS: usize = 10;
/// A chapter title is a card title: it has to read at a glance.
pub const MAX_TITLE_CHARS: usize = 48;
pub const MAX_POINT_CHARS: usize = 90;

pub const SYSTEM: &str = "You plan a video before it is recorded. The author has talked \
through the idea, typed notes, or recorded a rehearsal; you turn that into a plan they \
will record from, chapter by chapter, with the plan on a teleprompter beside the camera.\n\n\
Output a JSON object:\n\
- working_title: what the video shows or argues, under 70 characters.\n\
- audience: who it is for, one sentence.\n\
- promise: what the viewer walks away with, one sentence.\n\
- hook: { line, angle }. line is the opening sentence or two, written to be said out \
loud; angle is why it holds a viewer.\n\
- outline: the arc in 3-6 short beats.\n\
- chapters: in recording order, always in this shape: one chapter of kind \"hook\" \
first; then one of kind \"outline\", where the author tells the viewer what the video \
covers; then two to eight of kind \"body\", one per topic; then one of kind \"cta\" \
last.\n\
- cta: { line, placement }. line is the ask, written to be said; placement is what \
earns it.\n\
- instructions: 3-8 directions for recording: what to have open, setup, delivery.\n\n\
Each chapter:\n\
- kind: \"hook\", \"outline\", \"body\" or \"cta\".\n\
- title: 2-5 words naming it. No numbering, no trailing punctuation. A body chapter's \
title is its chapter card and its line in the outline, so it names the topic.\n\
- goal: what the viewer should understand by its end, one sentence.\n\
- points: the beats to hit, as short fragments a glance is enough for. 2-5, never more \
than 6.\n\
- verbatim: only where the exact words matter. The hook chapter's is the hook line.\n\
- cues: delivery notes, only where earned. Omit otherwise.\n\
- show: what is on screen while it is said, or \"camera\" for a talking head.\n\
- layout: \"talking-head\", \"split\" (screen beside the camera) or \"outline\" (the \
points beside the camera).\n\
- est_seconds: roughly how long it runs.\n\
- card: body chapters only. Whether its chapter card (number and title) goes in front of \
it. Leave it out to keep the card; false only when the chapter carries on from the one \
before without a break, or the author asks. When a current plan is given, keep each \
chapter's card as it is unless the refine note changes it.\n\n\
The project's format and category come first in the request. When there is a category, \
the video belongs in it: plan for the people who follow that topic.\n\n\
Keep the author's voice and their examples; tighten, do not invent facts they did not \
give. The hook chapter is short — the hook and the promise, nothing else — said to the \
camera, layout \"talking-head\". The outline chapter is short too: one line per body \
chapter, in order, layout \"outline\"; its points are the body chapters' titles. The \
CTA chapter is short, usually \"talking-head\". When a current plan is given, refine it: keep what works and \
change what the refine note asks.";

/// The plan for a short-format project: one vertical video, recorded as one
/// chapter. One chapter because a short renders and posts its chapters as
/// clips of their own (see [`crate::config::RenderTargets::for_session`]), and
/// because a short has no room for the long video's outline and closing ask —
/// the render leaves both out of every vertical.
pub const SHORT_SYSTEM: &str = "You plan a short video before it is recorded: one vertical \
video under a minute, recorded in one take with the plan on a teleprompter beside the camera. \
The author has talked through the idea, typed notes, or recorded a rehearsal.\n\n\
Output a JSON object:\n\
- working_title: what the short shows or argues, under 70 characters.\n\
- audience: who it is for, one sentence.\n\
- promise: what the viewer gets from it, one sentence.\n\
- hook: { line, angle }. line is the first sentence, said in the first two seconds: the \
claim, or the result on screen. angle is why it stops the scroll.\n\
- outline: the beats in order, 2-4 of them.\n\
- chapters: exactly one, of kind \"body\": the whole short.\n\
- cta: { line, placement }. line is how it ends: a line to remember, a question for the \
viewer, or a short ask. placement is what earns it.\n\
- instructions: 2-5 directions for recording: what to have open, framing, delivery.\n\n\
The chapter:\n\
- kind: \"body\".\n\
- title: 2-5 words naming it. No numbering, no trailing punctuation.\n\
- goal: what the viewer should get from it, one sentence.\n\
- points: the beats after the hook, as short fragments a glance is enough for. 2-4.\n\
- verbatim: the hook line, unless other exact words matter more.\n\
- cues: delivery notes, only where earned. Omit otherwise.\n\
- show: what is on screen while it is said, or \"camera\" for a talking head.\n\
- layout: \"talking-head\" when the camera carries it, \"split\" (screen beside the \
camera) when the screen does.\n\
- est_seconds: how long it runs, under 60.\n\n\
One point, made fast: no intro, no outline, no \"in this video\". The project's category \
comes first in the request with what makes a short one of its kind; follow it. Keep the \
author's voice and their examples; tighten, do not invent facts they did not give. When a \
current plan is given, refine it: keep what works and change what the refine note asks.";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
struct PlanExtraction {
    #[serde(default)]
    working_title: String,
    #[serde(default)]
    audience: String,
    #[serde(default)]
    promise: String,
    #[serde(default)]
    hook: ExtractedHook,
    #[serde(default)]
    outline: Vec<String>,
    #[serde(default)]
    chapters: Vec<ExtractedChapter>,
    #[serde(default)]
    cta: ExtractedCta,
    #[serde(default)]
    instructions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
struct ExtractedHook {
    #[serde(default)]
    line: String,
    #[serde(default)]
    angle: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
struct ExtractedCta {
    #[serde(default)]
    line: String,
    #[serde(default)]
    placement: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedChapter {
    kind: ChapterKind,
    #[serde(default)]
    title: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    points: Vec<String>,
    #[serde(default)]
    verbatim: Option<String>,
    #[serde(default)]
    cues: Vec<String>,
    #[serde(default)]
    show: String,
    #[serde(default)]
    layout: Option<ExtractedLayout>,
    #[serde(default)]
    est_seconds: Option<u32>,
    #[serde(default)]
    card: Option<bool>,
}

/// [`Pair`] as the model names it. Its own type because `Pair` is the
/// recorder's, and the schema the model sees should not change when the
/// recorder's does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum ExtractedLayout {
    TalkingHead,
    Split,
    Outline,
}

impl From<ExtractedLayout> for Pair {
    fn from(layout: ExtractedLayout) -> Pair {
        match layout {
            ExtractedLayout::TalkingHead => Pair::TalkingHead,
            ExtractedLayout::Split => Pair::Split,
            ExtractedLayout::Outline => Pair::Outline,
        }
    }
}

/// What the project is, from its settings: the planner is told before it
/// reads a word of the idea, so a short is planned as a short and a demo as a
/// demo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Brief {
    pub format: crate::sessions::Format,
    /// The category's name, when the project has one.
    pub category: Option<String>,
    /// What makes a short one of its kind — see
    /// [`crate::category::ShortCategory::definition`]. Topic categories have none.
    pub definition: Option<&'static str>,
}

impl Brief {
    fn short(&self) -> bool {
        self.format == crate::sessions::Format::Short
    }
}

/// What a plan is built from. Every field is optional on its own; [`user_prompt`]
/// refuses when there is nothing at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sources {
    pub brief: Brief,
    pub instructions: String,
    pub typed: String,
    /// `(take number, transcript)` for each idea take with words.
    pub takes: Vec<(u32, String)>,
    /// `(chapter number, transcript)` from a rehearsal recording.
    pub rehearsal: Vec<(u32, String)>,
}

impl Sources {
    fn has_material(&self) -> bool {
        !self.typed.trim().is_empty()
            || self.takes.iter().any(|(_, t)| !t.trim().is_empty())
            || self.rehearsal.iter().any(|(_, t)| !t.trim().is_empty())
    }
}

/// A refine: the version it starts from and what should change.
pub struct Refine<'a> {
    pub base: &'a Plan,
    pub note: &'a str,
}

pub fn user_prompt(project: &str, sources: &Sources, refine: Option<&Refine>) -> Result<String> {
    if !sources.has_material() && refine.is_none() {
        bail!("nothing to plan from yet — record an idea take, type the idea, or plan from a rehearsal");
    }
    let mut user = format!("Video project: {project}\n");
    let brief = &sources.brief;
    user.push_str(if brief.short() {
        "Format: short: one vertical video under a minute, recorded as one chapter\n"
    } else {
        "Format: long: a horizontal video recorded in chapters\n"
    });
    match (&brief.category, brief.definition) {
        (Some(name), Some(definition)) => {
            user.push_str(&format!("Category: {name}. {definition}\n"));
        }
        (Some(name), None) => user.push_str(&format!("Category: {name}\n")),
        (None, _) => user.push_str("Category: none picked yet\n"),
    }
    let instructions = sources.instructions.trim();
    user.push_str("\nAuthor's instructions:\n");
    user.push_str(if instructions.is_empty() {
        "(none)"
    } else {
        instructions
    });
    user.push('\n');
    for (n, text) in &sources.takes {
        let text = text.trim();
        if !text.is_empty() {
            user.push_str(&format!("\n<Idea take {n}>\n{text}\n</Idea take {n}>\n"));
        }
    }
    let typed = sources.typed.trim();
    if !typed.is_empty() {
        user.push_str(&format!("\n<Typed idea>\n{typed}\n</Typed idea>\n"));
    }
    for (n, text) in &sources.rehearsal {
        let text = text.trim();
        if !text.is_empty() {
            user.push_str(&format!(
                "\n<Rehearsal chapter {n}>\n{text}\n</Rehearsal chapter {n}>\n"
            ));
        }
    }
    if let Some(refine) = refine {
        let current = serde_json::to_string_pretty(&refine.base.body)?;
        user.push_str(&format!(
            "\n<Current plan>\n{current}\n</Current plan>\n\nRefine note:\n{}\n",
            match refine.note.trim() {
                "" => "(none — tighten it where it is weakest)",
                note => note,
            }
        ));
    }
    Ok(user)
}

#[tracing::instrument(skip(sources, refine, prompt_root), fields(model, provider))]
pub fn build_plan(
    project: &str,
    sources: &Sources,
    refine: Option<&Refine>,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(PlanBody, super::trace::LlmStep)> {
    let short = sources.brief.short();
    let (prompt_id, builtin) = if short {
        (prompt::PLAN_SHORT, SHORT_SYSTEM)
    } else {
        (prompt::PLAN, SYSTEM)
    };
    let preamble = prompt::resolve(prompt_id, builtin, prompt_root);
    let prompt = user_prompt(project, sources, refine)?;
    let (extracted, step) = super::extract::extract::<PlanExtraction>(
        prompt_id, "plan", &preamble, prompt, model, provider,
    )?;
    let body = if short {
        clean_short(extracted)?
    } else {
        clean(extracted)?
    };
    Ok((body, step))
}

/// The model's plan, trimmed, capped, and put in the one shape the recorder
/// can use: hook, outline, body chapters, call to action.
///
/// A hook or CTA chapter the model left out is made from the `hook` / `cta`
/// line; a second one is kept as a body chapter where it stood rather than
/// dropped, since it is still something the author said. A plan with no hook
/// line, no CTA line or no body at all is an error — there is nothing to fill
/// the gap from.
///
/// The outline is built here rather than taken on trust: its points are the
/// body chapters' titles, in order, so the card on screen promises exactly the
/// chapters that follow. The model's outline chapter lends only its title and
/// wording. A plan with a single body chapter has nothing to outline and gets
/// none. Then the layouts: see [`check_layouts`].
fn clean(raw: PlanExtraction) -> Result<PlanBody> {
    let mut chapters: Vec<PlanChapter> =
        raw.chapters.into_iter().filter_map(clean_chapter).collect();

    let mut hook_line = raw.hook.line.trim().to_string();
    let mut cta_line = raw.cta.line.trim().to_string();

    let hook = take_kind(&mut chapters, ChapterKind::Hook, true);
    let cta = take_kind(&mut chapters, ChapterKind::Cta, false);
    let outline = take_kind(&mut chapters, ChapterKind::Outline, true);
    for chapter in &mut chapters {
        chapter.kind = ChapterKind::Body;
    }
    chapters.truncate(MAX_BODY_CHAPTERS);
    if chapters.is_empty() {
        bail!("the plan came back with no body chapters — build it again");
    }

    // A line missing at the top level but present as the chapter's wording
    // is the same line; take it from there.
    if hook_line.is_empty() {
        hook_line = verbatim_of(hook.as_ref());
    }
    if cta_line.is_empty() {
        cta_line = verbatim_of(cta.as_ref());
    }
    if hook_line.is_empty() {
        bail!("the plan came back with no hook — build it again");
    }
    if cta_line.is_empty() {
        bail!("the plan came back with no call to action — build it again");
    }
    let hook = hook.unwrap_or_else(|| made_chapter(ChapterKind::Hook, "The hook"));
    let cta = cta.unwrap_or_else(|| made_chapter(ChapterKind::Cta, "What to do next"));
    let outline = (chapters.len() > 1).then(|| outline_chapter(outline, &chapters));

    let mut ordered = Vec::with_capacity(chapters.len() + 3);
    ordered.push(hook);
    ordered.extend(outline);
    ordered.extend(chapters);
    ordered.push(cta);
    check_layouts(&mut ordered);

    Ok(PlanBody {
        working_title: raw.working_title.trim().to_string(),
        audience: raw.audience.trim().to_string(),
        promise: raw.promise.trim().to_string(),
        hook: Hook {
            line: hook_line,
            angle: raw.hook.angle.trim().to_string(),
        },
        outline: clean_list(&raw.outline, MAX_OUTLINE, MAX_POINT_CHARS),
        chapters: ordered,
        cta: Cta {
            line: cta_line,
            placement: raw.cta.placement.trim().to_string(),
        },
        instructions: clean_list(&raw.instructions, MAX_INSTRUCTIONS, 200),
    })
}

/// The model's plan for a short, in the one shape a short records in: a
/// single body chapter.
///
/// What the model split across chapters anyway is folded into one rather
/// than dropped: the first body chapter (else the first) lends its title and
/// wording, and every chapter's points follow in order, capped like any
/// chapter's. The hook line is what it says first, word for word, unless the
/// chapter has wording of its own, and the closing line rides as a cue. A
/// short needs a hook; it may end without an ask.
fn clean_short(raw: PlanExtraction) -> Result<PlanBody> {
    let chapters: Vec<PlanChapter> = raw.chapters.into_iter().filter_map(clean_chapter).collect();
    let hook_line = match raw.hook.line.trim() {
        "" => verbatim_of(chapters.iter().find(|c| c.kind == ChapterKind::Hook)),
        line => line.to_string(),
    };
    if hook_line.is_empty() {
        bail!("the plan came back with no hook — build it again");
    }
    let lead = chapters
        .iter()
        .position(|c| c.kind == ChapterKind::Body)
        .unwrap_or(0);
    let Some(mut chapter) = chapters.get(lead).cloned() else {
        bail!("the plan came back with no chapter — build it again");
    };
    let points: Vec<String> = chapters.iter().flat_map(|c| c.points.clone()).collect();
    chapter.points = clean_list(&points, MAX_POINTS, MAX_POINT_CHARS);
    let estimates: Vec<u32> = chapters.iter().filter_map(|c| c.est_seconds).collect();
    chapter.est_seconds = (!estimates.is_empty()).then(|| estimates.iter().sum());
    chapter.kind = ChapterKind::Body;
    if chapter.verbatim.is_none() {
        chapter.verbatim = Some(hook_line.clone());
    }
    let cta_line = raw.cta.line.trim().to_string();
    if !cta_line.is_empty() {
        chapter.cues.push(format!("End on: {cta_line}"));
    }
    let mut ordered = vec![chapter];
    check_layouts(&mut ordered);
    Ok(PlanBody {
        working_title: raw.working_title.trim().to_string(),
        audience: raw.audience.trim().to_string(),
        promise: raw.promise.trim().to_string(),
        hook: Hook {
            line: hook_line,
            angle: raw.hook.angle.trim().to_string(),
        },
        outline: clean_list(&raw.outline, MAX_OUTLINE, MAX_POINT_CHARS),
        chapters: ordered,
        cta: Cta {
            line: cta_line,
            placement: raw.cta.placement.trim().to_string(),
        },
        instructions: clean_list(&raw.instructions, MAX_INSTRUCTIONS, 200),
    })
}

/// The outline chapter: the model's, when it wrote one, with its points
/// replaced by the body chapters' titles.
fn outline_chapter(model: Option<PlanChapter>, body: &[PlanChapter]) -> PlanChapter {
    let mut chapter = model.unwrap_or_else(|| made_chapter(ChapterKind::Outline, OUTLINE_TITLE));
    if chapter.title.trim().is_empty() {
        chapter.title = OUTLINE_TITLE.into();
    }
    chapter.points = body
        .iter()
        .map(|chapter| clip(&chapter.title, crate::outline::MAX_TEXT_CHARS))
        .collect();
    chapter.show = "the outline beside the camera".into();
    chapter
}

/// Every chapter's layout, checked against its place in the shape.
///
/// The hook is a talking head whatever the model said: it is the face that
/// opens the video. The outline is the outline layout — the layout exists to
/// show its points. A body or CTA chapter keeps the layout it was given, and
/// one with none gets the layout its `show` implies: the screen beside the
/// camera when there is something to show, a talking head when it is the
/// camera alone.
fn check_layouts(chapters: &mut [PlanChapter]) {
    for chapter in chapters {
        match chapter.kind {
            ChapterKind::Hook => {
                chapter.layout = Some(Pair::TalkingHead);
                chapter.show = "camera".into();
            }
            ChapterKind::Outline => chapter.layout = Some(Pair::Outline),
            ChapterKind::Body | ChapterKind::Cta => {
                let show = chapter.show.trim();
                chapter.layout = chapter.layout.or(Some(
                    if show.is_empty() || show.eq_ignore_ascii_case("camera") {
                        Pair::TalkingHead
                    } else {
                        Pair::Split
                    },
                ));
            }
        }
    }
}

/// Removes and returns the first (`first`) or last chapter of `kind`.
fn take_kind(
    chapters: &mut Vec<PlanChapter>,
    kind: ChapterKind,
    first: bool,
) -> Option<PlanChapter> {
    let index = if first {
        chapters.iter().position(|c| c.kind == kind)
    } else {
        chapters.iter().rposition(|c| c.kind == kind)
    }?;
    Some(chapters.remove(index))
}

fn verbatim_of(chapter: Option<&PlanChapter>) -> String {
    chapter
        .and_then(|c| c.verbatim.as_deref())
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn made_chapter(kind: ChapterKind, title: &str) -> PlanChapter {
    PlanChapter {
        kind,
        title: title.into(),
        goal: String::new(),
        points: Vec::new(),
        verbatim: None,
        cues: Vec::new(),
        show: "camera".into(),
        layout: Some(Pair::TalkingHead),
        est_seconds: None,
        card: true,
    }
}

/// `None` for a chapter with neither a title nor a point: nothing to record.
fn clean_chapter(raw: ExtractedChapter) -> Option<PlanChapter> {
    let title = clip(
        raw.title.trim().trim_end_matches(['.', ',', ';', ':']),
        MAX_TITLE_CHARS,
    );
    let points = clean_list(&raw.points, MAX_POINTS, MAX_POINT_CHARS);
    if title.is_empty() && points.is_empty() {
        return None;
    }
    Some(PlanChapter {
        kind: raw.kind,
        title,
        goal: raw.goal.trim().to_string(),
        points,
        verbatim: raw
            .verbatim
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        cues: clean_list(&raw.cues, MAX_POINTS, MAX_POINT_CHARS),
        show: raw.show.trim().to_string(),
        layout: raw.layout.map(Pair::from),
        est_seconds: raw.est_seconds.filter(|&s| s > 0),
        card: raw.card.unwrap_or(true),
    })
}

fn clean_list(items: &[String], max: usize, max_chars: usize) -> Vec<String> {
    items
        .iter()
        .map(|item| clip(item.trim(), max_chars))
        .filter(|item| !item.is_empty())
        .take(max)
        .collect()
}

/// Character-wise, ending on a word where it can — the same rule as
/// [`super::outline`]'s points.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(space) if space > max / 2 => cut[..space].trim_end().to_string(),
        _ => cut.trim_end().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(kind: ChapterKind, title: &str) -> ExtractedChapter {
        ExtractedChapter {
            kind,
            title: title.into(),
            goal: String::new(),
            points: vec![format!("{title} point")],
            verbatim: None,
            cues: Vec::new(),
            show: String::new(),
            layout: None,
            est_seconds: None,
            card: None,
        }
    }

    fn raw(chapters: Vec<ExtractedChapter>) -> PlanExtraction {
        PlanExtraction {
            working_title: " Ship it ".into(),
            hook: ExtractedHook {
                line: "Your deploy takes an hour.".into(),
                angle: String::new(),
            },
            cta: ExtractedCta {
                line: "Subscribe for part two.".into(),
                placement: String::new(),
            },
            chapters,
            ..PlanExtraction::default()
        }
    }

    fn kinds(body: &PlanBody) -> Vec<ChapterKind> {
        body.chapters.iter().map(|c| c.kind).collect()
    }

    fn titles(body: &PlanBody) -> Vec<&str> {
        body.chapters.iter().map(|c| c.title.as_str()).collect()
    }

    use ChapterKind::{Body, Cta as CtaKind, Hook as HookKind, Outline as OutlineKind};

    #[test]
    fn a_well_formed_plan_keeps_its_order() {
        let body = clean(raw(vec![
            chapter(HookKind, "The hour"),
            chapter(Body, "The cache"),
            chapter(Body, "The fix"),
            chapter(CtaKind, "Next time"),
        ]))
        .unwrap();
        assert_eq!(body.working_title, "Ship it");
        assert_eq!(kinds(&body), [HookKind, OutlineKind, Body, Body, CtaKind]);
        assert_eq!(
            titles(&body),
            [
                "The hour",
                OUTLINE_TITLE,
                "The cache",
                "The fix",
                "Next time"
            ]
        );
    }

    /// The shape is enforced: a hook the model put second moves to the front,
    /// a CTA it put first moves to the end, and a second hook stays where it
    /// was as body.
    #[test]
    fn hook_goes_first_cta_last_and_extras_become_body() {
        let body = clean(raw(vec![
            chapter(CtaKind, "Next time"),
            chapter(HookKind, "The hour"),
            chapter(Body, "The cache"),
            chapter(HookKind, "Another hook"),
        ]))
        .unwrap();
        assert_eq!(kinds(&body), [HookKind, OutlineKind, Body, Body, CtaKind]);
        assert_eq!(
            titles(&body),
            [
                "The hour",
                OUTLINE_TITLE,
                "The cache",
                "Another hook",
                "Next time"
            ]
        );
    }

    /// The outline lists the body chapters by title, in order, whatever the
    /// model put in it — and keeps the model's title and wording, moved to
    /// right after the hook wherever the model put it.
    #[test]
    fn the_outline_follows_the_hook_and_lists_the_body_titles() {
        let body = clean(raw(vec![
            chapter(HookKind, "The hour"),
            chapter(Body, "The cache"),
            ExtractedChapter {
                points: vec!["Something else entirely".into()],
                verbatim: Some("Here is the plan.".into()),
                layout: Some(ExtractedLayout::Split),
                ..chapter(OutlineKind, "The road map")
            },
            chapter(Body, "The fix"),
            chapter(CtaKind, "Next time"),
        ]))
        .unwrap();
        let outline = &body.chapters[1];
        assert_eq!(outline.kind, OutlineKind);
        assert_eq!(outline.title, "The road map");
        assert_eq!(outline.points, ["The cache", "The fix"]);
        assert_eq!(outline.verbatim.as_deref(), Some("Here is the plan."));
        assert_eq!(outline.layout, Some(Pair::Outline));
    }

    /// One body chapter has nothing to outline.
    #[test]
    fn a_single_body_chapter_gets_no_outline() {
        let body = clean(raw(vec![
            chapter(HookKind, "The hour"),
            chapter(OutlineKind, "Coming up"),
            chapter(Body, "The cache"),
            chapter(CtaKind, "Next time"),
        ]))
        .unwrap();
        assert_eq!(kinds(&body), [HookKind, Body, CtaKind]);
    }

    /// The hook is a talking head whatever the model said; a chapter with no
    /// layout gets the one its `show` implies, and a given one is kept.
    #[test]
    fn every_chapter_gets_a_checked_layout() {
        let body = clean(raw(vec![
            ExtractedChapter {
                layout: Some(ExtractedLayout::Split),
                show: "the dashboard".into(),
                ..chapter(HookKind, "The hour")
            },
            ExtractedChapter {
                show: "the terminal".into(),
                ..chapter(Body, "The cache")
            },
            ExtractedChapter {
                show: "camera".into(),
                ..chapter(Body, "Why it matters")
            },
            ExtractedChapter {
                layout: Some(ExtractedLayout::Outline),
                ..chapter(Body, "Three rules")
            },
            chapter(CtaKind, "Next time"),
        ]))
        .unwrap();
        let layouts: Vec<_> = body.chapters.iter().map(|c| c.layout).collect();
        assert_eq!(
            layouts,
            [
                Some(Pair::TalkingHead),
                Some(Pair::Outline),
                Some(Pair::Split),
                Some(Pair::TalkingHead),
                Some(Pair::Outline),
                Some(Pair::TalkingHead),
            ]
        );
        assert_eq!(body.chapters[0].show, "camera");
    }

    #[test]
    fn a_missing_hook_or_cta_chapter_is_made_from_its_line() {
        let body = clean(raw(vec![chapter(Body, "The cache")])).unwrap();
        assert_eq!(kinds(&body), [HookKind, Body, CtaKind]);
        assert_eq!(body.chapters[0].title, "The hook");
        assert_eq!(body.chapters[2].title, "What to do next");
        // And the deck gives them their lines.
        let plan = Plan {
            body,
            ..Plan::default()
        };
        let notes = plan.to_notes();
        assert_eq!(
            notes.chapters[0].verbatim.as_deref(),
            Some("Your deploy takes an hour.")
        );
        assert_eq!(
            notes.chapters[2].verbatim.as_deref(),
            Some("Subscribe for part two.")
        );
    }

    #[test]
    fn a_hook_line_left_only_in_the_chapter_is_found_there() {
        let mut extraction = raw(vec![
            ExtractedChapter {
                verbatim: Some("An hour. Every deploy.".into()),
                ..chapter(HookKind, "The hour")
            },
            chapter(Body, "The cache"),
            chapter(CtaKind, "Next time"),
        ]);
        extraction.hook.line = "  ".into();
        let body = clean(extraction).unwrap();
        assert_eq!(body.hook.line, "An hour. Every deploy.");
    }

    #[test]
    fn no_hook_no_cta_or_no_body_is_an_error() {
        let mut no_hook = raw(vec![chapter(Body, "The cache")]);
        no_hook.hook.line.clear();
        assert!(clean(no_hook).unwrap_err().to_string().contains("no hook"));

        let mut no_cta = raw(vec![chapter(Body, "The cache")]);
        no_cta.cta.line.clear();
        assert!(clean(no_cta)
            .unwrap_err()
            .to_string()
            .contains("no call to action"));

        let no_body = raw(vec![
            chapter(HookKind, "The hour"),
            chapter(CtaKind, "Next"),
        ]);
        assert!(clean(no_body).unwrap_err().to_string().contains("no body"));
    }

    #[test]
    fn chapters_are_trimmed_clipped_capped_and_blank_ones_dropped() {
        let mut long = chapter(Body, &"word ".repeat(20));
        long.points = (0..9).map(|i| format!(" point {i} ")).collect();
        long.points.push("   ".into());
        long.layout = Some(ExtractedLayout::Outline);
        long.est_seconds = Some(0);
        let blank = ExtractedChapter {
            points: Vec::new(),
            ..chapter(Body, "  ")
        };
        let many: Vec<_> = (0..20)
            .map(|i| chapter(Body, &format!("Body {i}")))
            .collect();
        let mut chapters = vec![long, blank];
        chapters.extend(many);
        let body = clean(raw(chapters)).unwrap();
        let first = &body.chapters[2];
        assert!(first.title.chars().count() <= MAX_TITLE_CHARS);
        assert!(!first.title.ends_with(' '));
        assert_eq!(first.points.len(), MAX_POINTS);
        assert_eq!(first.points[0], "point 0");
        assert_eq!(first.layout, Some(Pair::Outline));
        assert_eq!(first.est_seconds, None, "zero seconds is no estimate");
        // Hook, outline and CTA are made, plus the body capped.
        assert_eq!(body.chapters.len(), MAX_BODY_CHAPTERS + 3);
        assert!(body.chapters.iter().all(|c| !c.title.trim().is_empty()));
    }

    #[test]
    fn the_prompt_carries_every_source_and_the_refine_base() {
        let sources = Sources {
            brief: Brief::default(),
            instructions: "For engineers. Keep it under ten minutes.".into(),
            typed: "Mention the cache hit rate.".into(),
            takes: vec![(1, "So deploys are slow.".into()), (2, "  ".into())],
            rehearsal: vec![(3, "In this chapter.".into())],
        };
        let base = Plan {
            number: 2,
            body: PlanBody {
                working_title: "Ship it".into(),
                ..PlanBody::default()
            },
            ..Plan::default()
        };
        let refine = Refine {
            base: &base,
            note: "Shorter hook",
        };
        let prompt = user_prompt("Deploys", &sources, Some(&refine)).unwrap();
        assert!(prompt.contains("Video project: Deploys"));
        assert!(prompt.contains("Format: long"), "{prompt}");
        assert!(prompt.contains("Category: none picked yet"), "{prompt}");
        assert!(prompt.contains("Keep it under ten minutes."));
        assert!(prompt.contains("<Idea take 1>\nSo deploys are slow.\n"));
        assert!(
            !prompt.contains("<Idea take 2>"),
            "a silent take is left out"
        );
        assert!(prompt.contains("<Typed idea>\nMention the cache hit rate."));
        assert!(prompt.contains("<Rehearsal chapter 3>\nIn this chapter."));
        assert!(prompt.contains("\"working_title\": \"Ship it\""));
        assert!(prompt.contains("Refine note:\nShorter hook"));
    }

    #[test]
    fn nothing_to_plan_from_is_refused_unless_refining() {
        let empty = Sources {
            instructions: "For engineers.".into(),
            ..Sources::default()
        };
        assert!(user_prompt("Deploys", &empty, None).is_err());
        let base = Plan::default();
        let refine = Refine {
            base: &base,
            note: "",
        };
        let prompt = user_prompt("Deploys", &empty, Some(&refine)).unwrap();
        assert!(prompt.contains("tighten it where it is weakest"));
    }

    /// The format and category are said before the idea: a topic by name,
    /// and a short category with what makes a short one of its kind.
    #[test]
    fn the_prompt_says_the_format_and_the_category() {
        let demos = crate::category::short_category("demos").unwrap();
        let sources = Sources {
            brief: Brief {
                format: crate::sessions::Format::Short,
                category: Some(demos.name.into()),
                definition: Some(demos.definition),
            },
            typed: "The agent files the ticket itself.".into(),
            ..Sources::default()
        };
        let prompt = user_prompt("Tickets", &sources, None).unwrap();
        assert!(
            prompt.contains("Format: short: one vertical video"),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!("Category: Demos. {}", demos.definition)),
            "{prompt}"
        );
        let sources = Sources {
            brief: Brief {
                category: Some("Agents".into()),
                ..Brief::default()
            },
            ..sources
        };
        let prompt = user_prompt("Tickets", &sources, None).unwrap();
        assert!(
            prompt.contains("Format: long: a horizontal video"),
            "{prompt}"
        );
        assert!(prompt.contains("Category: Agents\n"), "{prompt}");
    }

    /// A short is one body chapter: what the model split up is folded into
    /// it in order, it opens on the hook line and ends on the closing line.
    #[test]
    fn a_short_is_one_chapter_that_opens_on_the_hook() {
        let body = clean_short(raw(vec![
            chapter(HookKind, "The claim"),
            ExtractedChapter {
                show: "the ticket queue".into(),
                est_seconds: Some(35),
                ..chapter(Body, "Watch it file")
            },
            ExtractedChapter {
                est_seconds: Some(10),
                ..chapter(CtaKind, "Try it")
            },
        ]))
        .unwrap();
        assert_eq!(kinds(&body), [Body]);
        let short = &body.chapters[0];
        assert_eq!(short.title, "Watch it file");
        assert_eq!(
            short.points,
            ["The claim point", "Watch it file point", "Try it point"]
        );
        assert_eq!(
            short.verbatim.as_deref(),
            Some("Your deploy takes an hour.")
        );
        assert_eq!(short.cues, ["End on: Subscribe for part two."]);
        assert_eq!(short.layout, Some(Pair::Split), "the screen carries it");
        assert_eq!(short.est_seconds, Some(45));
        // Nothing for the render to leave out: no outline, no closing chapter.
        assert!(!body.ends_with_cta());
        let plan = Plan {
            body,
            ..Plan::default()
        };
        assert_eq!(
            plan.to_notes().chapters[0].verbatim.as_deref(),
            Some("Your deploy takes an hour.")
        );
    }

    /// A short needs a hook and something to say; it may end without an ask.
    #[test]
    fn a_short_needs_a_hook_but_not_an_ask() {
        let mut no_ask = raw(vec![chapter(Body, "Watch it file")]);
        no_ask.cta.line.clear();
        let body = clean_short(no_ask).unwrap();
        assert!(body.chapters[0].cues.is_empty());
        assert_eq!(body.cta.line, "");

        let mut no_hook = raw(vec![chapter(Body, "Watch it file")]);
        no_hook.hook.line.clear();
        assert!(clean_short(no_hook)
            .unwrap_err()
            .to_string()
            .contains("no hook"));
        assert!(clean_short(raw(Vec::new()))
            .unwrap_err()
            .to_string()
            .contains("no chapter"));
    }

    /// A card stays on unless the model turned it off, which it is told to do
    /// only when the plan it refines or the note says so.
    #[test]
    fn a_card_the_model_turned_off_stays_off() {
        let body = clean(raw(vec![
            chapter(HookKind, "The hour"),
            chapter(Body, "The cache"),
            ExtractedChapter {
                card: Some(false),
                ..chapter(Body, "More on the cache")
            },
            chapter(CtaKind, "Next time"),
        ]))
        .unwrap();
        let cards: Vec<bool> = body.chapters.iter().map(|c| c.card).collect();
        assert_eq!(cards, [true, true, true, false, true]);
    }

    #[test]
    fn the_schema_names_the_three_kinds_and_layouts() {
        let schema = serde_json::to_string(&schemars::schema_for!(PlanExtraction)).unwrap();
        for word in ["hook", "outline", "body", "cta", "talking-head", "split"] {
            assert!(schema.contains(&format!("\"{word}\"")), "{word} in schema");
        }
    }
}
