# Production pipeline: from a category to a post that links to the blog

Written 2026-10-10 from an assessment of master at `3fc73b9`. This plan sets
the order in which the app gets every capability the workflow below needs.
There is no publishing cadence: videos go out as they are made, so the goal is
capability, not a calendar.

## The workflow

1. **Project.** Pick a category; decide one-off or series; decide long or short;
   name it. All four are project settings.
2. **Plan.** Record the idea as audio, build the plan, refine it, approve it.
   The plan is saved to S3.
3. **Production.** Record with the plan on screen. There can be several
   chapters, each in the layout the plan gives it, with a chapter card where
   the plan asks for one (mostly long form).
4. **Edit and render.** Each chapter is cut and rendered in the cloud as soon
   as it is recorded.
5. **Distribution.** YouTube first, then the blog, then the social posts, which
   link back to the blog.

## Decisions (2026-10-10)

1. **Rendering runs on GPU machines, one chapter at a time, started when the
   chapter is recorded.** Fargate has no GPU, and software WebGL measured about
   5.6 s a frame on a 16-vCPU box (480 frames in 45 minutes), so a vertical
   chapter would take hours. A g4dn T4 rendered a 3190-frame chapter in 299 s.
   The GPU path already exists (`src/edit/gpu.rs`, `infra/gpu-render/`); its
   Terraform moves into saaga-terraform.
2. **A series is episodes inside one project.** A series project plans its
   episodes up front: title, goal, order. Each episode then gets its own
   chapter plan, recording, YouTube upload and blog post. A one-off project is
   one video, as every project is today.
3. **Social posts are native video, with the blog URL added by the app.** The
   model no longer decides whether the link appears. A post waits until the blog
   it links to is live. Shorts cut from a long video link to that video's blog
   post. A short never gets a blog post of its own, so a standalone short (a
   short-format project) posts with no link and does not wait.
4. **No cadence.** Buffer posts go out on a tick with `shareNow`, as now.
5. **Instagram and TikTok say "link in bio".** They do not make caption links
   clickable, so their posts leave the URL out and end on a "link in bio" line;
   the bio link points at the blog. LinkedIn, X and Bluesky carry the URL.
6. **Storage.** `saaga-internal-dev/socials/` is the private record, and the
   plan is the first thing saved there. It went live with saaga-terraform PR #42
   on 2026-10-10. The finals Buffer fetches go to `saaga-dev-cdn/socials/`
   (`cdn.saagasolve.dev`). `saaga-screencast-media` is retired, which replaces
   decision 1 of `socials-archive-plan.md` ("finals are uploaded twice").

## Where we are

| Step | Today | Gap |
|---|---|---|
| Category | Built: Strapi topic categories, plus `demos` / `opinions` for shorts (`src/category.rs`). Set Up makes the Strapi row, the playlist and the template entry. | — |
| Long or short | Built: `format` in `session.json` (`src/sessions.rs`). It drives render, upload and Buffer. | The planner is not told the format. |
| One-off or series | Missing. One project folder is one video. `drafts/vN` are retakes. | Everything in Phase 7. |
| Name | Built, in the native strip above the tabs. | — |
| Plan: record, build, refine, approve | Built on the Plan tab (`src/plan/`, `src/agent/plan.rs`, `plan.html`). | The planner sees only the title and the idea, not the format or category, so a short gets a long-video shape. |
| Plan saved to S3 | Missing. Everything is under `~/.stream-recorder/sessions/<project>/plan/`. | Phase 2. |
| Layout per chapter | Built: `PlanChapter.layout`, applied by `preselect_planned_layout` and at New Chapter (`src/app/plan_recording.rs`). | — |
| Chapter cards | Built automatically: a 3 s card before every body chapter of the long version (`src/edit/compose.rs`), titled from the plan. | No per-chapter on/off in the plan. |
| Plan visible while recording | Built: the Speaking notes teleprompter follows the chapter. | — |
| Video details tab | Present. It holds the progress strip, title and description, figures, the rendered-clips review, Summary, Critique & next take, and published links. | Removing it needs new homes for the five things only it has (Phase 3). |
| Edit and render | Built: transcript cut, then HyperFrames, locally or on GPU machines (`cloud` render target). | Runs once per version on Render. Nothing renders a chapter as it closes. The Terraform is in this repo. |
| YouTube | Built: manual. It needs a render, approved artwork and valid copy (`src/stage.rs`). | Tags are not sent at upload. |
| Blog | Built: manual. It requires the YouTube upload, which it embeds. | — |
| Socials linked to the blog | Partial. The copy prompt uses a blog URL only if one exists when the copy is written. | Copy is written at render time, before YouTube or the blog exist. Buffer rows do not wait for the blog. Nothing rebuilds the rows when the blog goes live. |

