//! The Project and Plan tabs' web panes, drawn from what is on disk.
//!
//! Both are read-only summaries in this phase: the Project pane says where the
//! open project stands — its plan, its recording versions, what is rendered and
//! what is live — and the Plan pane shows the author's input, the idea takes
//! and the selected plan version in full. Each is one function over a
//! [`Session`], so the first draw and every redraw after a project or version
//! change agree, the way [`super::settings_page`] does for Settings.
//!
//! The contexts are plain structs rather than `context!` literals built at the
//! call site: the template is the only reader, and a field renamed on one side
//! shows up as a failing test here instead of a blank cell on screen.

use std::path::Path;

use serde::Serialize;

use crate::plan::schema::ChapterKind;
use crate::plan::{self, Plan, TakeText};
use crate::session::Session;

/// Where the project's plan stands, for the Project pane's first card.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanStatus {
    /// `none`, `draft` or `approved` — the template's tag colour.
    pub state: &'static str,
    /// "Plan 2 approved", "Plan 3 selected — not approved yet", or how to start.
    pub text: String,
    /// The working title of the plan the text names, empty when there is none.
    pub working_title: String,
    /// Chapters in that plan, hook and CTA included.
    pub chapters: usize,
}

/// One recording version on the Project pane.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionRow {
    /// "v2" — recording versions keep their `vN` name; only plans are "Plan N".
    pub label: String,
    pub current: bool,
    /// Closed chapters in `drafts/vN`, the number a plan's chapters line up with.
    pub chapters: usize,
    pub rendered: bool,
    /// What else the version has on disk, in pipeline order: cut, titles, posts…
    pub stages: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Link {
    pub name: &'static str,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectView {
    pub title: String,
    pub folder: String,
    pub root: String,
    pub plan: PlanStatus,
    pub versions: Vec<VersionRow>,
    /// Chapters in the speaking-notes deck, and whether a plan wrote it.
    pub deck: Option<usize>,
    pub deck_from_plan: bool,
    pub links: Vec<Link>,
}

/// The plan's standing in one line. The approved version is named first
/// because it is the one that writes the deck; the selected one only when it
/// is a different version, since that is the case where what is on the Plan
/// tab is not what the teleprompter is reading.
pub fn plan_status(dir: &Path) -> PlanStatus {
    let versions = plan::versions(dir);
    let selected = plan::selected(dir);
    let approved = plan::approved(dir);
    let shown = |plan: Option<&Plan>| {
        plan.map_or((String::new(), 0), |p| {
            (p.body.working_title.clone(), p.body.chapters.len())
        })
    };
    match (approved, selected) {
        (_, None) if versions.is_empty() => PlanStatus {
            state: "none",
            text: "No plan yet — build one on the Plan tab.".into(),
            working_title: String::new(),
            chapters: 0,
        },
        (Some(approved), selected) => {
            let (working_title, chapters) = shown(Some(&approved));
            let text = match selected.filter(|&n| n != approved.number) {
                Some(n) => format!("Plan {} approved · Plan {n} on screen", approved.number),
                None => format!("Plan {} approved", approved.number),
            };
            PlanStatus {
                state: "approved",
                text,
                working_title,
                chapters,
            }
        }
        (None, selected) => {
            let n = selected.or_else(|| versions.last().copied()).unwrap_or(1);
            let (working_title, chapters) = shown(plan::load(dir, n).ok().as_ref());
            PlanStatus {
                state: "draft",
                text: format!("Plan {n} selected — not approved yet"),
                working_title,
                chapters,
            }
        }
    }
}

/// Every recording version with its chapter count. A project from before
/// versioning has no `vN` folders at all; it gets one row for the flat take
/// rather than a table that claims it has recorded nothing.
fn version_rows(session: &Session) -> Vec<VersionRow> {
    let versions = session.list_versions();
    if versions.is_empty() {
        return vec![VersionRow {
            label: "Unversioned".into(),
            current: true,
            chapters: crate::notes::closed_chapter_numbers(&session.dir).len(),
            rendered: session
                .render_dir()
                .join("horizontal/longform.mp4")
                .is_file(),
            stages: Vec::new(),
        }];
    }
    versions
        .iter()
        .map(|v| {
            let stage = |name: &str| session.root.join(name).join(format!("v{}", v.n));
            let stages = [
                (v.has_edit, "cut"),
                (v.has_titles, "titles"),
                (v.has_posts, "posts"),
                (v.has_distribute, "on S3"),
            ]
            .into_iter()
            .filter_map(|(has, name)| has.then_some(name))
            .collect();
            VersionRow {
                label: format!("v{}", v.n),
                current: session.version == Some(v.n),
                chapters: crate::notes::closed_chapter_numbers(&stage("drafts")).len(),
                // The longform, not the folder: a render that failed part way
                // leaves `render/vN` behind with nothing in it worth watching.
                rendered: stage("render").join("horizontal/longform.mp4").is_file(),
                stages,
            }
        })
        .collect()
}

