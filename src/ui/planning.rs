//! The Project and Plan tabs' web panes, drawn from what is on disk.
//!
//! The Project pane is a read-only summary of where the open project stands —
//! its plan, its recording versions, what is rendered and what is live. The
//! Plan pane is where the work happens: idea takes, the author's input, the
//! build buttons and the selected plan version as editable boxes, all posted
//! back through `ui::web`. Each is one function over a
//! [`Session`], so the first draw and every redraw after a project or version
//! change agree, the way [`super::settings_page`] does for Settings.
//!
//! The contexts are plain structs rather than `context!` literals built at the
//! call site: the template is the only reader, and a field renamed on one side
//! shows up as a failing test here instead of a blank cell on screen.

use std::collections::BTreeMap;
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
    /// The Category card. Filled by the app, which holds the team template;
    /// `None` in a view built from disk alone, and the card is left out.
    pub category: Option<CategoryView>,
}

/// One option in the Project tab's category dropdown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CategoryOption {
    pub slug: String,
    pub label: String,
    pub selected: bool,
}

/// The project's category, as the card spells out what it drives.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentCategory {
    pub slug: String,
    pub name: String,
    /// The playlist on YouTube, empty when there is none yet.
    pub playlist_url: String,
    /// As the box shows them: `#SEO #AIAgents`.
    pub hashtags: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CategoryView {
    pub options: Vec<CategoryOption>,
    pub current: Option<CurrentCategory>,
    /// The standing blog default a project with no category still files under.
    pub fallback: Option<String>,
    /// A set-up is running: the buttons are off.
    pub busy: bool,
    /// The team template came from S3, so a set-up can save to it.
    pub can_save: bool,
    pub status: String,
}

/// The Category card for the project at `root`.
///
/// The options are the Strapi categories as last read, plus the project's own
/// when the list does not have it — set up by a teammate since this Mac read
/// the list — so the dropdown never shows a different category from the one
/// the project is filed under.
pub fn category_view(
    root: &Path,
    library: &crate::blog::library::Library,
    team: &crate::team::TeamTemplate,
    fallback: Option<String>,
    busy: bool,
    can_save: bool,
    status: &str,
) -> CategoryView {
    let choice = crate::category::load(root);
    let selected = choice.as_ref().map(|choice| choice.slug.as_str());
    let mut options: Vec<CategoryOption> = std::iter::once(CategoryOption {
        slug: String::new(),
        label: "— none —".into(),
        selected: selected.is_none(),
    })
    .chain(library.categories.iter().map(|entry| CategoryOption {
        slug: entry.slug.clone(),
        label: entry.name.clone(),
        selected: selected == Some(entry.slug.as_str()),
    }))
    .collect();
    if let Some(choice) = &choice {
        if !options.iter().any(|option| option.selected) {
            options.push(CategoryOption {
                slug: choice.slug.clone(),
                label: choice.name.clone(),
                selected: true,
            });
        }
    }
    let current = crate::category::team_entry(root, team).map(|category| CurrentCategory {
        playlist_url: match category.playlist_id.as_str() {
            "" => String::new(),
            id => format!("https://www.youtube.com/playlist?list={id}"),
        },
        hashtags: category.hashtags.join(" "),
        slug: category.slug,
        name: category.name,
    });
    CategoryView {
        options,
        fallback: fallback.filter(|_| current.is_none()),
        current,
        busy,
        can_save,
        status: status.to_string(),
    }
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
        category: None,
    }
}

/// The Project tab's pane.
pub fn project_page(session: &Session, category: CategoryView) -> String {
    super::render::page(
        "project.html",
        ProjectView {
            category: Some(category),
            ..project_view(session)
        },
    )
}

/// What the Plan pane shows that is not on disk: the idea take being
/// recorded, and whether a plan is being built. Both lock parts of the page —
/// see [`PlanView::locked`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlanLive {
    pub recording: Option<u32>,
    pub building: bool,
}

