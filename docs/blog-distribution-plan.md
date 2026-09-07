# Blog distribution — the longform as a `/education` video post

A `blog` stage that takes the finished longform and creates a **Video Post** in
Strapi, which renders at `saagasolve.com/education/{slug}`: the YouTube embed, a
chapter timeline, an SEO article body, and a thumbnail.

Unlike [Substack notes](./substack-notes-plan.md), this one *is* distribution —
it writes a public page over the network, so it needs a ledger, a gate, and a
draft-first default.

**Scope: the longform only.** The vertical chapter cuts are Buffer's job and
never reach the blog. One video post per longform, so one row in the ledger per
project version — the `videoChapters` below are chapters *within* that one
video, not the shorts cut from it. Two things follow. The blog stage does not
depend on `crate::distribute` or `S3_BUCKET` at all: the video is already on
YouTube and the thumbnail goes straight to Strapi's own `/api/upload`, so
nothing here needs a public S3 URL. And every input is longform-shaped —
`render/horizontal/longform.mp4` for the duration, the `publish` ledger for the
URL — which is what makes §5's gate depend on `publish` rather than `render`.

---

## 1. Where everything lives

Three repos are involved, and the pieces are not all on their default branches.

| What | Repo / path | Branch state |
|---|---|---|
| `/education` listing + detail page | `saaga-landing` `src/app/(app)/education/` | **`origin/dev` only** — absent from `origin/main` |
| Video post components | `saaga-landing` `src/components/organisms/education-{content,detail}/` | `origin/dev` |
| Strapi read client | `saaga-landing` `src/lib/strapi/video/{fetch,types}.ts` | `origin/dev` |
| `video-post` collection type | `strapi-cms` `src/api/video-post/content-types/video-post/schema.json` | **`origin/main`** (merged, `cbeb7c1`) |
| `content.video-chapter` component | `strapi-cms` `src/components/content/video-chapter.json` | `origin/main` |
| `education-category` collection | `strapi-cms` `src/api/education-category/` | `origin/main` |
| **Working Python publisher (prior art)** | `saaga-martech/posts/platforms/strapi.py` + `distribute.py::_publish_strapi` | on disk |
| **Working draft generator (prior art)** | `saaga-martech/posts/blog.py` | on disk |

There are **two** `/education` implementations in `saaga-landing` history. The
older one (`feature/video-post`, local) rendered an AssemblyAI transcript with
speaker lines. The newer one (`origin/dev`) is a redesign that reads a
first-class `videoChapters` component instead and **does not render the
transcript at all**. `origin/dev` is what the deploy workflow ships to the `dev`
environment, so it is the contract to build against — but see §7.

---

## 2. The field contract

`video-post` on `strapi-cms@main`, and where each field comes from:

| Strapi field | Req | Source in stream-recorder |
|---|---|---|
| `title` | ✔ | generated (SEO title), falling back to `session.title()` |
| `slug` | ✔ (uid) | slugified from the generated title |
| `date` | ✔ | today, as `YYYY-MM-DD` |
| `shortAndMetaDescription` | ✔ | generated, 140–160 chars |
| `video` → `{url, caption}` | ✔ | `publish::load(session)` newest `Upload.url`; caption generated |
| `thumbnail` (media) | ✔ | `thumbnail::schema::active()` → `POST /api/upload` → media id |
| `duration` (int ≥1) | ✔ | `edit::cut::probe_duration_seconds(render/horizontal/longform.mp4)` |
| `videoChapters[]` → `{name, startOffset, endOffset}` | – | **synthesized** from the cuts + approved card titles — §3 |
| `content` (dynamiczone) | – | generated `content.text` / `content.quote` / `content.table` — §4 |
| `isFeatured` (bool) | – | `false`. Pinning the featured slot is an editorial call, not a pipeline one |
| `author` (rel) | – | looked up by name: **"Andrew Melnychuk-Oseen"** — new, see §6 |
| `educationCategory` (rel) | – | **"AI Powered Marketing"** — the one value the `/education` filter offers |
| `category` (rel) | – | legacy blog Category, kept for back-compat |
| `transcript` (json) + `transcriptProvider` | – | still on the schema; `origin/dev` ignores it. Send it anyway — §3 |
| `magicLinkCta` | – | out of scope for v1 |