## The pieces

### Project settings

- A `kind` field in `session.json`: `one-off` or `series`. It is set on the
  Project tab beside the category and format; unknown keys already survive a
  save. The name stays in the strip.
- The project's settings are what the planner and the S3 path read. The
  category slug is the first folder under `socials/`.

### Planning

- **The format is fixed once a plan is built.** The plan is shaped for it,
  so the Format card locks; the other format is a new project.
- **The planner gets the format and the category.** A short is planned as a
  short (one point, a hook in the first line, under a minute) in one body
  chapter, with its own prompt (`plan.short`), and `demos` / `opinions` use
  their definitions from `src/category.rs`. One chapter because the render
  leaves the outline and CTA chapters out of every vertical, and posts each
  chapter of a short as a clip. A long video keeps hook, outline, body
  chapters and CTA.
- **A chapter card is a plan field.** `card` on `PlanChapter`, on unless turned
  off, read only for body chapters, and editable on the Plan tab for a long
  video. `compose` writes a card only where it is on, and the cards count
  themselves, so turning one off leaves no gap in the numbers.
- **The plan saves to S3** after every build, refine, approve and un-approve:
  `plan/` goes to `saaga-internal-dev/socials/<category>/<project>/videos/<video>/plan/`.
  That is a video's plan. The project-level `socials/<category>/<project>/plan/`
  is the series plan (Phase 7), as in the archive layout. A one-off's video is
  named like its project, so its plan is at
  `socials/<category>/<project>/videos/<project>/plan/`.
- **The S3 folder name** is the project's name in URL form
  (`support-agents-that-file-tickets`), or its folder timestamp when it has no
  name. A rename moves the folder, as a category change does.
  `INTERNAL_BUCKET` and `INTERNAL_PREFIX` go in `dev.sops.env`, so the whole
  team writes to the same place. The sync is never fatal; a failure is a status
  line, and the next save retries. A category change moves the folder (copy,
  then delete). This is the first slice of the archive sync in
  `socials-archive-plan.md`, with the same `.archive.json` change record.

### Production

- **The Record tab's right side is the teleprompter only.** The Video details
  tab goes, and what only it holds moves:

  | Piece | New home | Why there |
  |---|---|---|
  | Rendered-clips review | YouTube tab, above Upload | You check the render right before it goes public. |
  | Notes for Write, Summary | YouTube tab, beside Write | They feed the title and description written there. |
  | Critique & next take | Plan tab | It writes a new plan version. |
  | Progress strip (Photo → YouTube) | Project tab | It is the project's status. |
  | The artwork/photo status line (~20 `set_thumbnail_status` calls) | Thumbnail tab | The messages are about artwork and photos. |

- Chapters, retakes and per-chapter layouts stay as they are.

### Edit and render, per chapter

- **When a chapter closes**, the transcript job already runs. When it lands,
  the app cuts that chapter and renders its pieces on a GPU machine: the
  vertical chapter, an outline body, the card. Renders are incremental and
  inputs are content-addressed, so pressing Render after the last chapter only
  joins the longform and fills in anything missing.
- **v1 is one machine per chapter**, through the existing `gpu::render_all`
  with a single job. Each machine pays about 2.5 minutes of boot.
- **v2 keeps one machine warm for a recording session.** It takes chapters as
  they arrive and powers off after a set idle time. A g4dn.2xlarge is about
  $0.75 an hour.
