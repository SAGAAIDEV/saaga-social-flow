//! `chapter-03.plan.json`: which plan chapter a recorded chapter was recorded
//! for, written beside its media the moment the chapter opens.
//!
//! The teleprompter shows plan chapter *n* while recording chapter *n* is open,
//! so what the author was reading from is a fact about the take, and this file
//! records it. The render reads it back for what HyperFrames draws — the
//! chapter card's title, which chapter is the call to action, the outline's
//! planned points — so a take keeps the plan it was recorded against even
//! after the plan grows a new version.
//!
//! One small file per chapter, beside `chapter-03.layout.json`, for the reason
//! that file gives: `Router::retire_chapter` moves everything named for a
//! chapter into `.discarded/` by prefix, so a retake's old binding goes with
//! its old take without that code learning a new name.
//!
//! The plan version is a reference, not a copy: the chapter is looked up in
//! `plan/vN.json` when it is read, so a title fixed in a draft plan after the
//! take reaches the card. The snapshot kept alongside is what is used if that
//! version cannot be read.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::layouts::Pair;

use super::schema::{ChapterKind, Plan, PlanChapter};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// The plan version, `N` of `plan/vN.json`.
    pub plan: u32,
    /// Whether that version was approved when the chapter opened — a take
    /// recorded against a draft says so.
    #[serde(default)]
    pub approved: bool,
    /// The plan chapter, 1-based. Recording chapter *n* is plan chapter *n*.
    pub chapter: u32,
    pub kind: ChapterKind,
    pub title: String,
    #[serde(default)]
    pub points: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Pair>,
}

pub fn path(session_dir: &Path, n: u32) -> PathBuf {
    session_dir.join(format!("chapter-{n:02}.plan.json"))
}

/// Record that recording chapter `n` opened against `plan`'s chapter `n`.
///
/// A chapter past the plan's end, or opened with no plan at all, is recorded
/// as bound to nothing: any file left from an earlier opening of the same
/// number is removed rather than left to claim a plan chapter this take never
/// showed.
pub fn bind(session_dir: &Path, n: u32, plan: Option<&Plan>) -> Result<()> {
    let target = path(session_dir, n);
    let Some((plan, chapter)) = plan.and_then(|plan| Some((plan, plan.body.chapter(n)?))) else {
        match std::fs::remove_file(&target) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("removing {}", target.display()))
            }
            _ => return Ok(()),
        }
    };
    let binding = Binding {
        plan: plan.number,
        approved: plan.approved,
        chapter: n,
        kind: chapter.kind,
        title: chapter.title.clone(),
        points: chapter.points.clone(),
        layout: chapter.layout,
    };
    let text = serde_json::to_string_pretty(&binding).context("serializing plan binding")?;
    std::fs::write(&target, text + "\n").with_context(|| format!("writing {}", target.display()))
}