pub fn project_view(session: &Session) -> ProjectView {
    let plan_dir = plan::dir(session);
    // Read without `Session::notes_dir`, which creates the folder: drawing a
    // summary should not leave anything behind on disk.
    let deck = crate::notes::load_notes(&session.root.join("notes"))
        .ok()
        .map(|notes| notes.chapters.len());
    let mut links = Vec::new();
    if let Some(upload) = crate::publish::longform(session) {
        links.push(Link {
            name: "YouTube",
            url: upload.url,
        });
    }
    if let Some(upload) = crate::publish::short(session) {
        links.push(Link {
            name: "Short",
            url: upload.url,
        });
    }
    if let Some(post) = crate::blog::load(session).into_iter().next_back() {
        links.push(Link {
            name: if post.published { "Blog" } else { "Blog draft" },
            url: if post.published {
                post.url
            } else {
                post.admin_url
            },
        });
    }
    ProjectView {
        title: session.title(),
        folder: session.folder(),
        root: session.root.display().to_string(),
        plan: plan_status(&plan_dir),
        versions: version_rows(session),
        deck,
        deck_from_plan: plan::deck_from_plan(&plan_dir),
        links,
    }
}

/// The Project tab's pane.
pub fn project_page(session: &Session) -> String {
    super::render::page("project.html", project_view(session))
}

/// One idea take on the Plan pane.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TakeRow {
    pub label: String,
    /// `ready`, `transcribing` or `nothing`.
    pub state: &'static str,
    pub text: String,
    /// Why a finished take gave nothing — silent, skipped, failed.
    pub why: String,
}

/// An entry in the Plan pane's version list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanVersionItem {
    pub n: u32,
    /// "Plan N", never "vN" — that is what recording versions are called.
    pub label: String,
    pub selected: bool,
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChapterView {
    pub n: usize,
    /// `hook`, `body` or `cta`.
    pub kind: &'static str,
    pub kind_label: &'static str,
    pub title: String,
    pub goal: String,
    pub points: Vec<String>,
    pub verbatim: Option<String>,
    pub cues: Vec<String>,
    pub show: String,
    pub layout: Option<&'static str>,
    /// "0:45", from the plan's estimate.
    pub est: Option<String>,
}

/// The selected plan version, flattened for the template.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanShown {
    /// "Plan 2" or "Plan 2 (approved)".
    pub label: String,
    pub approved: bool,
    pub working_title: String,
    pub audience: String,
    pub promise: String,
    pub hook_line: String,
    pub hook_angle: String,
    pub outline: Vec<String>,
    pub chapters: Vec<ChapterView>,
    pub cta_line: String,
    pub cta_placement: String,
    pub instructions: Vec<String>,
    /// "Plan 1", when this version was refined from another.
    pub refined_from: Option<String>,
    pub refine_note: String,
    pub sources: Vec<String>,
    pub created_at: String,
    /// The whole length the chapter estimates add up to, when any have one.
    pub est_total: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanView {
    pub instructions: String,
    pub typed: String,
    pub takes: Vec<TakeRow>,
    pub versions: Vec<PlanVersionItem>,
    pub plan: Option<PlanShown>,
    /// Why the selected version could not be shown, when it could not.
    pub unreadable: Option<String>,
}

