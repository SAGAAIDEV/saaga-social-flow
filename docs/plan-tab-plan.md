# Project, Plan, Record: planning a video before it is recorded

## Context

The Record tab (`src/ui/mod.rs`, `draft` pane) does three jobs in one window:

- **Project** — the Project popup, the name field, and New Project / Clean Up
  Old Recordings in the Session group.
- **Recording and versioning** — devices, layout, Start/Retake/Pause/Stop,
  Render, the Version popup and New Version, the preview.
- **Speaking notes** — the right column's "Speaking notes" sub-tab: Provider,
  Model, a "Notes prompt" field, the Notes / Copy Transcript / Suggest Shorts
  buttons, and the deck (`notes::NotesPane`).

The only way to get a plan today is backwards: record a full rehearsal (camera,
screen and all), press **Notes**, and `agent::notes` turns the rehearsal
transcript into a deck. There is no hook, no call to action, no statement of
who the video is for, and nothing that tells the presenter how to record it.

`notes/notes.json` is nonetheless load-bearing. It is per *project* (copied
forward by New Version) and read by chapter position — `chapters[n-1]` is
recording chapter *n* — by the chapter cards (`edit::chapter_titles`), titles
(`titles::run`), the blog's section headings (`longform::build`), social posts
and reflect. Whatever the plan becomes, it has to keep feeding that file.

This plan splits the Record tab into three workflow steps — **Project → Plan →
Record** — and makes Plan the place a video is thought through before a camera
turns on: talk the idea into the mic, add instructions, and get back a
structured plan (hook, outline, chapter outlines, CTA, recording instructions)
that becomes the speaking-notes deck.

## Decisions

1. **Three tabs where there was one.** `workflow::STEPS` becomes
   `project, plan, video, youtube, blog, socials`.
   - **Project**: the Project popup, name field, New Project, Clean Up Old
     Recordings — moved, not rebuilt. They keep their selectors and
     `ControlTarget` ivars, so `switch_project` / `new_project` /
     `run_cleanup` do not change. Below them a web pane summarises the
     project: plan status, versions with chapter counts and render state,
     published links.
   - **Plan**: new — see below.
   - **Record** (label "Video recording" stays): devices, layout, record
     buttons, Render, **Version popup and New Version**, the preview, and on
     the right "Video details" plus the deck as a read-only teleprompter.
     Versioning stays with recording because a version *is* a set of takes;
     the plan is per project and outlives versions, exactly like notes today.

2. **Plan takes are figure asides without a figure.** A plan take is mic-only
   audio recorded with `figure::aside::Aside::start(conn, path)` — the second
   writer on the already-running `AvDelegate`. It works with no take rolling
   (the router-less branch of `App::begin_break` already relies on this). No
   new `AVCaptureSession`: `av_delegate.rs` and `docs/figure-aside-plan.md`
   record the static a cold second session on the same mic produced. The
   delegate already refuses a second aside, which is the guard against a
   plan take and a figure break colliding. Quit must finish an open plan take
   before capture stops, as it already does for a figure's aside
   (`end_break(false)` in the `Action::Quit` arm), or the `.m4a` is left
   without its trailer and will not play.

3. **Transcription is the existing chapter path.** On Stop,
   `notes::spawn_chapter_transcript(take)` — ledger, silence detection, retry
   and `IN_FLIGHT` all come with it; the transcript lands beside the take.
   The *waiting* helpers do not carry over: `notes::still_running` and
   `job_state` build `chapter-NN` names inside a recording folder. Plan takes
   get their own small waiter over their transcript paths, on the path-based
   `transcribe::running(&out)` those helpers already use.

4. **One structured call, one prompt id.** `agent::plan` on
   `agent::extract::extract::<PlanExtraction>` under a new
   `prompt::PLAN = "plan.video"`, registered in `prompt::builtin` so the house
   style can be tuned in `~/.stream-recorder/prompts/plan.video.txt` without
   retuning notes or the outline. Limits are enforced in code after the call,
   as `agent::outline::clean_points` does, not trusted to the prompt.

5. **Plan owns `notes.json` once a plan is approved.** Approving writes the
   deck through `notes::write_deck`, so the teleprompter, card titles, titles,
   blog headings and posts all read the plan with no change on their side.
   Only the approved version writes the deck: selecting another version shows
   it and does not change `notes.json`, so a recording already made against
   one plan does not get another plan's titles on its next render. The first
   time a plan writes the deck, an existing rehearsal-built `notes.json` is
   copied to `notes/notes.before-plan.json`.

