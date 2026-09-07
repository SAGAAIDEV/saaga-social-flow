# Substack notes from the longform

## What this is

A stage that reads the longform's transcript, notes and titles and returns
**writing notes** — beats, quotes, title options, timestamps — that get typed by
hand into Substack. Not a finished essay to paste.

That distinction is the whole design. Everything else in `src/posts/` writes
final copy that goes out through a machine (Buffer, the YouTube Data API), so it
is graded on being sendable. This is graded on being *typeable from*: a paragraph
you have to delete before you can start writing is worse than a one-line beat.

## Where it sits

Today the pipeline after the render is:

```
Post (posts.json) ──► Distribute (links.json) ──► Schedule (Buffer) ──► queue
                 └──► YouTube (publish/, direct upload)
```

`src/publish/mod.rs` already made this exact split once, and its reasoning
applies again verbatim: Buffer "accepts one asset and a little metadata", which
suits many short cuts, and does not suit one long thing that wants its own
treatment. Substack is a third destination with no API in this loop at all — the
transport is a human at a keyboard.

So this is a sibling stage, not a platform inside `posts.json`:

```
Post ──► Distribute ──► Schedule
    ├──► YouTube
    └──► Substack (substack/vN/, notes to type from)
```

**Why not just add `"substack"` to `posts::generate::SYSTEM_PROMPT`.** Three
reasons, each of which has already bitten something in this repo:

1. One system prompt cannot serve a 280-character hook and a 1,200-word essay
   outline. Tuning either degrades the other, and there is one overlay file per
   prompt id — so you could never adjust the Substack voice without touching the
   voice of eight social platforms.
2. `schedule::plan` would build a row for it and `channels::resolve_channel`
   would fail it with "no substack channel connected to Buffer" on every plan —
   a permanent false negative in the review surface.
3. `PlatformPost` is `{title, content, tags}`. Notes are sections, beats and
   quotes. Flattening that into one `content` string throws away the structure
   that makes it typeable.

## Output

`{root}/substack/v{N}/` — a stage folder like every other, so it follows the
open version (`Session::substack_dir()`, mirroring `posts_dir`).

| File | For |
|---|---|
| `substack.json` | the structured notes, incl. prompt attribution |
| `notes.md` | the same thing as plain text, to keep open beside the editor |

`substack.json` carries `version`, `prompt_version` and `prompt_hash` exactly as
`PostsManifest` does, so a Substack post can be attributed to a prompt version
later rather than to a timestamp.

## Schema

Two types, split the way `PostsExtraction` / `PostsManifest` are split: the model
is never asked to fill in bookkeeping it cannot know.

```rust
// What the model returns.
struct NotesExtraction {
    titles: Vec<String>,      // options, not a choice
    subtitles: Vec<String>,
    hooks: Vec<String>,       // opening lines
    sections: Vec<ExtractedSection>,
    quotes: Vec<ExtractedQuote>,
    close: Vec<String>,
}
struct ExtractedSection { heading: String, beats: Vec<String>, chapter: Option<u32> }
struct ExtractedQuote   { text: String, chapter: Option<u32> }

// What lands on disk.
pub struct SubstackNotes {
    pub version: Option<u32>,
    pub prompt_version: Option<u32>,
    pub prompt_hash: String,
    pub titles: Vec<String>,
    pub subtitles: Vec<String>,
    pub hooks: Vec<String>,
    pub sections: Vec<Section>,   // + timestamp, computed here
    pub quotes: Vec<Quote>,       // + timestamp, computed here
    pub close: Vec<String>,
    pub links: Vec<Link>,         // computed here
}
```

`timestamp` and `links` are filled in Rust, never asked of the model — it has no
way to know where a chapter starts in the longform or what URL the upload got,
and asking would invite it to invent both.

The output *shape* stays out of the prompt text and lives in the `JsonSchema`,
for the reason `posts/generate.rs` documents: an overlay that rewrites the voice
must not be able to break parsing.

## The prompt, and how it stays adjustable

New id `substack.notes`, registered in `agent::prompt` beside `NOTES`, `TITLES`
and `POSTS`, with the builtin below compiled into `src/substack/generate.rs`.

### The gap this has to close

`prompt::resolve` reads `{project_root}/prompts/{id}.txt`. Every recording is a
**new timestamped project folder** (`~/.stream-recorder/sessions/{id}/`), so a
per-project overlay is thrown away the moment you start the next video. As
written today, "adjust as time goes on" means re-editing the prompt for every
single video, forever.

`thumbnail/references.rs` already solved the same problem for reference images,
and states the principle: "Global rather than per-project on purpose — a
channel's look is the point, and a per-project folder would drift."

### The change

`prompt::resolve` gains a middle tier:

1. `{project_root}/prompts/{id}.txt` — this project's overlay. What Reflect
   writes; still wins, because a deliberate per-project override must.
2. `~/.stream-recorder/prompts/{id}.txt` — **the standing house prompt**.
   `STREAM_RECORDER_PROMPTS` overrides the directory, mirroring
   `THUMBNAIL_REFERENCES`.
3. The compiled-in builtin.

Version lookup checks the project ledger, then a global
`~/.stream-recorder/prompts/versions.jsonl`. A file in neither ledger resolves
`version: None` by hash — which is already the module's documented, honest answer
for a hand-edited overlay, and the hash means attribution never fails.

**Flag:** this is shared code. It changes resolution for `notes.slide_deck`,
`titles.chapter_cards` and `posts.social` too. That is the point — they have the
same problem — but it needs a test pinning that a project overlay still beats a
global one, and that an absent global file changes nothing.

### Editing it

The pane shows the resolved prompt's path and version, with an **Edit prompt**
button that creates `~/.stream-recorder/prompts/substack.notes.txt` from the
builtin if it is missing, then opens it (`open -t`).

Deliberately not seeded automatically on first run. `config.rs` documents what
happens when defaults get frozen into a user's file behind their back — the
`RETIRED` model list exists because of exactly that. Copying the builtin out is a
choice to stop tracking it, so it costs a click.

### Draft builtin

```
You turn a recorded technical video into WRITING NOTES for a Substack essay.

You are not writing the essay. Someone types it by hand from what you return,
and anything that reads as finished prose is deleted rather than typed. So:
beats, not paragraphs. One line each, naming the thing to say and — where the
transcript has one — the concrete detail that proves it: a number, a name, a
before and after, a mistake and what it cost. A beat that could have been
written without watching this particular video is a bad beat.

Work only from the transcript. Do not invent facts, numbers, tool names,
timings or outcomes that are not in it. If the video does not support a
section, return fewer sections.

titles: 3 options. Plain and specific. No "the ultimate guide", no
  clickbait framing, no clever-colon constructions.
subtitles: 3 options. A Substack subtitle is a sentence, not a slogan.
hooks: 3 opening lines. The first sentence is the whole email preview, so
  each is a concrete claim or a scene — never a throat-clear, never
  "in this post I'll".
sections: 4 to 7, in the order the video makes them. The heading is a
  working heading; the writer will rewrite it. 2 to 5 beats each. Set
  `chapter` to the chapter number the material came from.
quotes: lines the speaker actually said, verbatim from the transcript,
  worth reproducing as a pull quote. Trim to a sentence or two, and change
  nothing else. If nothing is quotable, return none — never paraphrase
  something into a quote.
close: 2 options for the last beat. What the reader does or thinks next.
```

A test pins that the builtin says notes-not-prose and carries no output schema,
matching `the_builtin_prompt_no_longer_carries_the_output_schema`.

## Inputs

| Source | Path | Gives |
|---|---|---|
| Deck | `{root}/notes/notes.json` | project title, chapter titles, points |
| Titles | `titles/vN/titles.json` | better working headings when generated |
| Transcript | `notes::collect_completed(&session.dir)` | the words, per chapter |
| Durations | `edit/vN/chapter-NN/chapter-NN-horizontal.mp4` | chapter offsets |
| Links | `distribute/vN/links.json`, `{root}/youtube.jsonl` | mp4 + watch URLs |

Transcript volume is the same load `posts` already sends, so no new problem.

### Timestamps

`compose::prepare` builds the longform as: chapter 1 body, then a
`CARD_SECONDS`-long title card before each later chapter, then that chapter's
body. So the offset of chapter *n* is a pure function of the earlier chapters'
durations — testable with no ffprobe in the test:

```rust
fn offsets(durations: &[(u32, f64)], card: f64) -> Vec<(u32, f64)>
```

Durations come from `cut::probe_duration_seconds` on each chapter's horizontal
cut. Absent cuts mean absent timestamps, not a failure — the notes are still
typeable without them.

Worth having beyond this stage: the same offsets are what YouTube chapter
markers in the description need.

## UI

A **Substack** tab, immediately after Post, as a `WebPane` — the Review /
Reflect / Thumbnail pattern. Not a native form: the content is nested (sections
containing beats), which `PostsForm`'s flat scroll of editors cannot express, and
panes are re-rendered whole so there is no state to fall out of step with disk.

Contents:

- Header: resolved prompt version + path, **Edit prompt**, a model dropdown
  (`data-send-change`, persisted to config like the thumbnail pane's).
- **Generate Notes** button, gated (below).
- The notes: title options, subtitle options, hooks, then sections with their
  beats, quotes with their timestamps, close options, links.
- Copy buttons — see below.
- **Open notes.md**.

### Copy

Nothing in this codebase touches `NSPasteboard` yet. Add one `WebEvent` variant:

```rust
CopyText { text: String }  →  UiEvent::CopyText(String)
```

The app writes it to `NSPasteboard::generalPasteboard()` as
`NSPasteboardTypeString`. Native rather than `navigator.clipboard`, which is
unreliable for a `file://` page in `WKWebView`.

One variant covers every button, through the existing `data-send` delegation —
copy-all, copy a title, and (the one that actually matters when hand-typing)
copy a quote verbatim, so the one thing that must not be retyped from memory is
not retyped from memory.

### Gate

`Stages.substack`, needing only recorded chapters — same as `posts`, and for the
same reason `stage.rs` gives: gate on what the code actually reads, not on taste.
Timestamps and links enrich when present and are silently absent otherwise.

## Files (as built)

Split by responsibility, each well under 500 lines:

```
src/substack/mod.rs       spawn + run orchestration, SubstackEvent, edit_prompt
src/substack/schema.rs    types, save/load, the notes.md writer
src/substack/generate.rs  builtin prompt, extraction types, user prompt
src/substack/context.rs   transcripts, titles, chapter offsets, links
src/substack/pane.rs      view model for the template
src/ui/templates/substack.html
```

Touched:

| File | Change |
|---|---|
| `main.rs` | `mod substack;` |
| `session.rs` | `substack_dir()` |
| `stage.rs` | `Stages.substack`, `Busy.substack` |
| `hotkeys.rs` | `Action::GenerateSubstack`, `Action::EditSubstackPrompt` |
| `agent/prompt.rs` | `SUBSTACK` const, `builtin()` arm, **the library tier** |
| `agent/reflect.rs` | `KNOWN_PROMPTS` + `substack.notes` |
| `reflect/mod.rs` | the live `substack.notes` preamble joins the corpus |
| `ui/render.rs` | register `substack.html` |
| `ui/web.rs` | `GenerateSubstack`, `EditSubstackPrompt`, `CopyText` |
| `ui/mod.rs` | the tab, `copy_to_pasteboard`, `set_substack_status` |
| `app/mod.rs` | pane, `drain_substack`, handlers, repaint with the other views |
| `posts/generate.rs` | its overlay test made hermetic against the library |

## Stages of work

All four landed together; they are listed in the order they were built, and each
was green before the next started.

1. **Generator, headless.** `schema` + `generate` + `context` + the `notes.md`
   writer + the prompt id. No UI, verified by unit tests.
2. **The library tier** in `agent::prompt`, with the precedence tests. Separate
   because it is shared code, so a regression there is attributable.
3. **The tab.** Template, `WebPane`, pasteboard copy, gate, wiring.
4. **Enrichment.** Chapter offsets → timestamps, links from `links.json` and
   `youtube.jsonl`, Reflect registration so the loop can propose rewrites to
   `substack.notes` like it does for `posts.social`.

## Tests

House style — named as sentences, each pinning a failure that would otherwise be
silent.

- `notes_round_trip` — `substack.json` saves and loads unchanged.
- `the_builtin_prompt_asks_for_beats_not_prose`, and carries no output schema.
- `a_section_with_no_beats_is_dropped` — the truncated-response guard that
  `a_post_with_no_body_is_dropped` exists for.
- `a_quote_keeps_the_words_it_was_given` — no trimming beyond whitespace; a
  "quote" that has been cleaned up is not a quote.
- `a_project_overlay_still_beats_the_global_one`.
- `an_absent_global_prompt_changes_nothing` — the no-op case for the three
  existing prompt ids.
- `chapter_offsets_account_for_the_title_cards` — including that chapter 1 has
  no card in front of it.
- `notes_markdown_is_typeable` — headings, beats as bullets, quotes marked.
- `the_pane_renders_sections_beats_and_quotes` (in `ui::render`'s suite).

## Decisions taken

1. **Its own tab**, sitting between Post and Review. It is a separate reader with
   a separate prompt, and the Post tab is a flat scroll of editors that cannot
   express sections containing beats.
2. **No model of its own.** It runs on the Post tab's model and provider. This is
   a post step, so it is configured where the other post step is — a second
   dropdown would be one more thing to keep in step for no decision anyone has
   asked to make differently yet. `config.substack_model` is the change to make
   if that stops being true.
3. **Longform only.** Per the ask. Chapters already have their own copy.

## Still open

- Nothing reads `substack.json` downstream, by design. If that changes — an
  archive, a second destination — the prompt attribution is already on the file
  and does not need adding later.
- `reflect::validate` cannot generation-check a `substack.notes` rewrite. It says
  so ("not generation-checked — review the diff on its merits") rather than
  implying a test that did not run, which is the same treatment titles and notes
  get. A cheap check would be word counts per beat.