/// One idea take on the Plan pane.
///
/// The tag, its tone and the buttons are decided here rather than in the
/// template, because the pane's script repaints a single row with them when a
/// transcript lands — see `App::poll_plan_takes` — and a row drawn by the page
/// and a row patched by the script must not be able to disagree.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TakeRow {
    pub n: u32,
    pub label: String,
    /// `recording`, `ready`, `transcribing`, `nothing` (finished with no
    /// words: silent, skipped or failed) or `orphaned` (no job and no
    /// transcript — nothing is going to finish it).
    pub state: &'static str,
    pub tag: &'static str,
    /// `ok`, `warn`, `bad` or empty — the tag's colour.
    pub tone: &'static str,
    pub text: String,
    /// Why a finished take gave nothing — silent, skipped, failed.
    pub why: String,
    /// A failed or orphaned take can be sent again. A silent one cannot: the
    /// job would hear the same silence.
    pub retry: bool,
    /// Not while it records, and not while its transcript is being written —
    /// see [`plan::delete_take`].
    pub can_delete: bool,
}

pub fn take_row(take: &plan::Take, recording: Option<u32>) -> TakeRow {
    let row = |state, tag, tone| TakeRow {
        n: take.n,
        label: format!("Take {:02}", take.n),
        state,
        tag,
        tone,
        text: String::new(),
        why: String::new(),
        retry: false,
        can_delete: true,
    };
    if recording == Some(take.n) {
        return TakeRow {
            can_delete: false,
            ..row("recording", "Recording…", "bad")
        };
    }
    match plan::take_text(take) {
        TakeText::Words(text) => TakeRow {
            text,
            ..row("ready", "Transcribed", "ok")
        },
        TakeText::Transcribing => match plan::take_job(take) {
            plan::TakeJob::Running(_) => TakeRow {
                can_delete: false,
                ..row("transcribing", "Transcribing…", "")
            },
            plan::TakeJob::Finished | plan::TakeJob::Orphaned => TakeRow {
                why: plan::ORPHANED.into(),
                retry: true,
                ..row("orphaned", "Not transcribed", "bad")
            },
        },
        TakeText::Nothing(why) => {
            let status = crate::notes::load_transcript_at(&take.audio).map(|t| t.status);
            let (tag, tone, retry) = match status {
                Some(crate::notes::TranscriptStatus::Completed) => ("No speech", "warn", false),
                Some(crate::notes::TranscriptStatus::Skipped) => ("Skipped", "warn", true),
                _ => ("Failed", "bad", true),
            };
            TakeRow {
                why,
                retry,
                ..row("nothing", tag, tone)
            }
        }
    }
}

pub fn take_rows(dir: &Path, recording: Option<u32>) -> Vec<TakeRow> {
    plan::takes(dir)
        .iter()
        .map(|take| take_row(take, recording))
        .collect()
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
    /// The layout's wire name for the picker — `talking-head`, `split`,
    /// `outline` — or empty for none suggested.
    pub layout_key: String,
    /// "0:45", from the plan's estimate.
    pub est: Option<String>,
}

/// One entry in a chapter's layout picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayoutOption {
    pub key: String,
    pub label: &'static str,
}

/// The selected plan version, flattened for the template.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanShown {
    pub n: u32,
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
    /// The project the page is drawn for. Every message the page sends
    /// carries it back, so the app can ignore one from a replaced page.
    pub root: String,
    pub instructions: String,
    pub typed: String,
    pub takes: Vec<TakeRow>,
    /// The take being recorded, by number, which turns Record idea into Stop.
    pub recording: Option<u32>,
    /// "Stop take 03" — the record button's label while a take is running.
    pub recording_label: Option<String>,
    pub building: bool,
    pub versions: Vec<PlanVersionItem>,
    pub plan: Option<PlanShown>,
    /// Why the selected version could not be shown, when it could not.
    pub unreadable: Option<String>,
    /// Why Build, Refine and Plan from rehearsal are switched off, when they
    /// are — said on the page beside them, since a greyed button explains
    /// nothing.
    pub locked: Option<String>,
    /// Whether this recording version has chapters to plan from.
    pub can_rehearse: bool,
    pub layouts: Vec<LayoutOption>,
}