6. **The hook is chapter 1, the CTA is the last chapter, the body is
   everything between.** Each is recorded as a chapter of its own, and plan
   chapter *n* is recording chapter *n*, so every consumer of `chapters[n-1]`
   stays aligned. Neither the hook nor the CTA gets a chapter card:
   - The hook needs nothing. `edit::compose` already gives chapter 1 no card
     and numbers the cards from chapter 2 as "01", so the first body chapter
     reads "01" as it should.
   - The CTA chapter needs its card skipped. Each plan chapter carries a
     `kind` (`hook` / `body` / `cta`). Compose skips the card in front of the
     **last recorded chapter** whenever the approved plan ends with a CTA, not
     whichever chapter holds the CTA's position in the plan. Plan and
     recording only line up by count, and one extra chapter break would
     otherwise skip the wrong card. That is the one render change this plan
     makes.
   - The Record tab shows "Chapter 3 of 5 — <planned title>" while recording
     and warns when a take goes past the plan's chapter count, so a mismatch
     is seen while it can still be fixed.
   - The CTA chapter is never rendered as a vertical short (every chapter is
     one today — `distribute` uploads "the chapter shorts"). Whether the hook
     chapter should be one is still open.
   The model is told to produce exactly one hook chapter first, one CTA
   chapter last, and any number of body chapters between. `clean` enforces
   that shape rather than trusting the model to follow it.

7. **"Instructions" goes both ways.** The author's instructions (audience,
   tone, length, must-mention) are input; the model returns `instructions` —
   recording directions: what to have open, setup, delivery — as output.

8. **Plans have versions, and approving one locks it.** Every Build or
   Refine writes a new plan version, `plan/v1.json`, `v2.json`, and so on.
   Nothing is overwritten, and the tab has a version picker to go back to an
   earlier one. The UI calls them **Plan 1, Plan 2…**, never "v1", which is
   what recording versions are called. At most one version is approved at a
   time; approving one un-approves the rest. **Approve** locks that version: its fields go read-only and Refine
   is disabled until it is un-approved. A refine always starts from the
   selected version and produces the next number, so going back to v2 and
   refining gives v4, not a branch.

9. **The rehearsal path becomes an input to Plan, not a second generator.**
   "Plan from rehearsal" feeds the current version's closed chapter
   transcripts into the same call in place of (or beside) plan takes. The
   **Notes** button and `agent::notes::SYSTEM` stay until that is proven, then
   go (phase 4).

## The Plan tab

A native strip on top, a web pane (`templates/plan.html`) below.

- **Native strip**: Provider and Model — the popups moved from the Record
  tab's right column, still driving `notes_pick`. Plan is a single, important
  call, so its default model is a strong one rather than Gemini Flash.
- **Web pane**, top to bottom:
  1. **Your idea** — `Record idea` / `Stop` toggle with a timer, and a list of
     takes: duration, transcript state (transcribing / ready / silent /
     failed with retry), delete. A "type it instead" textarea for ideas that
     are easier written.
  2. **Your instructions** — textarea, autosaved.
  3. **Build plan** (or **Refine** once a plan exists: sends the current plan,
     any new takes, and a refine note), and **Plan from rehearsal**.
  4. **The plan** — editable, autosaved: working title, audience, promise,
     hook, outline, chapters (title, goal, points, verbatim, what to show,
     suggested layout, estimated length), CTA, recording instructions.
     A version picker (v1, v2, …) above it. **Approve** locks the selected
     version and disables Refine until it is un-approved. "Go to recording →"
     switches tabs.

Status for the job goes on one native line above the pane, as the video
details pane does, so progress never repaints the page mid-typing.

## Data

All under the project root, beside `notes/`:

```
plan/
  input.json                 { instructions, typed }
  take-01.m4a                mic-only idea take
  take-01.transcript.json    written by spawn_chapter_transcript
  v1.json, v2.json, …        one per Build / Refine, never overwritten
  current.json               { selected: N } — the version on screen
```

```rust
pub struct Plan {
    pub working_title: String,
    pub audience: String,
    pub promise: String,             // what the viewer walks away with
    pub hook: Hook,                  // { line: verbatim opener, angle: why it lands }
    pub outline: Vec<String>,        // the arc, 3-6 beats
    pub chapters: Vec<PlanChapter>,
    pub cta: Cta,                    // { line, placement }
    pub instructions: Vec<String>,   // recording directions
    #[serde(default)] pub approved: bool,          // locks this version against Refine
    #[serde(default)] pub refined_from: Option<u32>,
    #[serde(default)] pub refine_note: String,
    #[serde(default)] pub sources: Vec<String>,     // takes / rehearsal it was built from
}
pub struct PlanChapter {
    pub kind: ChapterKind,           // hook (first) / body / cta (last)
    pub title: String,               // 2-5 words: becomes the card title
    pub goal: String,
    pub points: Vec<String>,         // ≤ 6 fragments
    pub verbatim: Option<String>,
    pub cues: Vec<String>,
    pub show: String,                // what is on screen
    pub layout: Option<Pair>,        // talking head / split / outline
    pub est_seconds: Option<u32>,
}
```

`Plan::to_notes(&self) -> NotesData` is the one mapping from plan to deck, one
deck chapter per plan chapter in order. It is pure and unit-tested. The `hook`
and `cta` fields carry the reasoning (the angle, where the CTA sits). The
chapters carry what is said: the hook chapter's `verbatim` is `hook.line`, and
the CTA chapter's is `cta.line`.