**The video URL.** `getEmbedUrl` on `dev` matches `youtube.com/watch?v=`,
`youtube.com/embed/` and `youtu.be/` against an 11-character id — so the plain
watch URL that `publish::Upload` already stores works as-is, no normalisation
needed. (The legacy Python narrowed it to `youtu.be/<id>`; harmless, but not
required.) What matters is that it **must be a YouTube URL at all**: a
non-matching string makes the player render nothing, producing a video post
with no video and no error anywhere.

---

## 3. Chapters and the offset shift

This is the part with a real correctness trap in it.

stream-recorder transcribes **per chapter**. Each `chapter-NN.transcript.json` is
`{status, text, words[{text, start, end, confidence}]}` with timestamps in
milliseconds **starting from zero for that chapter**. It does not request
`auto_chapters` or `speaker_labels`, so there are no AssemblyAI chapters or
utterances to forward.

The longform, however, is `chapter-01`'s body, then a title card in front of
every *later* chapter, then that chapter's body. So chapter 2's words claim
`start: 0` while sitting minutes into the video. Forwarding them unshifted
would put every chapter marker on the wrong frame — and quietly, since nothing
validates a timestamp.

`substack::context::offsets(durations, compose::CARD_SECONDS)` already computes
exactly the right start for each chapter, including the detail that chapter one
has no card in front of it. Reuse it (§5 moves it somewhere both stages can
reach).

**`videoChapters`** — one per recorded chapter:
- `name` — approved card title (`titles::load(…).title_for(n)`) → notes-deck
  chapter title → `"Chapter N"`. Same precedence the Substack notes use.
- `startOffset` — the computed offset, **in whole seconds** (the component's own
  description says seconds; the AssemblyAI transcript below is in milliseconds —
  do not mix them).
- `endOffset` — `startOffset + that chapter's own duration`.

**`transcript`** — worth sending even though `origin/dev` ignores it: it is a
`json` column, it costs one field, `feature/video-post` rendered it for SEO, and
regenerating it later means re-running the whole pipeline. Shape it like
AssemblyAI so either frontend can read it: `text` concatenated, `words` with
offsets shifted into longform time, `chapters` in **milliseconds**,
`audio_duration` from the probe. Set `transcriptProvider: "assemblyai"`.

If any chapter's duration fails to probe, emit **no** chapters rather than a
partial list — the same rule `substack::context::timestamps` already follows,
for the same reason: a partial list silently shifts every later offset, and a
wrong timestamp is worse than a missing one.

---

## 4. The article body, and a prompt to tune

`content.text.textBodyHtml` is a CKEditor HTML field. Substack notes are beats
for hand-typing and are the wrong shape here — the blog is a published artifact
that has to read as finished prose. So this needs its own generation.

New prompt id `prompt::BLOG = "blog.article"`, resolved through the same tiering
the Substack work added — project overlay → `~/.stream-recorder/prompts/blog.article.txt`
→ builtin — so it is tunable per §"adjust as time goes on" and survives the next
project folder. Add it to `reflect::KNOWN_PROMPTS` and the corpus loop so Reflect
can propose rewrites.

Its input is the same longform context the Substack stage builds (transcripts,
notes, chapter titles), which is why §5 lifts that builder out.

Draft builtin — the legacy `posts/blog.py` rules, kept because they encode real
downstream constraints, plus two that branch reading turned up:

```
You write the article that accompanies a recorded technical video on
saagasolve.com.

The video is embedded at the top of the page. Never open with "in this video"
and never address the reader as a viewer — the article has to stand on its own
if the embed fails, and it is what search engines read.

Work only from the transcript. Do not invent facts, numbers, product names or
outcomes that were not said. Treat the transcript as research rather than a
script: drop filler, false starts and repairs, and write in the speaker's voice
without reproducing their hesitations.

title — under 200 characters, primary keyword near the front, no clickbait.

description — 140 to 160 characters. It is both the meta description and the
teaser on the listing page, so it must read as a complete promise and end in a
full stop. Primary keyword inside the first 60 characters.

keywords — 5 to 10 phrases someone would actually type into a search box.

caption — under 120 characters, shown under the embed. Say what the video
shows, not what the article argues.

sections — 3 to 6, forming the spine: an opening that states the problem, two
to four body sections, and a close. Every body section starts with an H2.
Inside the html use only <p>, <h2>, <h3>, <ul>, <ol>, <li>, <strong>, <em> and
<a href>. Never <h1> — the page supplies it — and no inline styles or classes.
The H2 text becomes the page's table of contents, so each one has to make sense
read on its own, out of order.

quotes — 0 to 2, for a line worth remembering. The highlight must be a literal
substring of the quote; it renders in brand orange, and a highlight that is not
found renders nothing.

table — at most one, and only where the material genuinely compares options or
summarises structured data. Never force prose into a table. 2 to 5 columns, 2 to
8 rows, cells under 60 characters, each cell starting with a capital letter, and
no commas inside a cell — the value is joined on commas downstream.
```

The two additions: the H2-as-table-of-contents rule is real —
`education/[slug]/page.tsx` on `dev` builds the ToC by regexing `<h1|h2>` out of
`textBodyHtml` — and the no-commas rule is real, `_to_strapi_blocks` flattens
table cells with `", ".join`.

Length rules stay prose in the prompt rather than `JsonSchema` constraints, then
get trimmed in Rust after extraction. The legacy code learned this the hard way
(`_normalize_lengths`): models treat schema length bounds as advisory and a
too-long string becomes a hard validation bounce instead of a slightly long
title.

---

## 5. Shape: a stage, like `publish`

Modelled on `crate::publish` (the YouTube upload), not on `crate::schedule` —
one artifact, one POST, its own client and its own ledger.

```
src/blog/
├── mod.rs        stage entry: BlogEvent, spawn_publish, edit_prompt
├── schema.rs     Article (the generated draft) + Post (the ledger row)
├── generate.rs   SYSTEM_PROMPT + the JsonSchema extraction
├── chapters.rs   videoChapters + the synthesized transcript (pure, testable)
├── strapi.rs     the REST client: upload, create, verify relations, publish
└── pane.rs       what the Blog tab shows
```

Plus `src/ui/templates/blog.html`.

**One refactor:** `src/substack/context.rs` becomes `src/longform.rs`. It builds
"the longform's transcripts, notes, chapter titles and offsets", which is now
input to two stages; leaving it under `substack/` would make the blog stage
import from a sibling feature. Tests move with it.

**Ledger** `blog.jsonl`, at the project root beside `youtube.jsonl`, holding
`{document_id, slug, url, admin_url, video_id, published, created_at}`. The
ledger — not the API — is what stops a double post: Strapi will happily accept
the same article twice under `slug-2`, and nothing here would ever clean up the
second one. Key on the YouTube video id, so a re-upload is legitimately a new
post and a second press on the same one is refused with the existing URL.

**Gate.** `Stages.blog` is `Missing` until:
- the longform is on YouTube (`publish::load` has a row) — because `video.url`
  must be a YouTube URL, and this is the one stage that hard-depends on
  `publish` rather than on `render`
- a thumbnail is activated — `thumbnail` is a required field, and the upload is
  a separate call that cannot be retrofitted by the pipeline
- `STRAPI_API_URL` and `STRAPI_API_TOKEN` are set

**Draft-first.** The legacy Python passed `publish=True`. Recommend defaulting
to draft here, with an explicit second button to publish: this is the only
target that puts machine-written prose on the marketing site under a permanent
URL, and the Review tab established that renders get looked at before they
leave. The ledger records which state it is in, so publishing later is a cheap
follow-up rather than a re-post.

**Client details worth porting verbatim** from `posts/platforms/strapi.py`,
because each one is a bug someone already paid for:
1. Relations are set with a bare numeric id on `POST`, which some Strapi
   v5 + token-permission combinations **silently drop**. Ask for
   `?populate[0]=category&populate[1]=educationCategory` on create, check the
   echo, and `PUT` the relation if it came back null.
