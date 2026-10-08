# Socials archive: every video's record in `saaga-internal-dev/socials/`

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
expiry, with folder markers for `daily`, `meetings`, `reports`, `tasks`, `org`.

This plan makes `s3://saaga-internal-dev/socials/` the company's record of its
content: **category → project → what the project needs to keep**, plus the
calendars that schedule it.

## Decisions

1. **The internal bucket is the record; the media bucket stays the delivery
   host.** Buffer, Instagram and TikTok fetch a video by public URL, sometimes
   days after it is queued. The internal bucket blocks public access, and a
   presigned URL from an SSO login dies within hours. So finals are uploaded
   twice: to `screencast/` for the platforms (expiring) and to `socials/` to
   keep.
2. **Keep only what we need.** Sources, decisions, finals and what was
   published. Anything the app rebuilds from those — laid-out chapter renders,
   cut chapter videos, composition files, render intermediates, waveforms, the
   per-step copies of LLM calls — stays local.
3. **New projects only.** No backfill. A project is archived if it was created
   after this ships: New Project writes `"archive": true` to `session.json`,
   and sync does nothing for a project without it. Old projects stay local.
4. **Category first, then project.** The folder is the category's slug, and that
   slug is the one `src/category.rs` already files a video under everywhere: the
   Strapi blog category (`/blog/category/[slug]`), the YouTube playlist and the
   hashtags in the team template. One slug, one meaning.
5. **Categories wait on the blog alignment with Laura.** The proposed set is
   `education`, `go-to-market`, `ai-seo-automation`, `agents`; the blog has
   `ai-literacy`, `gtm`, `ai-powered-seo-geo`, `ai-powered-marketing`,
   `ai-powered-content-writing` and `Tools-comparison`. Once agreed, each is set
   up with the existing `category::set_up` (Strapi row, public playlist, team
   template entry), so the S3 folder, the blog category and the playlist are
   created together. A project with no category yet goes under
   `uncategorized/`.
6. **The project folder is the project id** (`2026-09-30_17-04-12`), never the
   title — the same identity the media bucket and every Buffer row use. The
   title lives in `project.json`. Changing a project's category moves its
   folder (S3 copy, then delete); nothing outside the bucket points at these
   keys, so nothing breaks.
7. **Notes are per version.** The speaking notes a version was recorded with
   and the critique of that version's take go under `notes/vN/`, so a new
   version's notes sit beside what the previous one said to change.
8. **An organised layout, not a mirror of the session folder.** The local
   folder is organised for the app; this is organised for people. `project.json`
   carries a file index (key → local path), so the mapping goes both ways.
9. **Format is a folder.** Wherever a file comes in both shapes, it is under
   `horizontal/` or `vertical/`, and `project.json` lists the project's formats.
10. **Exhaustive by construction.** The local → S3 mapping is a table in code.
    Every local file either maps to a key or is on an explicit skip list with a
    reason. A file that is neither is reported as unmapped, and the test fails,
    so a new feature cannot quietly stop being archived — or start uploading
    something nobody decided to keep.

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
  <category>/                       e.g. education | go-to-market | ai-seo-automation | agents | uncategorized
    <project>/                      2026-09-30_17-04-12
      project.json                  manifest: title, category, formats, status, versions, every link, file index
      plan/                         the idea, plan versions, video brief, rehearsal transcripts
      notes/vN/                     the speaking notes vN was recorded with, and the critique of vN
      recording/vN/                 raw camera, screen and audio per chapter
      transcripts/vN/               per chapter and whole, as JSON and text
      edit/vN/                      cut lists, layouts, outline, card, figures
      final/vN/horizontal/          longform.mp4
      final/vN/vertical/            chapter-NN.mp4 (the shorts), longform
      thumbnails/                   the approved set and its exports, brief, approval
      distribution/
        youtube/                    metadata (title, description, tags), title options, uploads ledger
        posts/vN/                   copy per video and platform
        buffer/                     what was queued, with Buffer post ids
        blog/vN/                    article, Strapi payload, artwork; published ledger
        substack/vN/                notes
        media-links/vN.json         the public URLs the platforms fetched
      analytics/                    Buffer numbers over time, reflect
      llm/calls.jsonl               every call: model, provider, prompt id + version, full prompt, output
      shorts/short-NN/              a short cut from this project, same layout