## Phases

### 1 — the plan itself (no UI)
- `src/plan/mod.rs` (schema, versioned load/save, select, approve, `to_notes`),
  `src/agent/plan.rs` (SYSTEM, extraction types, `clean`, `user_prompt`,
  `build_plan`), `prompt::PLAN` + `builtin`.
- `user_prompt` takes the take transcripts, typed idea, instructions, and on
  Refine the current plan as JSON.
- `cargo run -- plan <project root>` in `src/cli.rs`: builds from whatever
  transcripts are in `plan/`, writes `plan.json` and the deck. Lets the prompt
  be tuned without the window.
- Tests: clipping and caps; empty chapters dropped; `clean` repairs the
  shape (exactly one hook first, one CTA last, a missing one is filled from
  the `hook`/`cta` fields); `to_notes` keeps chapter count and order;
  `user_prompt` carries every source and the refine base; saving never
  overwrites a version; an approved version refuses a refine.

### 2 — the tab split
- `workflow::STEPS` gains `project` and `plan`; its test updated.
- Build the Project view: move the Project popup, name field, New Project and
  Clean Up out of `layout_left` and the Session group into it; add the
  project summary web pane (`templates/project.html`).
- Build the Plan view: native strip with the moved Provider/Model popups,
  `plan.html` web pane. The Record tab's right column keeps "Video details"
  and the deck, read-only; Copy Transcript and Suggest Shorts stay on Record
  (they act on a finished take). The "Notes prompt" field goes — its job is
  the plan's instructions box.
- Update the `buttons` array ranges in `ui/mod.rs` (RECORD/NOTES/SESSION,
  and `REGIONS`, which is an index into SESSION) carefully; the comment there
  says why.

### 3 — recording ideas and building from them
- `WebEvent`/`UiEvent`: `PlanRecordToggle`, `PlanDeleteTake(n)`,
  `SavePlanInput(fields)`, `BuildPlan { refine }`, `PlanFromRehearsal`,
  `SavePlan(fields)`, `SelectPlanVersion(n)`, `ApprovePlan(bool)`.
  `SavePlan` edits the selected version in place and is refused when it is
  approved. Build/Refine is refused when the selected version is approved.
- Quit finishes an open plan take (decision 2); Start Recording and Record
  idea refuse each other.
- `App::plan_take: Option<Aside>` in a new `src/app/plan.rs`. Start refuses
  while a chapter is recording or a break is on; Start Recording refuses while
  a plan take is on. Stop finishes the aside and spawns the transcript.
- `PlanJob` on a background thread with a `Receiver`, scoped to the project it
  started in (the `CopyJob` pattern in `app/video_brief.rs`). It waits for
  in-flight take transcripts using the existing `still_running` /
  `waiting_message` helpers, then calls `agent::plan`.
- On result: write the next `vN.json`, select it, write the deck, and reload
  the teleprompter (`refresh_version_views`).

### 4 — tie-ins and retirement
- Chapter cards: `edit::compose` skips the card in front of the last recorded
  chapter when the approved plan ends with a CTA (decision 6), and falls back
  to today's behaviour when there is no approved plan. Add a test next to the
  existing "chapter one has no card" test.
- Shorts: leave the CTA chapter out of the vertical parts.
- Record tab: "Chapter n of N — <title>" and the past-the-plan warning.
- Video details: `video_brief::Source::prompt` includes the plan's promise,
  audience, hook and CTA, so the title and description say what was meant.
- Outline layout: `agent::outline::user_prompt` gets the chapter's planned
  points as a hint, so the on-screen card matches the plan. Anchors and
  timing still come from the transcript.
- Record tab: when chapter *n* starts and the plan suggests a layout for it,
  preselect that layout (the pending-layout path already exists).
- Remove the Notes button and `agent::notes` once Plan from rehearsal has
  replaced it.

## Verification

`cargo test`, `cargo clippy`, `cargo fmt --check` each phase, plus the CLI in
phase 1 against a real project's transcripts. The app is not launched from
here (it takes the camera and mic); the visual check of each phase is left to
the author, with a short list of what to look at.

## Settled

- Hook as its own chapter 1 and CTA as its own last chapter, with any number
  of body chapters between. Neither gets a chapter card (decision 6).
- Approving locks a plan version against Refine and edits. Plans are expected
  to go through several versions (decision 8).
- The Project tab is pick / name / new / clean up plus the summary. No project
  templates or duplication.

## Known limits

- **Clean Up Old Recordings deletes idea takes.** It removes every audio and
  video file (`m4a` included) from projects untouched for a week. The
  transcripts are JSON and survive, so a plan can still be rebuilt from them;
  only replaying the take is lost.

## Open questions

1. Should the hook chapter be rendered as a vertical short? (The CTA is not.)