2. Lookups use `$eqi` (case-insensitive) and try `name` then `slug`, so
   "Education" matches an entry stored as "education".
3. ~~`draftAndPublish` is on, so creating is two calls: `POST /api/video-posts`
   then `POST /api/video-posts/{documentId}/actions/publish`.~~ **Wrong — see
   §10.** That route does not exist in Strapi 5's REST API.
4. A failed publish is **not** a failed create — report "created as draft" with
   the admin URL, not an error, or a retry duplicates the entry.

---

## 6. Set the author

The legacy publisher never sent `author` — grepping `distribute.py` for it
returns nothing. Every video post therefore has no byline, and the
`VideoObject`/`Person` structured data that the E-E-A-T work on the landing
side was built for has nothing to populate. `dev`'s fetch populates
`author.profilePicture`, so the design expects one.

Worth closing in the port: look the author up by name the same way the
categories are looked up, with the name coming from `config.yaml`.

---

## 7. Blockers — resolve before writing code

These change *where* this posts, so they come first.

1. ~~`/education` is not on `origin/main`.~~ **Resolved: it is live in
   production.** `https://saagasolve.com/education` renders 10 video posts. The
   local `origin/main` ref was stale. Slugs are bare (`/education/ai-memory-tool`),
   and the category filter currently offers one taxonomy value, "AI Powered
   Marketing".
2. ~~Which Strapi host?~~ **Resolved: `https://cms.saagasolve.com`** — the
   production CMS is the target. `stream-recorder/.env` still points
   `STRAPI_API_URL` at `http://localhost:1337` and must be repointed.
   `GET /api/video-posts` there returns **403 unauthenticated**, so the token is
   required for reads as well as writes — a misconfigured token fails at lookup,
   before anything is created.
3. **The API token needs the right scopes** — this is the "add an API key" step.
   A custom token with: `create` on `video-post`, `update` on `video-post` (the
   relation-retry `PUT`), `create` on `upload` (the thumbnail), `find` on
   `category`, `education-category` and `author`, and permission on the
   `video-post` publish action. A read-only token gets through the lookups and
   fails on create.
4. **`education-category` rows must already exist** in Strapi admin. A lookup
   miss is not an error — the post uploads and lands uncategorised, which is
   easy to miss until someone filters the listing.
5. **`strapi-cms` local checkout is behind.** `feature/magic-link-cta` locally
   sits at `b169bcb`; `videoChapters` and `isFeatured` arrived in `a253709` and
   are on `origin/main` as of `cbeb7c1`. Build against `origin/main`.

---

## 8. Order of work

1. Answer §7.1–7.2 — the target environment decides the rest.
2. `src/longform.rs` — move `substack/context.rs`, tests with it. Pure refactor,
   green before and after.
3. `blog/chapters.rs` — `videoChapters` + the synthesized transcript, with the
   offset shift and the all-or-nothing rule. No network, fully unit-testable;
   the highest-risk logic, so it lands first and alone.
4. `blog/generate.rs` + `prompt::BLOG` + the library overlay + Reflect wiring.
   Also pure — extraction shape, trimming, dropped-block rules.
5. `blog/strapi.rs` + `blog.jsonl`. Port the four hard-won behaviours from §5.
6. `blog/mod.rs`, `Stages.blog`, `Busy.blog`, `session.blog_dir()`.
7. `blog/pane.rs`, `blog.html`, the Blog tab, hotkey actions, `app` handlers.
8. First run against the `dev` environment as a draft, read the page, then
   publish.

## 9. Decisions taken

Answered 2026-08-18, and now settled:

- **Target** — `https://cms.saagasolve.com`, the production CMS. `/education` is
  live there with 10 posts.
- **Author** — `Andrew Melnychuk-Oseen`, looked up by name. The existing library
  is bylined to a different author; pipeline posts carry Andrew's byline.
- **Education category** — `AI Powered Marketing`, the only taxonomy value the
  filter bar currently offers.
- **Article length** — the short draft: 3–6 sections, no images or code blocks.
  The hand-written library runs 8–12 H2 sections, but a four-minute transcript
  cannot fill that without inventing material, which the prompt forbids. Editing
  up in Strapi is the intended path.
