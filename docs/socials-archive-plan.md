# Socials archive: every project's record in `saaga-internal-dev/socials/`

Reviewed 2026-10-08. The example layout, filled in with one project of two
videos, is the "Socials Archive Layout" page.

## Context

A video project leaves three kinds of trace today, and only one of them is
shared:

| What | Where | Lasts |
|---|---|---|
| Final videos, chapter shorts, `transcript.txt`, thumbnail exports | `saaga-screencast-media/screencast/<project>/vN/`, public read | **30 days** — a lifecycle rule expires all of `screencast/`, whatever the `retain` tag says |
| Team template (funnel links, YouTube footer, categories) | `saaga-screencast-media/team/templates.json`, private | indefinitely |
| Everything else — the plan, raw takes, edits, transcripts, notes, post copy, Buffer post ids, YouTube and blog ids, Substack notes, analytics, every LLM call | `~/.stream-recorder/sessions/<project>/` on one Mac | until that disk goes |

`saaga-screencast-media` is in no Terraform; it was made by hand.
`saaga-internal-dev` is `saaga-terraform/s3-internal.tf` on `main`
(`${project}-internal-${aws_env}`): private, KMS-encrypted, versioned, no
expiry. What is in it today:

| Prefix | Written by | Shape |
|---|---|---|
| `meetings/` | the Google Meet transcript lambdas | `<date>/<meeting>/metadata.json`, `transcript.json`, `transcript.txt`; locked to those lambdas and Terraform, so the dev login cannot read or list it |
| `org/` | `/role` | `<person>/role.md`, `preferences.md`, `communication-preferences.md` |
| `reports/` | `/evening` | `daily-summaries/<date>/<name>.md` and `.json` |
| `daily/`, `tasks/` | nobody yet | empty |

The pattern: the subject at the top, then a natural key, then a JSON file for
programs beside a text or Markdown file for people. Every object write goes to
EventBridge; the one rule today matches `meetings/*/transcript.json`.

This plan makes `s3://saaga-internal-dev/socials/` the company's record of its
content: **category → project → videos**, plus the calendars that schedule it.

## Decisions

1. **The internal bucket is the record; the media bucket stays the delivery
   host.** Buffer, Instagram and TikTok fetch a video by public URL, sometimes
   days after it is queued. The internal bucket blocks public access, and a
   presigned URL from an SSO login dies within hours. So finals are uploaded
   twice: to `screencast/` for the platforms (expiring) and to `socials/` to
   keep.
2. **`socials/` is for everyone on the team.** It is not locked down like
   `meetings/`: the dev login and the solve task role read and write it.
3. **Category, then project, then videos.** A category is a topic area. A
   project is one topic in it, planned once, that makes one or more videos.
   There is always a `videos/` folder, even for a single video, so the layout
   keeps one shape.
4. **Planning starts at the project.** The project plan takes a topic, a
   category and a format, and proposes the videos to make: title, goal, order.
   Each video then gets its own plan — chapters, hook, call to action, speaking
   notes — which is what the Plan tab makes today.
5. **A project is long or short.** Long is horizontal, short is vertical. It is
   chosen on the project, and its videos follow it. A short-format project
   renders and uploads exactly as a short recorded with Start Short does:
   vertical only, uploaded as a Short on its own. An aside recorded with Start
   Short is a video of its own in the same project, always short; each video's
   `metadata.json` records its format.
6. **Folders are named by a slug of the title, fixed when created.** No dates
   in folder names. When it was created, its order in the project and its
   current title are in `metadata.json`, so a retitle moves nothing. A video's
   recording id — today's session folder, `2026-10-09_15-00-00` — is in its
   `metadata.json` too; it stays the key in the media bucket and in Buffer.
   Changing a project's category moves its folder (S3 copy, then delete);
   nothing outside the bucket points at these keys.
7. **Every project and video folder has a `metadata.json` and a
   `summary.md`.** `metadata.json` is the name `meetings/` uses; it is rebuilt
   on every sync and never edited by hand. `summary.md` is the YouTube title
   and description with the links: copy we already write and approve, so it
   costs no model call.
8. **Keep only what we need.** Sources, decisions, finals and what was
   published. Anything the app rebuilds from those — laid-out chapter renders,
   cut chapter videos, composition files, render intermediates, waveforms, the
   per-step copies of LLM calls — stays local.