/// `45` → `0:45`, `125` → `2:05`.
fn mmss(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn chapter_view(n: usize, chapter: &crate::plan::schema::PlanChapter) -> ChapterView {
    let (kind, kind_label) = match chapter.kind {
        ChapterKind::Hook => ("hook", "Hook"),
        ChapterKind::Body => ("body", "Body"),
        ChapterKind::Cta => ("cta", "Call to action"),
    };
    ChapterView {
        n,
        kind,
        kind_label,
        title: chapter.title.clone(),
        goal: chapter.goal.clone(),
        points: chapter.points.clone(),
        verbatim: chapter
            .verbatim
            .clone()
            .filter(|line| !line.trim().is_empty()),
        cues: chapter.cues.clone(),
        show: chapter.show.clone(),
        layout: chapter.layout.map(crate::layouts::Pair::as_str),
        est: chapter.est_seconds.map(mmss),
    }
}

pub fn plan_shown(plan: &Plan) -> PlanShown {
    let body = &plan.body;
    let estimates: Vec<u32> = body
        .chapters
        .iter()
        .filter_map(|chapter| chapter.est_seconds)
        .collect();
    PlanShown {
        label: plan.label(),
        approved: plan.approved,
        working_title: body.working_title.clone(),
        audience: body.audience.clone(),
        promise: body.promise.clone(),
        hook_line: body.hook.line.clone(),
        hook_angle: body.hook.angle.clone(),
        outline: body.outline.clone(),
        chapters: body
            .chapters
            .iter()
            .enumerate()
            .map(|(i, chapter)| chapter_view(i + 1, chapter))
            .collect(),
        cta_line: body.cta.line.clone(),
        cta_placement: body.cta.placement.clone(),
        instructions: body.instructions.clone(),
        refined_from: plan.refined_from.map(|n| format!("Plan {n}")),
        refine_note: plan.refine_note.clone(),
        sources: plan.sources.clone(),
        created_at: plan.created_at.clone(),
        est_total: (!estimates.is_empty()).then(|| mmss(estimates.iter().sum())),
    }
}

pub fn plan_view(session: &Session) -> PlanView {
    let dir = plan::dir(session);
    let input = plan::load_input(&dir);
    let takes = plan::takes(&dir)
        .iter()
        .map(|take| {
            let label = format!("Take {:02}", take.n);
            match plan::take_text(take) {
                TakeText::Words(text) => TakeRow {
                    label,
                    state: "ready",
                    text,
                    why: String::new(),
                },
                TakeText::Transcribing => TakeRow {
                    label,
                    state: "transcribing",
                    text: String::new(),
                    why: String::new(),
                },
                TakeText::Nothing(why) => TakeRow {
                    label,
                    state: "nothing",
                    text: String::new(),
                    why,
                },
            }
        })
        .collect();
    let selected = plan::selected(&dir);
    let loaded: Vec<(u32, Option<Plan>)> = plan::versions(&dir)
        .into_iter()
        .map(|n| (n, plan::load(&dir, n).ok()))
        .collect();
    let versions = loaded
        .iter()
        .map(|(n, plan)| PlanVersionItem {
            n: *n,
            label: format!("Plan {n}"),
            selected: selected == Some(*n),
            approved: plan.as_ref().is_some_and(|p| p.approved),
        })
        .collect();
    let (plan, unreadable) = match selected {
        None => (None, None),
        Some(n) => match plan::load(&dir, n) {
            Ok(plan) => (Some(plan_shown(&plan)), None),
            Err(err) => (None, Some(format!("Plan {n} could not be read: {err:#}"))),
        },
    };
    PlanView {
        instructions: input.instructions,
        typed: input.typed,
        takes,
        versions,
        plan,
        unreadable,
    }
}

/// The Plan tab's pane.
pub fn plan_page(session: &Session) -> String {
    super::render::page("plan.html", plan_view(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::schema::{Cta, Hook, PlanBody, PlanChapter};

    fn project(tag: &str) -> Session {
        let root = std::env::temp_dir().join(format!("ui-planning-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("drafts/v1")).unwrap();
        Session::open_root(root).unwrap()
    }

    fn chapter(kind: ChapterKind, title: &str) -> PlanChapter {
        PlanChapter {
            kind,
            title: title.into(),
            goal: format!("{title} goal"),
            points: vec![format!("{title} point")],
            verbatim: None,
            cues: Vec::new(),
            show: String::new(),
            layout: None,
            est_seconds: Some(40),
        }
    }

    fn plan(title: &str) -> Plan {
        Plan {
            body: PlanBody {
                working_title: title.into(),
                audience: "Engineers who own a deploy".into(),
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
                    chapter(ChapterKind::Cta, "Next time"),
                ],
                ..PlanBody::default()
            },
            ..Plan::default()
        }
    }

    #[test]
    fn a_project_with_no_plan_says_how_to_start_one() {
        let session = project("no-plan");
        let view = project_view(&session);
        assert_eq!(view.plan.state, "none");
        assert!(view.plan.text.contains("Plan tab"));
        assert_eq!(view.deck, None);
        assert!(view.links.is_empty());
        let html = project_page(&session);
        assert!(!html.contains("template error"), "{html}");
    }

    /// The approved version is the one the teleprompter reads, so it leads;
    /// a different version on screen is said beside it, not instead.
    #[test]
    fn the_plan_status_names_the_approved_version_first() {
        let session = project("status");
        let dir = plan::dir(&session);
        plan::save_new(&dir, plan("One")).unwrap();
        assert_eq!(plan_status(&dir).text, "Plan 1 selected — not approved yet");
        plan::save_new(&dir, plan("Two")).unwrap();
        plan::approve(&dir, 1, &session.root.join("notes")).unwrap();
        let status = plan_status(&dir);
        assert_eq!(status.state, "approved");
        assert_eq!(status.text, "Plan 1 approved · Plan 2 on screen");
        assert_eq!(status.working_title, "One");
        assert_eq!(status.chapters, 3);
        plan::select(&dir, 1).unwrap();
        assert_eq!(plan_status(&dir).text, "Plan 1 approved");
        let view = project_view(&session);
        assert_eq!(view.deck, Some(3));
        assert!(view.deck_from_plan);
    }

    #[test]
    fn versions_are_listed_with_their_chapter_counts() {
        let session = project("versions");
        for name in ["chapter-01.mp4", "chapter-02.mp4"] {
            std::fs::write(session.root.join("drafts/v1").join(name), "").unwrap();
        }
        std::fs::create_dir_all(session.root.join("drafts/v2")).unwrap();
        let rows = version_rows(&session.open_version(2).unwrap());
        let summary: Vec<_> = rows
            .iter()
            .map(|r| (r.label.as_str(), r.chapters, r.current))
            .collect();
        assert_eq!(summary, [("v1", 2, false), ("v2", 0, true)]);
        assert!(rows.iter().all(|r| !r.rendered));
    }

    #[test]
    fn the_plan_view_reads_takes_input_and_the_selected_version() {
        let session = project("plan-view");
        let dir = plan::dir(&session);
        let empty = plan_view(&session);
        assert!(empty.plan.is_none() && empty.versions.is_empty() && empty.takes.is_empty());

        plan::save_input(
            &dir,
            &plan::Input {
                instructions: "Keep it under five minutes.".into(),
                typed: String::new(),
            },
        )
        .unwrap();
        std::fs::write(dir.join("take-01.m4a"), "").unwrap();
        std::fs::write(
            dir.join("take-01.transcript.json"),
            r#"{"status":"completed","text":"Deploys are slow."}"#,
        )
        .unwrap();
        std::fs::write(dir.join("take-02.m4a"), "").unwrap();
        plan::save_new(&dir, plan("One")).unwrap();
        plan::save_new(&dir, plan("Two")).unwrap();
        plan::approve(&dir, 2, &session.root.join("notes")).unwrap();

        let view = plan_view(&session);
        assert_eq!(view.instructions, "Keep it under five minutes.");
        let takes: Vec<_> = view
            .takes
            .iter()
            .map(|t| (t.label.as_str(), t.state))
            .collect();
        assert_eq!(takes, [("Take 01", "ready"), ("Take 02", "transcribing")]);
        let labels: Vec<_> = view.versions.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["Plan 1", "Plan 2"]);
        assert!(view.versions[1].approved && view.versions[1].selected);
        let shown = view.plan.expect("the selected plan");
        assert_eq!(shown.label, "Plan 2 (approved)");
        assert_eq!(shown.chapters[2].kind_label, "Call to action");
        assert_eq!(shown.chapters[0].est.as_deref(), Some("0:40"));
        assert_eq!(shown.est_total.as_deref(), Some("2:00"));
    }

    #[test]
    fn estimates_read_as_minutes_and_seconds() {
        assert_eq!(mmss(45), "0:45");
        assert_eq!(mmss(125), "2:05");
    }
}