- **Publish mode** — **publish immediately**, matching the legacy Python
  (`publish=True`). Recommended draft-first and was overruled; the stage still
  records the admin URL in the ledger so a bad post is quick to find and fix.

## 10. As built

Landed 2026-08-18. `cargo test` — 671 passed, 0 failed; `cargo check --all-targets`
clean apart from two warnings that predate this work, both in `src/ops/graphs.rs`.

| File | What it does |
|---|---|
| `src/longform.rs` | **moved** from `src/substack/context.rs` — the shared longform context, now read by two stages. `Link` moved with it; `substack::schema` re-exports it |
| `src/blog/mod.rs` | the stage: `BlogEvent`, `spawn_publish`, the `blog.jsonl` ledger, `edit_prompt` |
| `src/blog/chapters.rs` | `videoChapters` (seconds) and the longform-timed transcript (milliseconds) from one pass |
| `src/blog/generate.rs` | `SYSTEM_PROMPT` and the `JsonSchema` extraction |
| `src/blog/schema.rs` | `Article`, `Block`, save/load, `slugify` |
| `src/blog/payload.rs` | the exact JSON Strapi receives, and the dynamic-zone mapping |
| `src/blog/strapi.rs` | the REST client: lookups, multipart upload, create, relation retry, publish |
| `src/blog/pane.rs` | what the Blog tab shows |
| `src/ui/templates/blog.html` | the pane |

Shared files touched: `main.rs`, `session.rs` (`blog_dir`), `stage.rs`
(`Stages.blog`, `Busy.blog`), `hotkeys.rs`, `ui/web.rs`, `ui/render.rs`,
`ui/mod.rs` (Blog tab, after YouTube), `app/mod.rs`, `app/startup.rs`,
`agent/prompt.rs` (`BLOG`), `agent/reflect.rs`, `reflect/mod.rs`.

### Decisions that changed during the build

- **Both taxonomies are set, not just `educationCategory`.** The deployed
  frontend filters its listing on `category.slug` but fetches the filter options
  from `/api/education-categories` — two different collections. Setting one
  leaves the post uncategorised on whichever half disagrees, so both get the
  same name and a miss costs only a log line.
- **The article body is `Vec<Block>` in published order**, not parallel lists of
  sections and quotes. The dynamic zone is ordered and mixed, and a quote belongs
  under the section it came out of.
- **The admin URL is copyable.** minijinja escapes `/` to `&#x2f;` in the
  displayed text, and the admin URL is the one you need in a browser to fix a bad
  post, so it carries a copy button with the raw value.
- **`OPENER_SECONDS` does not affect the offsets.** Checked rather than assumed:
  it is used only by the vertical talking-head composition, and it is a
  sub-parameter inside a chapter's own duration rather than added time. The
  horizontal longform is body-01, then card + body per later chapter, which is
  what `longform::offsets` already models.
- **A `slugify` off-by-one was fixed.** A cut landing exactly on a separator
  already ends on a whole word; trimming back there dropped one.

### Before the first run

1. ~~Point `STRAPI_API_URL` at `https://cms.saagasolve.com`.~~ **Done**
   (2026-08-29).
2. ~~Put a token in `STRAPI_API_TOKEN`.~~ **Done** — scopes verified against the
   live CMS: all four collections read `200`; `POST /api/video-posts` and
   `POST /api/upload` return `400 ValidationError` rather than `403`, and
   `PUT /api/video-posts/{id}` returns `404`, so create, upload and update are
   all permitted.
3. ~~Confirm the category and author exist.~~ **Superseded** — both are now
   picked from the CMS's own lists on the Blog tab rather than looked up by a
   compiled-in name. See §12.

## 11. Still open

- Whether to send `transcript` at all, if `/education` on `main` ends up being
  the `dev` redesign that ignores it. Cheap to send, cheap to drop.
- Whether `isFeatured` should ever be set by the pipeline. Currently `false`.
- `magicLinkCta` — the component exists and is clearly meant for this
  ("deep-links a viewer into a SAAGA demo"), but nothing in stream-recorder
  knows a demo URL yet.

---