9. **New projects only.** No backfill. A project is archived if it was created
   after this ships: New Project writes `"archive": true` to `session.json`,
   and sync does nothing for a project without it. Old projects stay local.
10. **A category is one slug everywhere**: the folder, the Strapi blog category
    (`/blog/category/[slug]`), the YouTube playlist and the hashtags in the
    team template, all from `src/category.rs`. A project with no category yet
    goes under `uncategorized/`.
11. **Notes are per version.** The speaking notes a version was recorded with
    and the critique of that version's take go under `notes/vN/`, so a new
    version's notes sit beside what the previous critique said to change.
12. **An organised layout, not a mirror of the session folder.** The local
    folder is organised for the app; this is organised for people. Each
    `metadata.json` carries a file index (key → local path), so the mapping
    goes both ways.
13. **Format is a folder under `final/`.** `final/vN/horizontal/` and
    `final/vN/vertical/`.
14. **Exhaustive by construction.** The local → S3 mapping is a table in code.
    Every local file either maps to a key or is on an explicit skip list with a
    reason. A file that is neither is reported as unmapped, and the test fails,
    so a new feature cannot quietly stop being archived — or start uploading
    something nobody decided to keep.
15. **Events come later.** Writes to `socials/` will fire events that lambdas
    act on: uploads, social posts, and a morning report built from the day's
    work (which replaces `/morning`). `socials/activity/` is reserved for them;
    the design is its own plan.

## Layout

```
socials/
  README.md                         this layout, for anyone browsing
  team/templates.json               funnel links, YouTube footer, categories (moved here)
  prompts/                          the prompt library every LLM call names
    <prompt_id>.txt
    versions.jsonl
    history/<prompt_id>.<timestamp>.txt
  references/thumbnails/            reference images for thumbnail generation
  calendars/
    YYYY-MM.json                    the month's slots across all categories (see Calendars)
  activity/                         reserved for events (decision 15)
  <category>/                       agents | education | go-to-market | ai-seo-automation | uncategorized
    <project>/                      support-agents-that-file-tickets
      metadata.json                 title, category, format, created, the videos in order
      summary.md                    each video's YouTube title and description, with links
      plan/                         the project plan: topic, format, the videos it proposes; its versions
      llm/calls.jsonl               the calls made for the project plan
      videos/
        <video>/                    the-whole-ticket-loop
          metadata.json             title, order, format, status, recording id, versions, links, file index
          summary.md                its YouTube title and description, with links
          plan/                     the video plan, chapter plans per version, rehearsal transcripts
          notes/vN/                 the speaking notes vN was recorded with, and the critique of vN
          recording/vN/             raw camera, screen and audio per chapter
          transcripts/vN/           per chapter and whole, as JSON and text
          edit/vN/                  cut lists, layouts, outline, dropped chapters; card and figures
          final/vN/horizontal/      longform.mp4 (long)
          final/vN/vertical/        chapter-NN.mp4 cut from a long video; short.mp4 for a short
          thumbnails/               the approved set and its exports, approval
          distribution/
            youtube/                metadata (title, description, tags), title options, uploads
            posts/vN/               copy per video and platform
            buffer/                 what was queued, with Buffer post ids
            blog/vN/                article, Strapi payload, artwork; published ledger
            substack/vN/            notes
            media-links/vN.json     the public URLs the platforms fetched
          analytics/                Buffer numbers over time, reflect
          llm/calls.jsonl           every call: model, provider, prompt id + version, full prompt, output
```

A video can exist before it is recorded: the project plan creates its folder,
`metadata.json` (status `planned`, no recording id) and `plan/`. Recording gives
it a recording id, and everything else follows.

## Where every file goes

Until the app has projects above videos, each app project is one archive
project with one video, both named from its title, and the video's plan stands
in for the project plan.

Paths are relative to the session folder locally and to
`socials/<category>/<project>/videos/<video>/` in S3. `vN` is the recording
version; `NN` is a chapter.

**Video, plan and notes**