pub fn load(session_dir: &Path, n: u32) -> Option<Binding> {
    let text = std::fs::read_to_string(path(session_dir, n)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The plan chapter recording chapter `n` was recorded for, as the plan says
/// it now — or, if that version cannot be read, as it said when the chapter
/// opened. `None` for a chapter recorded without a plan.
pub fn resolve(plan_dir: &Path, session_dir: &Path, n: u32) -> Option<PlanChapter> {
    let binding = load(session_dir, n)?;
    let live = super::load(plan_dir, binding.plan)
        .ok()
        .and_then(|plan| plan.body.chapter(binding.chapter).cloned())
        // A hand edit can reshape a draft; a chapter that changed kind is no
        // longer the one that was on the teleprompter.
        .filter(|chapter| chapter.kind == binding.kind);
    Some(live.unwrap_or_else(|| PlanChapter {
        kind: binding.kind,
        title: binding.title,
        goal: String::new(),
        points: binding.points,
        verbatim: None,
        cues: Vec::new(),
        show: String::new(),
        layout: binding.layout,
        est_seconds: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::schema::PlanBody;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("plan-binding-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn chapter(kind: ChapterKind, title: &str) -> PlanChapter {
        PlanChapter {
            kind,
            title: title.into(),
            goal: String::new(),
            points: vec![format!("{title} point")],
            verbatim: None,
            cues: Vec::new(),
            show: String::new(),
            layout: Some(Pair::Split),
            est_seconds: None,
        }
    }

    fn plan() -> Plan {
        Plan {
            body: PlanBody {
                working_title: "Ship it".into(),
                chapters: vec![
                    chapter(ChapterKind::Hook, "Open"),
                    chapter(ChapterKind::Body, "Middle"),
                    chapter(ChapterKind::Cta, "Close"),
                ],
                ..PlanBody::default()
            },
            ..Plan::default()
        }
    }

    #[test]
    fn a_chapter_is_bound_to_the_plan_chapter_of_its_number() {
        let dir = temp("bind");
        let mut plan = plan();
        plan.number = 4;
        plan.approved = true;
        bind(&dir, 2, Some(&plan)).unwrap();
        let binding = load(&dir, 2).unwrap();
        assert_eq!(binding.plan, 4);
        assert!(binding.approved);
        assert_eq!(binding.chapter, 2);
        assert_eq!(binding.kind, ChapterKind::Body);
        assert_eq!(binding.title, "Middle");
        assert_eq!(binding.layout, Some(Pair::Split));
    }

    /// Past the plan, or with none, nothing is claimed — and an older file for
    /// the same number goes, so it cannot speak for this take.
    #[test]
    fn a_chapter_past_the_plan_is_bound_to_nothing() {
        let dir = temp("past");
        let plan = plan();
        bind(&dir, 3, Some(&plan)).unwrap();
        assert!(path(&dir, 3).is_file());
        bind(&dir, 3, None).unwrap();
        assert!(!path(&dir, 3).exists());
        bind(&dir, 4, Some(&plan)).unwrap();
        assert!(load(&dir, 4).is_none());
    }

    #[test]
    fn the_live_plan_wins_and_the_snapshot_covers_a_missing_version() {
        let root = temp("resolve");
        let plan_dir = root.join("plan");
        let session_dir = root.join("drafts");
        std::fs::create_dir_all(&session_dir).unwrap();
        let saved = crate::plan::save_new(&plan_dir, plan()).unwrap();
        bind(&session_dir, 2, Some(&saved)).unwrap();

        let mut edited = saved.clone();
        edited.body.chapters[1].title = "The real middle".into();
        crate::plan::update(&plan_dir, &edited).unwrap();
        assert_eq!(
            resolve(&plan_dir, &session_dir, 2).unwrap().title,
            "The real middle"
        );

        std::fs::remove_file(plan_dir.join("v1.json")).unwrap();
        let fallback = resolve(&plan_dir, &session_dir, 2).unwrap();
        assert_eq!(fallback.title, "Middle");
        assert_eq!(fallback.points, ["Middle point"]);
        assert!(resolve(&plan_dir, &session_dir, 1).is_none());
    }

    #[test]
    fn a_draft_reshaped_under_the_take_falls_back_to_what_was_shown() {
        let root = temp("reshaped");
        let plan_dir = root.join("plan");
        let session_dir = root.join("drafts");
        std::fs::create_dir_all(&session_dir).unwrap();
        let saved = crate::plan::save_new(&plan_dir, plan()).unwrap();
        bind(&session_dir, 3, Some(&saved)).unwrap();
        let mut edited = saved.clone();
        edited.body.chapters[2].kind = ChapterKind::Body;
        edited.body.chapters[2].title = "Not the ask".into();
        crate::plan::update(&plan_dir, &edited).unwrap();
        let resolved = resolve(&plan_dir, &session_dir, 3).unwrap();
        assert_eq!(resolved.kind, ChapterKind::Cta);
        assert_eq!(resolved.title, "Close");
    }
}