/// `45` → `0:45`, `125` → `2:05`.
fn mmss(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `0:45`, `2:05` or a bare `90` back to seconds; `None` for anything else,
/// including a box half-way through being typed.
fn parse_mmss(text: &str) -> Option<u32> {
    let text = text.trim();
    match text.split_once(':') {
        Some((m, s)) if s.len() == 2 => {
            let (m, s): (u32, u32) = (m.trim().parse().ok()?, s.parse().ok()?);
            (s < 60).then_some(m * 60 + s)
        }
        Some(_) => None,
        None => text.parse().ok(),
    }
}

/// A pair's wire name, `talking-head`, as serde writes it into the plan.
fn pair_key(pair: crate::layouts::Pair) -> String {
    serde_json::to_value(pair)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn layout_options() -> Vec<LayoutOption> {
    crate::layouts::Pair::ALL
        .iter()
        .map(|&pair| LayoutOption {
            key: pair_key(pair),
            label: pair.as_str(),
        })
        .collect()
}

fn chapter_view(n: usize, chapter: &crate::plan::schema::PlanChapter) -> ChapterView {
    let (kind, kind_label) = match chapter.kind {
        ChapterKind::Hook => ("hook", "Hook"),
        ChapterKind::Outline => ("outline", "Outline"),
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
        layout_key: chapter.layout.map(pair_key).unwrap_or_default(),
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
        n: plan.number,
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

/// A textarea of one item per line, with the blank lines dropped.
fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// The plan's boxes, as the page posts them, over the version they were
/// drawn from.
///
/// Keys are the plan's own field names — `working_title`, `hook_line`,
/// `outline` — and `ch{n}.title`, `ch{n}.points` and so on for chapter *n*.
/// A key that is absent leaves that field as it was, so a page that draws
/// fewer boxes cannot blank what it did not show. Lists are one item per line.
/// A chapter's kind and the plan's number, approval and sources are never
/// taken from the page: the shape is the model's and `clean`'s, and the lock
/// is [`plan::update`]'s.
pub fn plan_from_fields(base: &Plan, fields: &BTreeMap<String, String>) -> Plan {
    let mut plan = base.clone();
    let text = |key: &str, into: &mut String| {
        if let Some(value) = fields.get(key) {
            *into = value.trim().to_string();
        }
    };
    let list = |key: &str, into: &mut Vec<String>| {
        if let Some(value) = fields.get(key) {
            *into = lines(value);
        }
    };
    let body = &mut plan.body;
    text("working_title", &mut body.working_title);
    text("audience", &mut body.audience);
    text("promise", &mut body.promise);
    text("hook_line", &mut body.hook.line);
    text("hook_angle", &mut body.hook.angle);
    list("outline", &mut body.outline);
    text("cta_line", &mut body.cta.line);
    text("cta_placement", &mut body.cta.placement);
    list("instructions", &mut body.instructions);
    for (i, chapter) in body.chapters.iter_mut().enumerate() {
        let key = |name: &str| format!("ch{}.{name}", i + 1);
        text(&key("title"), &mut chapter.title);
        text(&key("goal"), &mut chapter.goal);
        list(&key("points"), &mut chapter.points);
        list(&key("cues"), &mut chapter.cues);
        text(&key("show"), &mut chapter.show);
        if let Some(value) = fields.get(&key("verbatim")) {
            let value = value.trim();
            chapter.verbatim = (!value.is_empty()).then(|| value.to_string());
        }
        if let Some(value) = fields.get(&key("layout")) {
            chapter.layout = serde_json::from_value(serde_json::Value::String(value.clone())).ok();
        }
        if let Some(value) = fields.get(&key("est")) {
            // Blank clears the estimate; anything unreadable — "1:" on the way
            // to "1:30" — keeps the last one rather than dropping it.
            if value.trim().is_empty() {
                chapter.est_seconds = None;
            } else if let Some(seconds) = parse_mmss(value) {
                chapter.est_seconds = Some(seconds);
            }
        }
    }
    plan
}

/// Why the build buttons are off, or `None` when they are on. A running build
/// comes first because it is the one that clears on its own.
fn locked(live: PlanLive, selected: Option<&Plan>) -> Option<String> {
    if live.building {
        return Some("A plan is being built — progress is on the line above.".into());
    }
    if let Some(n) = live.recording {
        return Some(format!(
            "Take {n:02} is recording — stop it first, or it would be left out of the plan."
        ));
    }
    selected.filter(|plan| plan.approved).map(|plan| {
        format!(
            "Plan {} is approved and locked — un-approve it to edit it, refine it or build again.",
            plan.number
        )
    })
}

pub fn plan_view(session: &Session, live: PlanLive) -> PlanView {
    let dir = plan::dir(session);
    let input = plan::load_input(&dir);
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
    let chosen = selected.map(|n| (n, plan::load(&dir, n)));
    let (plan, unreadable) = match &chosen {
        None => (None, None),
        Some((_, Ok(plan))) => (Some(plan_shown(plan)), None),
        Some((n, Err(err))) => (None, Some(format!("Plan {n} could not be read: {err:#}"))),
    };
    let chosen = chosen.and_then(|(_, plan)| plan.ok());
    PlanView {
        root: session.root.display().to_string(),
        instructions: input.instructions,
        typed: input.typed,
        takes: take_rows(&dir, live.recording),
        recording: live.recording,
        recording_label: live.recording.map(|n| format!("Stop take {n:02}")),
        building: live.building,
        versions,
        plan,
        unreadable,
        locked: locked(live, chosen.as_ref()),
        can_rehearse: can_rehearse(session),
        layouts: layout_options(),
    }
}

/// Whether "Plan from rehearsal" has anything to plan from: chapters closed in
/// the open recording version. Its own function because the app compares it
/// with what the pane last drew, to repaint when the Record tab changes it.
pub fn can_rehearse(session: &Session) -> bool {
    !crate::notes::closed_chapter_numbers(&session.dir).is_empty()
}

/// The Record tab's teleprompter: the plan the recording follows, one slide
/// per chapter, opening on chapter `chapter` — recording, or up next. Later
/// moves go through [`crate::notes::NotesPane::go_to_chapter`].
pub fn teleprompter_page(plan: &Plan, chapter: u32, recording: bool) -> String {
    #[derive(Serialize)]
    struct Page {
        plan: PlanShown,
        chapter: u32,
        recording: bool,
    }
    super::render::page(
        "teleprompter.html",
        Page {
            plan: plan_shown(plan),
            chapter: chapter.max(1),
            recording,
        },
    )
}

/// The Plan tab's pane, from a view already read — the app keeps the take
/// rows it was drawn with, to patch them as transcripts land.
pub fn plan_page(view: &PlanView) -> String {
    super::render::page("plan.html", view)
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

    /// The dropdown shows the category the project is filed under even when
    /// this Mac's Strapi list has not read it yet, and the card spells out
    /// the playlist and the hashtags the team saved for it.
    #[test]
    fn the_category_card_shows_what_the_category_drives() {
        let session = project("category");
        let library = crate::blog::library::Library {
            categories: vec![crate::blog::library::Entry {
                id: 14,
                name: "GTM".into(),
                slug: "gtm".into(),
                detail: None,
            }],
            ..Default::default()
        };
        let team = crate::team::TeamTemplate::default();
        let none = category_view(
            &session.root,
            &library,
            &team,
            Some("AI Literacy".into()),
            false,
            true,
            "",
        );
        assert!(none.options[0].selected && none.current.is_none());
        assert_eq!(none.fallback.as_deref(), Some("AI Literacy"));

        crate::category::save(
            &session.root,
            Some(&crate::category::Choice {
                slug: "seo-agents".into(),
                name: "SEO Agents".into(),
            }),
        )
        .unwrap();
        let team = team.with_category(crate::team::Category {
            slug: "seo-agents".into(),
            name: "SEO Agents".into(),
            playlist_id: "PL7".into(),
            hashtags: vec!["#SEO".into(), "#AIAgents".into()],
        });
        let view = category_view(
            &session.root,
            &library,
            &team,
            Some("AI Literacy".into()),
            false,
            true,
            "",
        );
        let picked: Vec<_> = view.options.iter().filter(|o| o.selected).collect();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].slug, "seo-agents");
        assert_eq!(view.fallback, None);
        let current = view.current.unwrap();
        assert_eq!(
            current.playlist_url,
            "https://www.youtube.com/playlist?list=PL7"
        );
        assert_eq!(current.hashtags, "#SEO #AIAgents");
    }

    #[test]
    fn a_project_with_no_plan_says_how_to_start_one() {
        let session = project("no-plan");
        let view = project_view(&session);
        assert_eq!(view.plan.state, "none");
        assert!(view.plan.text.contains("Plan tab"));
        assert_eq!(view.deck, None);
        assert!(view.links.is_empty());
        let html = project_page(
            &session,
            category_view(
                &session.root,
                &crate::blog::library::Library::default(),
                &crate::team::TeamTemplate::default(),
                None,
                false,
                true,
                "",
            ),
        );
        assert!(html.contains("category-new"), "{html}");
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
        let empty = plan_view(&session, PlanLive::default());
        assert!(empty.plan.is_none() && empty.versions.is_empty() && empty.takes.is_empty());
        assert_eq!(empty.locked, None);
        assert!(!empty.can_rehearse, "no chapters recorded yet");

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

        let view = plan_view(&session, PlanLive::default());
        assert_eq!(view.instructions, "Keep it under five minutes.");
        assert_eq!(view.root, session.root.display().to_string());
        let takes: Vec<_> = view
            .takes
            .iter()
            .map(|t| (t.label.as_str(), t.state))
            .collect();
        // Take 02 has no transcript and no job behind it: nothing will finish
        // it, so it is offered for a retry rather than shown as in progress.
        assert_eq!(takes, [("Take 01", "ready"), ("Take 02", "orphaned")]);
        assert!(view.takes[1].retry && !view.takes[0].retry);
        let labels: Vec<_> = view.versions.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["Plan 1", "Plan 2"]);
        assert!(view.versions[1].approved && view.versions[1].selected);
        let shown = view.plan.expect("the selected plan");
        assert_eq!(shown.label, "Plan 2 (approved)");
        assert_eq!(shown.n, 2);
        assert_eq!(shown.chapters[2].kind_label, "Call to action");
        assert_eq!(shown.chapters[0].est.as_deref(), Some("0:40"));
        assert_eq!(shown.est_total.as_deref(), Some("2:00"));
        assert!(view
            .locked
            .expect("approved locks")
            .contains("Plan 2 is approved"));
    }

    /// Each take state has its own tag and buttons, and they come from one
    /// place, since the pane's script patches a row with the same fields.
    #[test]
    fn take_rows_say_what_can_be_done_with_each_take() {
        let session = project("take-rows");
        let dir = plan::dir(&session);
        std::fs::create_dir_all(&dir).unwrap();
        for n in 1..=5 {
            std::fs::write(plan::take_path(&dir, n), "").unwrap();
        }
        let transcript = |n: u32, body: &str| {
            std::fs::write(dir.join(format!("take-{n:02}.transcript.json")), body).unwrap()
        };
        transcript(1, r#"{"status":"completed","text":""}"#);
        transcript(2, r#"{"status":"error","error":"upload timed out"}"#);
        transcript(3, r#"{"status":"processing"}"#);
        let busy = crate::notes::hold_running_for_test(&dir.join("take-03.transcript.json"))
            .expect("free");

        let rows = take_rows(&dir, Some(5));
        let summary: Vec<_> = rows
            .iter()
            .map(|r| (r.state, r.tag, r.retry, r.can_delete))
            .collect();
        assert_eq!(
            summary,
            [
                ("nothing", "No speech", false, true),
                ("nothing", "Failed", true, true),
                ("transcribing", "Transcribing…", false, false),
                ("orphaned", "Not transcribed", true, true),
                ("recording", "Recording…", false, false),
            ]
        );
        assert_eq!(rows[1].why, "upload timed out");
        drop(busy);
    }

    /// The page's states: the record button follows the take, and every
    /// reason the build buttons are off is said beside them.
    #[test]
    fn the_plan_page_draws_its_live_states() {
        let session = project("plan-page");
        let dir = plan::dir(&session);
        std::fs::create_dir_all(&dir).unwrap();
        let idle = plan_page(&plan_view(&session, PlanLive::default()));
        assert!(!idle.contains("template error"), "{idle}");
        assert!(idle.contains("Record idea"));
        assert!(idle.contains(r#""type":"planRecordToggle""#));
        assert!(idle.contains("Build plan"));

        std::fs::write(dir.join("take-01.m4a"), "").unwrap();
        let recording = plan_page(&plan_view(
            &session,
            PlanLive {
                recording: Some(1),
                building: false,
            },
        ));
        assert!(recording.contains("Stop take 01"), "{recording}");
        assert!(recording.contains("stop it first"));

        plan::save_new(&dir, plan("One")).unwrap();
        let draft = plan_page(&plan_view(&session, PlanLive::default()));
        assert!(draft.contains("Refine Plan 1"));
        assert!(draft.contains(r#"data-field="ch2.title""#));
        assert!(!draft.contains("readonly"), "a draft is editable");
        assert!(draft.contains("Go to recording"));

        let building = plan_page(&plan_view(
            &session,
            PlanLive {
                recording: None,
                building: true,
            },
        ));
        assert!(building.contains("A plan is being built"));

        plan::approve(&dir, 1, &session.root.join("notes")).unwrap();
        let approved = plan_page(&plan_view(&session, PlanLive::default()));
        assert!(approved.contains("readonly"), "an approved plan is locked");
        assert!(approved.contains("Un-approve"));
        assert!(approved.contains("un-approve it to edit it"));
        assert!(!approved.contains(">v1<"), "plans are never called vN");
    }

    #[test]
    fn hand_edits_land_on_their_fields_and_leave_the_rest_alone() {
        let base = plan("One");
        let mut fields = BTreeMap::new();
        for (key, value) in [
            ("working_title", "  Faster deploys "),
            ("outline", "Slow\n\n  Cache  \nFast\n"),
            ("ch2.title", "The cache"),
            ("ch2.points", "Hit rate\nEviction"),
            ("ch2.verbatim", "   "),
            ("ch2.layout", "split"),
            ("ch2.est", "1:30"),
            ("ch3.est", "1:"),
            ("ch1.est", ""),
            // Never taken from the page.
            ("number", "9"),
            ("approved", "true"),
        ] {
            fields.insert(key.to_string(), value.to_string());
        }
        let edited = plan_from_fields(&base, &fields);
        assert_eq!(edited.body.working_title, "Faster deploys");
        assert_eq!(edited.body.outline, ["Slow", "Cache", "Fast"]);
        let two = &edited.body.chapters[1];
        assert_eq!(two.points, ["Hit rate", "Eviction"]);
        assert_eq!(two.verbatim, None);
        assert_eq!(two.layout, Some(crate::layouts::Pair::Split));
        assert_eq!(two.est_seconds, Some(90));
        assert_eq!(two.kind, ChapterKind::Body);
        // Half-typed keeps the last estimate; blank clears it.
        assert_eq!(edited.body.chapters[2].est_seconds, Some(40));
        assert_eq!(edited.body.chapters[0].est_seconds, None);
        // Absent keys are untouched.
        assert_eq!(edited.body.audience, base.body.audience);
        assert_eq!(edited.body.hook, base.body.hook);
        assert_eq!(
            (edited.number, edited.approved),
            (base.number, base.approved)
        );
    }

    /// The lock as the pane meets it: an edit posted to an approved version
    /// is refused and changes nothing, un-approving lets the same edit land,
    /// and the deck the approval wrote stays as it was.
    #[test]
    fn a_page_edit_to_an_approved_plan_is_refused_until_it_is_unapproved() {
        let session = project("edit-lock");
        let dir = plan::dir(&session);
        let notes = session.root.join("notes");
        plan::save_new(&dir, plan("One")).unwrap();
        plan::approve(&dir, 1, &notes).unwrap();

        let mut fields = BTreeMap::new();
        fields.insert("working_title".to_string(), "Renamed".to_string());
        let stored = plan::load(&dir, 1).unwrap();
        let edited = plan_from_fields(&stored, &fields);
        assert!(edited.approved, "the page cannot lift the lock");
        let refused = plan::update(&dir, &edited).unwrap_err().to_string();
        assert!(refused.contains("un-approve it to edit it"), "{refused}");
        assert_eq!(plan::load(&dir, 1).unwrap().body.working_title, "One");

        plan::unapprove(&dir, 1).unwrap();
        let stored = plan::load(&dir, 1).unwrap();
        plan::update(&dir, &plan_from_fields(&stored, &fields)).unwrap();
        assert_eq!(plan::load(&dir, 1).unwrap().body.working_title, "Renamed");
        assert_eq!(
            crate::notes::load_notes(&notes).unwrap().title,
            "One",
            "un-approving and editing leave the approved deck alone"
        );
        let view = plan_view(&session, PlanLive::default());
        assert_eq!(view.locked, None);
        assert!(!view.plan.unwrap().approved);
    }

    #[test]
    fn layout_keys_round_trip_through_the_picker() {
        let options = layout_options();
        let keys: Vec<_> = options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["talking-head", "split", "outline"]);
        let mut fields = BTreeMap::new();
        fields.insert("ch1.layout".to_string(), "talking-head".to_string());
        let edited = plan_from_fields(&plan("One"), &fields);
        assert_eq!(
            edited.body.chapters[0].layout,
            Some(crate::layouts::Pair::TalkingHead)
        );
        fields.insert("ch1.layout".to_string(), String::new());
        assert_eq!(
            plan_from_fields(&edited, &fields).body.chapters[0].layout,
            None
        );
    }

    #[test]
    fn estimates_read_as_minutes_and_seconds() {
        assert_eq!(mmss(45), "0:45");
        assert_eq!(mmss(125), "2:05");
        assert_eq!(parse_mmss("2:05"), Some(125));
        assert_eq!(parse_mmss(" 90 "), Some(90));
        assert_eq!(parse_mmss("1:"), None);
        assert_eq!(parse_mmss("1:5"), None);
        assert_eq!(parse_mmss("1:75"), None);
    }

    /// The Record tab's teleprompter: one slide per plan chapter plus the
    /// past-the-plan slide, the hook and CTA saying their lines, author text
    /// escaped, and the page opening on the chapter it was asked for.
    #[test]
    fn the_teleprompter_shows_the_plan_on_the_chapter_asked_for() {
        let mut plan = plan("Ship <it>");
        plan.number = 3;
        plan.body.chapters[1].show = "the build log".into();
        plan.body.chapters[1].layout = Some(crate::layouts::Pair::Split);
        let html = teleprompter_page(&plan, 2, true);
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("Plan 3"));
        assert!(html.contains("draft — not approved"));
        assert_eq!(html.matches("<section class=\"slide").count(), 4);
        assert!(html.contains("Chapter 1 of 3 · Hook"));
        assert!(html.contains("Your deploy takes an hour."));
        assert!(html.contains("Subscribe for part two."));
        assert!(html.contains("On screen: <b>the build log</b>"));
        assert!(html.contains("Split"));
        assert!(html.contains("Ship &lt;it&gt;"), "author text is escaped");
        assert!(html.contains("goToChapter(2, true);"));
        assert!(html.contains("This chapter has no plan chapter"));

        plan.approved = true;
        let idle = teleprompter_page(&plan, 0, false);
        assert!(idle.contains(">approved<"));
        assert!(
            idle.contains("goToChapter(1, false);"),
            "chapter 0 opens on the first"
        );
    }
}