| Local | S3 |
|---|---|
| `session.json`, `category.json` | folded into the video's and the project's `metadata.json` |
| `plan/input.json`, `plan/current.json`, `plan/vN.json` | `plan/` |
| `plan/take-NN.transcript.json`, `plan/transcripts.jsonl` | `plan/takes/` |
| `video-brief.json` | `plan/video-brief.json` |
| `drafts/vN/chapter-NN.plan.json` | `plan/vN/chapter-NN.plan.json` |
| `drafts/vN/notes.json`, else `notes/notes.json` | `notes/vN/speaking-notes.json` — the app keeps a version's deck in `drafts/vN/` when a new plan is approved over it; a version without one was recorded under the current deck |
| `drafts/vN/critique.json` | `notes/vN/critique.json` |

**Recording, transcripts, edit, final**

| Local | S3 |
|---|---|
| `drafts/vN/chapter-NN.mp4` | `recording/vN/chapter-NN-camera.mp4` |
| `drafts/vN/chapter-NN-screen.mp4` | `recording/vN/chapter-NN-screen.mp4` |
| `drafts/vN/chapter-NN.m4a` | `recording/vN/chapter-NN.m4a` |
| `drafts/vN/chapter-NN.transcript.json`, `chapters.jsonl`, `transcripts.jsonl` | `transcripts/vN/` |
| `render/vN/horizontal/transcript.txt` | `transcripts/vN/longform.txt` |
| `render/vN/vertical/chapter-NN.txt` | `transcripts/vN/chapter-NN.txt` |
| `edit/vN/chapter-NN/edits.json` | `edit/vN/chapter-NN.edits.json` |
| `edit/vN/dropped.json` | `edit/vN/dropped.json` |
| `drafts/vN/chapter-NN.layout.json` | `edit/vN/chapter-NN.layout.json` |
| `outline/vN/outline.json`, `card.json` | `edit/vN/outline.json`, `edit/card.json` |
| `figures/`, `figures.jsonl` | `edit/figures/` |
| `render/vN/horizontal/longform.mp4` | `final/vN/horizontal/longform.mp4` |
| `render/vN/vertical/chapter-NN.mp4` | `final/vN/vertical/chapter-NN.mp4` |
| `render/vN/vertical/longform.mp4`, in a short | `final/vN/vertical/short.mp4` |
| `render/vN/summary.json` | `final/vN/summary.json` |

**Thumbnails, distribution, analytics**

| Local | S3 |
|---|---|
| `thumbnails/{artwork,approval,brief}.json`, `thumbnails.jsonl` | `thumbnails/` |
| `thumbnails/sets/<approved id>/`, `thumbnails/exports/<approved id>-*.jpg` | `thumbnails/approved/` |
| `youtube-metadata.json` | `distribution/youtube/metadata.json` |
| `titles/vN/titles.json` | `distribution/youtube/titles/vN.json` |
| `youtube.jsonl` | `distribution/youtube/uploads.jsonl` |
| `posts/vN/posts.json`, `posts/vN/<video>/<platform>.md` | `distribution/posts/vN/` |
| `schedule/vN/schedule.json` | `distribution/buffer/vN.json` |
| `schedule.jsonl` | `distribution/buffer/queued.jsonl` |
| `distribute/vN/links.json` | `distribution/media-links/vN.json` |
| `blog/vN/article.json`, `video-post.json`, `artwork/`, `component-job/` | `distribution/blog/vN/` |
| `blog.jsonl` | `distribution/blog/published.jsonl` |
| `substack/vN/substack.json`, `notes.md` | `distribution/substack/vN/` |
| `analytics.jsonl` | `analytics/analytics.jsonl` |
| `reflect/vN/reflect.json` | `analytics/reflect/vN.json` |
| `llm.jsonl` | `llm/calls.jsonl` |
| `shorts/short-NN/` | a video of its own in the same project, `videos/<its slug>/`, laid out the same way |

**Global, under `socials/`**

| Local (`~/.stream-recorder/`) | S3 |
|---|---|
| `prompts/<prompt_id>.txt`, `versions.jsonl`, `history/` | `prompts/` |
| `references/` | `references/thumbnails/` |
| team template (S3 `team/templates.json` in the media bucket) | `team/templates.json` |

### Left out, on purpose