- **The Terraform moves to saaga-terraform** as `gpu-render.tf`, dev only: the
  bucket, launch template, IAM and SSM. The resources are imported, not
  recreated, and Jean reviews it. The app keeps launching machines with the
  dev login. A Lambda that starts renders from S3 events is possible later; it
  belongs with the events plan.

### Distribution

- **YouTube:** a manual press after the render review, plus tags at upload
  from the copy and the category hashtags. The title and description get an
  **Approve that sticks**, like the thumbnail's. It is saved with the copy,
  locks it, and drops if the copy changes, and Upload waits for it. Built
  ahead of the phases and merged on 2026-10-10 (`3bc84c3`).
- **Blog:** unchanged: Write Article, then Publish, after YouTube.
- **Socials:**
  - A Buffer row for a long video, or for a short cut from one, waits until
    that video's blog row in `blog.jsonl` says `published`. The skip reason
    says so: "waiting for the blog".
  - The plan appends the blog URL to the post text in code, so copy written at
    render time is still right.
  - Publishing the blog sets `schedule_replan`, so the rows appear on their
    own.
  - Per platform: LinkedIn, X and Bluesky carry the URL in the text. Instagram
    and TikTok end on "link in bio" instead (decision 5).
  - A standalone short has no blog, so its posts carry no link and go out as
    soon as they are approved.
  - Copy keeps being written at render time. Only the link waits.
- **Slack** also posts the blog URL when the blog publishes.

### Series (episodes)

- An episode is a project of its own inside the series folder,
  `{series}/episodes/ep-NN/`, the way a short beside a video is
  `{root}/shorts/short-NN/` today (`src/shorts.rs` `parent_of`). Each episode
  runs the whole pipeline above unchanged.
- **The series plan** is a level above the chapter plan: the idea, then the
  episodes proposed with title, goal and order, then refine and approve.
  Approving creates the episode folders, each with a draft plan seeded from
  its line in the series plan.
- The Project tab lists the episodes and their status. The project picker
  opens an episode the way it opens a short.
- On S3: `socials/<category>/<series>/videos/<episode>/…`. A one-off project is
  `socials/<category>/<project>/videos/<project>/…`, so the layout has one
  shape (decision 3 of the archive plan).

## Phases

Each phase is a branch and a merge, in this order. Phases 1–4 complete the
pipeline for one-off videos; 5–6 make it fast; 7 adds series; 8 is the rest of
the archive.

Before Phase 4, one real short goes end to end: render, check it on Socials →
Files, approve and upload on the YouTube tab. Nothing has been uploaded to
`saaga-dev-cdn/socials` yet, and Phase 4's links depend on it.

| # | Phase | Size | Depends on |
|---|---|---|---|
| 1 | Project `kind`; planner gets format and category (a short is one chapter); `card` per chapter | S | — |
| 2 | Plan saved to `saaga-internal-dev/socials/`; the Files tab shows that bucket too | S–M | — |
| 3 | Recording page: Video details goes, its pieces move | M | — |
| 4 | Socials wait for the blog and carry its URL; YouTube tags; Slack on blog publish | M | — |
| 5 | Each chapter renders on a GPU machine when it closes | L | — |
| 6a | `infra/gpu-render` moves into saaga-terraform (import). Can start any time: Jean's review takes time | S–M | — |
| 6b | A warm machine per recording session | M | 5, 6a |
| 7 | Series: series plan, episodes as sub-projects, Project tab list | L | 1, 2 |
| 8 | Archive sync for everything else (`socials-archive-plan.md` phases 4, 6, 7) on this layout | M–L | 2 |

## Open questions

Questions 1 and 2 (what standalone shorts link to, and the link on Instagram
and TikTok) were answered on 2026-10-10: decisions 3 and 5.

3. **Series on YouTube:** a playlist per series beside the category playlist,
   and a series page on the blog?
4. **Mixed episodes:** can one series hold both long and short episodes?
5. **Render review:** with every chapter rendered as it closes, does anything
   still wait for a person before YouTube, beyond the review on the YouTube tab?
   Proposed: no; the rendered clips on the YouTube tab, checked before Upload,
   are the review. Settle in Phase 5.