```

## Where every file goes

Paths are relative to the session folder locally and to
`socials/<category>/<project>/` in S3. `vN` is the recording version;
`NN` is a chapter.

**Project, plan and notes**

| Local | S3 |
|---|---|
| `session.json`, `category.json` | folded into `project.json` (raw copies kept beside it) |
| `plan/input.json`, `plan/current.json`, `plan/vN.json` | `plan/` |
| `plan/take-NN.transcript.json`, `plan/transcripts.jsonl` | `plan/takes/` |
| `video-brief.json` | `plan/video-brief.json` |
| `drafts/vN/chapter-NN.plan.json` | `plan/vN/chapter-NN.plan.json` |
| `notes/notes.json` | `notes/vN/speaking-notes.json` — captured when vN is rendered, so it is the deck vN was recorded with, not the rewrite for the next take |
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
| `drafts/vN/chapter-NN.layout.json` | `edit/vN/chapter-NN.layout.json` |
| `outline/vN/outline.json`, `card.json` | `edit/vN/outline.json`, `edit/card.json` |
| `figures/`, `figures.jsonl` | `edit/figures/` |
| `render/vN/horizontal/longform.mp4` | `final/vN/horizontal/longform.mp4` |
| `render/vN/vertical/chapter-NN.mp4`, `longform.*` | `final/vN/vertical/` |
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
| `.blog.html`, `.video.html`, `.youtube.html`, … (dot-html at the root), `notes/notes.html`, `blog/vN/preview.html` | rendered views, rebuilt from the JSON |
| `plan/.wrote-deck`, `.DS_Store`, `node_modules/` | markers and junk |
| `config.json`, `logs/`, `strapi-library.json` | one machine's settings, logs and cache |
| `~/.stream-recorder/notes/standalone/` | one speaking-notes deck from 2026-08-13, before notes belonged to a project; nothing writes there now |

## `project.json`

Rebuilt on every sync from the ledgers, so it is never edited by hand:

```json
{
  "project": "2026-10-09_15-00-00",
  "title": "Agents that file their own tickets",
  "category": "agents",
  "formats": ["horizontal", "vertical"],
  "status": "published",
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
{ "date": "2026-10-14", "category": "agents", "project": null,
  "title": "Agents that file their own tickets", "format": "vertical",
  "platform": "linkedin", "status": "planned", "links": {} }
```

- **Planned** rows are written by people (and later by a Calendar tab) before a
  project exists; `project` is filled in when it is recorded.
- **Scheduled** and **published** rows come from each project's ledgers on
  sync. `schedule.jsonl` records when a post was queued, not when it goes out,
  so the due time is read from Buffer when analytics polls.

Defining the cadence per category (how many of each format, which days) is its
own plan; this one reserves the folder and the row shape.

## How it gets there (saaga-social-flow)

1. **`src/archive/layout.rs`** — the mapping and skip tables above as one pure
   function, local path → key, skip (with reason) or unmapped. Tested against a
   fixture session that has every file kind.
2. **`src/archive/sync.rs`** — walk the session, upload what changed, write
   `project.json`. "Changed" comes from a local `.archive.json` (size, mtime,
   SHA-256 per path): with KMS encryption an S3 ETag is not an MD5, so S3
   cannot tell us. Large files go multipart through the code
   `distribute::s3` already has. Media goes up with storage class
   `INTELLIGENT_TIERING`.
3. **Settings:** `INTERNAL_BUCKET=saaga-internal-dev`,
   `INTERNAL_PREFIX=socials` in `.env` (not secret), the same
   `AWS_PROFILE=dev` login. Off while unset.
4. **Which projects:** New Project writes `"archive": true` to `session.json`;
   sync skips any project without it.
5. **When it runs:** small files (JSON, Markdown, images) after every step that
   writes them: Plan, Critique, Render, Thumbnail approve, Titles, Posts, Queue
   to Buffer, YouTube upload, Blog publish, Substack, analytics poll. Raw takes
   and finals in a background queue after Render, with progress in the status
   bar. Never fatal: a failure is a status line and the next sync retries. An
   expired SSO login gets the same hint sops gives.
6. **Project tab:** an **Archive** row showing the category folder, last sync
   and anything unmapped, with an **Archive now** button. Changing the category
   moves the folder.
7. **Team template:** read and write `socials/team/templates.json` in the
   internal bucket, falling back once to the old key.
8. **Global files:** prompts and references sync when they change.

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

Per project, from the sessions so far: raw camera, screen and audio about
150 MB a chapter, the horizontal longform about 500 MB, plus the shorts.
Call it 1–1.5 GB for a four-chapter video, a few cents a month each.

## Phases

1. Terraform 1–4 and the KMS check.
2. Align the blog categories with Laura; set up the agreed ones.
3. `layout.rs` with the exhaustive test, then `sync.rs` and the `archive`
   flag on New Project.
4. Automatic sync after each step, the Project tab row, category moves.
5. Team template move, global prompts and references.
6. Calendars: the format here, then the cadence plan and a Calendar tab.

## Open decisions

1. **The category set and the old blog categories** — with Laura: which map,
   which retire, and whether posts already under `gtm`, `ai-literacy` and the
   rest are re-filed in Strapi.
