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
    // The Plan tab's version picker (phase 3).
    #[allow(dead_code)]
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