| Local | Why |
|---|---|
| `drafts/vN/chapter-NN-{horizontal,vertical}.mp4` | laid-out chapter renders; Render rebuilds them from the raw takes and the layout |
| `edit/vN/chapter-NN/chapter-NN-{horizontal,vertical}.mp4` | cut chapters; rebuilt from those and `edits.json` |
| `compose/` | composition files the renderer generates from the edit |
| `render/vN/**/seg-NN-*.mp4`, `*.joinable.mp4`, `longform.parts.txt`, `gpu.jsonl` | render intermediates and run logs |
| `edit/vN/chapter-NN/audio.mp3`, `peaks.json`, `drafts/vN/chapter-NN.mp3`, `plan/take-NN.m4a` | audio and waveforms derived from the takes; the rehearsal is kept as its transcript |
| `<step>/llm.json`, `<step>/vN/llm.json` | each is a copy of the step's last line in `llm.jsonl` (`agent::trace::write_step`) |
| `thumbnails/stills/`, `screens/`, `candidates/`, unapproved `sets/` and `exports/` | inputs and options nobody chose |
| `drafts/vN/.discarded/` | retakes that were thrown away |
| `cleaned.json` | a local record of what Clean Up deleted |
| `.blog.html`, `.video.html`, `.youtube.html`, `.thumbnail.html`, … (dot-html at the root), `notes/notes.html`, `blog/vN/preview.html` | rendered views, rebuilt from the JSON |
| `plan/.wrote-deck`, `.DS_Store`, `node_modules/` | markers and junk |
| `config.json`, `logs/`, `strapi-library.json` | one machine's settings, logs and cache |
| `~/.stream-recorder/notes/standalone/` | one speaking-notes deck from 2026-08-13, before notes belonged to a project; nothing writes there now |

## `metadata.json`

Rebuilt on every sync from the ledgers, so it is never edited by hand. The
project's:

```json
{
  "project": "support-agents-that-file-tickets",
  "title": "Support agents that file their own tickets",
  "category": "agents",
  "format": "long",
  "created": "2026-10-09T14:20:00-07:00",
  "videos": [
    { "video": "the-whole-ticket-loop", "order": 1,
      "title": "The whole ticket loop, on a real queue",
      "format": "long", "status": "published",
      "recording_id": "2026-10-09_15-00-00", "youtube": "https://youtu.be/…" },
    { "video": "set-up-the-ticket-agent", "order": 2,
      "title": "Set up the ticket agent in ten minutes",
      "format": "long", "status": "planned", "recording_id": null }
  ],
  "synced_at": "…"
}
```

A video's:

```json
{
  "video": "the-whole-ticket-loop",
  "project": "support-agents-that-file-tickets",
  "order": 1,
  "title": "The whole ticket loop, on a real queue",
  "format": "long",
  "status": "published",
  "recording_id": "2026-10-09_15-00-00",
  "current_version": 2,
  "versions": [1, 2],
  "youtube": [{ "video_id": "…", "orientation": "horizontal", "privacy": "public" }],
  "blog": { "slug": "…", "url": "https://saagasolve.com/blog/…", "published": true },
  "buffer": [{ "platform": "linkedin", "buffer_post_id": "…", "queued_at": "…" }],
  "media": [{ "id": "longform", "url": "https://saaga-screencast-media.s3…/landscape-….mp4" }],
  "files": { "recording/v2/chapter-01-camera.mp4": "drafts/v2/chapter-01.mp4" },
  "synced_at": "…"
}
```

`status` is the furthest step reached: `planned`, `recorded`, `edited`,
`rendered`, `published`.

## Calendars

`socials/calendars/YYYY-MM.json` is one month across every category: one row
per slot.

```json
{ "date": "2026-10-14", "category": "agents",
  "project": "support-agents-that-file-tickets", "video": "the-whole-ticket-loop",
  "title": "The whole ticket loop, on a real queue", "format": "long",
  "platform": "youtube", "status": "published", "links": {} }
```

- **Planned** rows are written by people (and later by a Calendar tab) before a
  project exists; `project` and `video` are filled in when they are planned.
- **Scheduled** and **published** rows come from each video's ledgers on sync.
  `schedule.jsonl` records when a post was queued, not when it goes out, so the
  due time is read from Buffer when analytics polls.

Defining the cadence per category (how many of each format, which days) is its
own plan; this one reserves the folder and the row shape.

## How it gets there (saaga-social-flow)