## 12. The pickers, and the publish route that never worked

Landed 2026-08-29. `cargo test` — 721 passed, 0 failed.

Two things were wrong in what shipped on 2026-08-18, both found by reading the
live CMS rather than the schema.

### The byline and the taxonomy were compiled-in names

`author_name()` returned `"Andrew Melnychuk-Oseen"` and `category_name()`
returned `"AI Powered Marketing"`, and the second fed *both* taxonomy lookups.
Against the production CMS:

- there is no author called `Andrew Melnychuk-Oseen` — the collection holds
  seven SEO content staff — so the lookup missed and, because a missing author
  was one `eprintln`, every post would have gone up with no byline;
- `educationCategory` has exactly one row, `Education`. `AI Powered Marketing`
  exists only in the *blog* `category` collection. One string could never match
  both.

Now both are picked from the CMS's own lists on the Blog tab and remembered in
`config.json` as **ids**. Ids matter here beyond tidiness: the live collection
has two authors both named `Danish Rafique`, so a by-name match picks one
arbitrarily. `library::Library::by_name` refuses on ambiguity instead.

| File | What it does |
|---|---|
| `src/blog/library.rs` | **new** — the cached author / education-category lists at `~/.stream-recorder/strapi-library.json`, and `Entry::label()` |
| `src/blog/strapi.rs` | `list_authors`, `list_education_categories` |
| `src/blog/mod.rs` | `Chosen`, `chosen_author`, `chosen_education_category`, `select_*`, `spawn_refresh_library` |
| `src/config.rs` | `blog_author_id` / `_name`, `blog_education_category_id` / `_name` |
| `src/stage.rs` | a missing byline is now a gate, not a log line |

Three smaller decisions:

- **The legacy `category` is off unless `BLOG_CATEGORY` names one.** Ten of the
  eleven live video posts leave it null. The one that sets it is the one the old
  pipeline made.
- **Relation lookups moved ahead of article generation.** A byline that does not
  resolve should not cost an LLM call.
- **`pane::build` takes `cfg` and `library` as arguments.** Both live in `$HOME`
  rather than under a path the function is given, so loading them internally
  made the pane — and its tests — depend on the machine.

### `/actions/publish` does not exist

§5.3 was ported faithfully from the Python and was wrong. The live CMS answers
`POST /api/video-posts/{documentId}/actions/publish` with **405** and
`allow: HEAD, GET`: it is an admin Content-Manager route, not a REST one. Since
a failed publish is only a warning (§5.4), every post made this way would have
gone up as a **draft** reporting `created as draft (publish failed)` — and the
Python has the same bug.

Publishing over REST is `?status=published` on the write itself.
`@strapi/core@5.27.0`, `dist/services/document-service/repository.js`:

```js
async function create(opts = {}) {
    const queryParams = await pipe(..., setStatusToDraft(contentType), ...)(params)
    const doc = await entries.create(queryParams)
    if (hasDraftAndPublish && params.status === 'published') {
        return publish({ ...params, documentId: doc.documentId }).then(d => d.entries[0])
    }
    return doc
}
```

`update` has the identical tail. Both run `setStatusToDraft` first, so a write
**without** the parameter touches only the draft — which is why the relation
retry carries the flag too. Patching a dropped relation without it would fix the
draft and leave the live page still missing it.

`published` is now read off the returned document's `publishedAt` rather than
inferred from the flag that was sent. Asking to publish and being published are
different facts, and conflating them is what hid this for three months.

## 13. Still open

- `magicLinkCta`, unchanged from §11.
- Whether `isFeatured` should ever be set by the pipeline. Currently `false`.
- **`educationCategory` has one generic row.** `Education` is the only value, so
  every post files identically and the filter bar has one chip. Real rows
  (Tutorials, Product Demos, …) are an admin-side job; the picker will show them
  on the next Refresh with no code change.
- **An author row for Andrew.** Deliberately deferred — the picker works against
  the seven existing authors, and the gate now refuses to post without one.
- `screencast/.env` and `sales/business-scrape/.env` still carry the dead
  `localhost:1337` pair, and `screencast`'s own Python client has the same
  `/actions/publish` bug.
