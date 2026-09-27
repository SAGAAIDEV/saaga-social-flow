//! `plan/vN.json`: one version of the video's plan.
//!
//! Recording chapter *n* is plan chapter *n* — the hook is chapter 1, the CTA
//! the last chapter, the body everything between — because every consumer of
//! `notes.json` reads it by position. [`Plan::to_notes`] is the one place a
//! plan becomes that deck.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::layouts::Pair;
use crate::notes::{Chapter, NotesData};

/// Where a chapter sits in the video's arc. The hook opens it and gets no
/// chapter card (chapter one never has one); the CTA closes it and gets none
/// either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ChapterKind {
    Hook,
    Body,
    Cta,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Hook {
    /// The opening line, as it should be said.
    pub line: String,
    /// Why it holds a viewer: the tension or promise it sets up.
    #[serde(default)]
    pub angle: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cta {
    /// The ask, as it should be said.
    pub line: String,
    /// Where it lands and what earns it.
    #[serde(default)]
    pub placement: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanChapter {
    pub kind: ChapterKind,
    /// 2-5 words. The card title for a body chapter, the section heading on
    /// the blog for every chapter.
    pub title: String,
    /// What the viewer should have understood by the chapter's end.
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub points: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbatim: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<String>,
    /// What is on screen while it is said.
    #[serde(default)]
    pub show: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Pair>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est_seconds: Option<u32>,
}

/// What the model wrote, cleaned: a plan before it has a number.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanBody {
    pub working_title: String,
    #[serde(default)]
    pub audience: String,
    /// What the viewer walks away with.
    #[serde(default)]
    pub promise: String,
    #[serde(default)]
    pub hook: Hook,
    /// The arc in a few beats.
    #[serde(default)]
    pub outline: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<PlanChapter>,
    #[serde(default)]
    pub cta: Cta,
    /// Directions for recording it: setup, what to have open, delivery.
    #[serde(default)]
    pub instructions: Vec<String>,
}

impl PlanBody {
    /// Plan chapter `n`, counted from 1 like recording chapters. Plan and
    /// recording line up by position and nothing else, so this is the whole
    /// of the mapping.
    pub fn chapter(&self, n: u32) -> Option<&PlanChapter> {
        let index = usize::try_from(n.checked_sub(1)?).ok()?;
        self.chapters.get(index)
    }

    /// Whether the plan closes on its call to action. `clean` puts one there on
    /// every build, but a hand-edited version can be any shape, so the render
    /// asks rather than assumes.
    pub fn ends_with_cta(&self) -> bool {
        self.chapters
            .last()
            .is_some_and(|chapter| chapter.kind == ChapterKind::Cta)
    }

    /// What the Record tab says while chapter `n` is recording: "Chapter 3 of
    /// 5 — The fix", or a warning once the take has gone past the plan.
    ///
    /// Past the plan is worth a warning rather than a shrug because of what the
    /// render does with it: it takes the *last recorded* chapter as the call to
    /// action, so an extra chapter break moves the CTA's missing card and
    /// missing short onto whatever is recorded last.
    pub fn position(&self, n: u32) -> String {
        let of = self.chapters.len();
        match self.chapter(n) {
            Some(chapter) if chapter.title.trim().is_empty() => format!("Chapter {n} of {of}"),
            Some(chapter) => format!("Chapter {n} of {of} — {}", chapter.title.trim()),
            None if self.ends_with_cta() => format!(
                "Chapter {n} is past the plan's {of} — the last chapter recorded is \
                 treated as the call to action"
            ),
            None => format!("Chapter {n} is past the plan's {of}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// The N of `vN.json`, shown as "Plan N" — never "vN", which is what
    /// recording versions are called.
    pub number: u32,
    #[serde(flatten)]
    pub body: PlanBody,
    /// Locks this version against edits and refines. At most one version is
    /// approved, and it is the one that writes the deck.
    #[serde(default)]
    pub approved: bool,
    /// The version this one was refined from; `None` for a fresh build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refined_from: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refine_note: String,
    /// What it was built from: "take 01", "typed idea", "rehearsal v1 chapter 02".
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub created_at: String,
}

impl Plan {
    /// The speaking-notes deck: one slide per chapter, in order, so slide *n*
    /// is recording chapter *n*. The hook and CTA chapters say their line
    /// verbatim unless the chapter already has its own wording, and what is
    /// on screen reads as a cue.
    pub fn to_notes(&self) -> NotesData {
        let body = &self.body;
        let chapters = body
            .chapters
            .iter()
            .map(|chapter| {
                let own = chapter
                    .verbatim
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty());
                let line = match chapter.kind {
                    ChapterKind::Hook => Some(body.hook.line.trim()),
                    ChapterKind::Cta => Some(body.cta.line.trim()),
                    ChapterKind::Body => None,
                }
                .filter(|line| !line.is_empty());
                let mut cues = chapter.cues.clone();
                // "camera" is what the model writes for a talking head, and a
                // cue saying so is noise on a slide read at a glance.
                let show = chapter.show.trim();
                if !show.is_empty() && !show.eq_ignore_ascii_case("camera") {
                    cues.push(format!("On screen: {show}"));
                }
                Chapter {
                    title: chapter.title.clone(),
                    points: chapter.points.clone(),
                    verbatim: own.or(line).map(str::to_string),
                    cues,
                }
            })
            .collect();
        NotesData {
            title: body.working_title.clone(),
            version: None,
            chapters,
        }
    }

    /// "Plan 3", or "Plan 3 (approved)".
    pub fn label(&self) -> String {
        if self.approved {
            format!("Plan {} (approved)", self.number)
        } else {
            format!("Plan {}", self.number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(kind: ChapterKind, title: &str) -> PlanChapter {
        PlanChapter {
            kind,
            title: title.into(),
            goal: String::new(),
            points: vec![format!("{title} point")],
            verbatim: None,
            cues: Vec::new(),
            show: String::new(),
            layout: None,
            est_seconds: None,
        }
    }

    fn plan() -> Plan {
        Plan {
            number: 2,
            body: PlanBody {
                working_title: "Ship it".into(),
                hook: Hook {
                    line: "Your deploy takes an hour.".into(),
                    angle: String::new(),
                },
                cta: Cta {
                    line: "Subscribe for part two.".into(),
                    placement: String::new(),
                },
                chapters: vec![
                    chapter(ChapterKind::Hook, "The hour"),
                    chapter(ChapterKind::Body, "The cache"),
                    chapter(ChapterKind::Body, "The fix"),
                    chapter(ChapterKind::Cta, "Next time"),
                ],
                ..PlanBody::default()
            },
            ..Plan::default()
        }
    }

    /// Slide n is recording chapter n: same count, same order, and the hook
    /// and CTA carry their lines word for word.
    #[test]
    fn the_deck_has_one_slide_per_chapter_in_order() {
        let notes = plan().to_notes();
        assert_eq!(notes.title, "Ship it");
        let titles: Vec<_> = notes.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["The hour", "The cache", "The fix", "Next time"]);
        assert_eq!(
            notes.chapters[0].verbatim.as_deref(),
            Some("Your deploy takes an hour.")
        );
        assert_eq!(notes.chapters[1].verbatim, None);
        assert_eq!(
            notes.chapters[3].verbatim.as_deref(),
            Some("Subscribe for part two.")
        );
    }

    #[test]
    fn a_chapters_own_wording_wins_and_what_is_shown_is_a_cue() {
        let mut plan = plan();
        plan.body.chapters[0].verbatim = Some("Sixty minutes. Every deploy.".into());
        plan.body.chapters[1].show = "the build log".into();
        plan.body.chapters[2].show = "Camera".into();
        let notes = plan.to_notes();
        assert_eq!(
            notes.chapters[0].verbatim.as_deref(),
            Some("Sixty minutes. Every deploy.")
        );
        assert_eq!(notes.chapters[1].cues, ["On screen: the build log"]);
        assert!(
            notes.chapters[2].cues.is_empty(),
            "a talking head is no cue"
        );
    }

    /// The Record tab's line: chapter n against the plan's count and title,
    /// and a warning — naming the CTA consequence only when there is a CTA —
    /// once the take goes past it.
    #[test]
    fn the_position_names_the_planned_chapter_and_warns_past_the_plan() {
        let mut plan = plan();
        assert_eq!(plan.body.position(1), "Chapter 1 of 4 — The hour");
        assert_eq!(plan.body.position(4), "Chapter 4 of 4 — Next time");
        let past = plan.body.position(5);
        assert!(past.starts_with("Chapter 5 is past the plan's 4"), "{past}");
        assert!(past.contains("call to action"), "{past}");
        plan.body.chapters[1].title = "  ".into();
        assert_eq!(plan.body.position(2), "Chapter 2 of 4");
        plan.body.chapters.pop();
        assert!(!plan.body.ends_with_cta());
        assert_eq!(plan.body.position(4), "Chapter 4 is past the plan's 3");
        assert!(plan.body.chapter(0).is_none(), "chapters count from one");
    }

    #[test]
    fn a_plan_round_trips_with_its_body_flattened() {
        let mut plan = plan();
        plan.body.chapters[1].layout = Some(Pair::Split);
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["working_title"], "Ship it");
        assert_eq!(json["chapters"][1]["layout"], "split");
        assert_eq!(json["chapters"][0]["kind"], "hook");
        let back: Plan = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
        assert_eq!(back.label(), "Plan 2");
    }
}