1. **Format on the project.** The Project tab picks long or short beside the
   category, saved in `session.json`. Short renders and uploads the way a short
   from Start Short does.
2. **Projects above videos.** A project screen takes the category, the format
   and the topic, and plans: the plan proposes the videos, and approving it
   creates each one with its slug and its own plan. New Video opens inside a
   project instead of starting a new one. Today's Plan tab becomes the video
   plan. A video gets its recording id when recording starts.
3. **`src/archive/layout.rs`** — the mapping and skip tables above as one pure
   function, local path → key, skip (with reason) or unmapped. Tested against a
   fixture session that has every file kind.
4. **`src/archive/sync.rs`** — walk the session, upload what changed, write
   both `metadata.json` files and `summary.md`. "Changed" comes from a local
   `.archive.json` (size, mtime, SHA-256 per path): with KMS encryption an S3
   ETag is not an MD5, so S3 cannot tell us. Large files go multipart through
   the code `distribute::s3` already has. Media goes up with storage class
   `INTELLIGENT_TIERING`.
5. **Settings:** `INTERNAL_BUCKET=saaga-internal-dev`,
   `INTERNAL_PREFIX=socials` in `.env` (not secret), the same
   `AWS_PROFILE=dev` login. Off while unset.
6. **Which projects:** New Project writes `"archive": true` to `session.json`;
   sync skips any project without it.
7. **When it runs:** small files (JSON, Markdown, images) after every step that
   writes them: Plan, Critique, Render, Thumbnail approve, Titles, Posts, Queue
   to Buffer, YouTube upload, Blog publish, Substack, analytics poll. Raw takes
   and finals in a background queue after Render, with progress in the status
   bar. Never fatal: a failure is a status line and the next sync retries. An
   expired SSO login gets the same hint sops gives.
8. **Project tab:** an **Archive** row showing the folder, last sync and
   anything unmapped, with an **Archive now** button. Changing the category
   moves the folder.
9. **Team template:** read and write `socials/team/templates.json` in the
   internal bucket, falling back once to the old key.
10. **Global files:** prompts and references sync when they change.

## Terraform (saaga-terraform, from `main`)

1. Add `"socials"` to `s3_internal_folders`.
2. Set `bucket_key_enabled = true` on the bucket's KMS encryption: every object
   and multipart part is otherwise a KMS call.
3. A lifecycle rule on `socials/` that expires noncurrent versions after 90
   days, so a re-uploaded file is not kept forever by versioning. No expiry on
   current objects.
4. Check that the dev SSO role can `kms:GenerateDataKey` on the bucket's key,
   with one test upload, before any code ships.
5. Separately: import `saaga-screencast-media`, its public-read policy and its
   30-day rule into Terraform.

## Size

Per video, from the sessions so far: raw camera, screen and audio about
150 MB a chapter, the horizontal longform about 500 MB, plus the shorts.
Call it 1–1.5 GB for a four-chapter long video, a few cents a month each.

## Phases

1. Format on the project.
2. Terraform 1–4 and the KMS check.
3. Align the blog categories with Laura; set up the agreed ones.
4. `layout.rs` with the exhaustive test, then `sync.rs` and the `archive`
   flag on New Project — one video per project.
5. Projects above videos: the project plan, New Video inside a project.
6. Automatic sync after each step, the Project tab row, category moves.
7. Team template move, global prompts and references.
8. Calendars: the format here, then the cadence plan and a Calendar tab.
9. Events and the lambdas that act on them (decision 15).

## Open decisions

1. **The categories** — with Laura, who is confirming them: which of the
   proposed `education`, `go-to-market`, `ai-seo-automation`, `agents` map onto
   the blog's `ai-literacy`, `gtm`, `ai-powered-seo-geo`,
   `ai-powered-marketing`, `ai-powered-content-writing` and `Tools-comparison`,
   which retire, and whether posts are re-filed. Post URLs do not contain the
   category, so renaming one changes only its `/blog/category/<slug>` page,
   not rankings.
2. **Shorts cut from a long video.** Render cuts each chapter of a long video
   into a vertical short today. Recommended: keep them, as part of the long
   video (`final/vN/vertical/chapter-NN.mp4`). The alternative is that long
   projects make only the long video and shorts come from short projects.
3. **Events and lambdas** — their own plan (decision 15).
