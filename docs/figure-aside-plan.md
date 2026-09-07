# Figures as a break in the recording

## Context

A figure today (`src/figure/`) is a screenshot taken *while the video keeps
rolling*: ⌃⇧S shows a drag overlay, the drag fires `SCScreenshotManager`, and
the JPEG is filed with the chapter and offset it was taken at. The caption is
written later by a separate, manual "Write Blurbs" pass on the Blog tab, from
the picture plus ±25 s of whatever the chapter transcript happens to say around
that moment. The whole feature lives on the Blog tab.

That is the wrong phase. A figure is something the author *stops to show*. The
recording should pause, the author should snip the screen and then say — into
the mic, with nothing else recording — what the reader is looking at and why.
The screenshot and that explanation are one pair of data, and the explanation
is what the caption and the surrounding prose are written from. This plan moves
the feature into the record phase and rebuilds the pair around a **break**.

Three further corrections ride along, because they fall out of the same change:

- **Chronology.** The article prompt (`blog/generate.rs::build_user_prompt`)
  lists figures as a flat block *before* the transcript, keyed by a
  `ch 03 · 1:24` string, and leaves the model to correlate. With a break the
  order is exact: figure *k* closed chapter *N*, so it belongs between chapter
  *N* and *N+1*. The shared evidence (`src/longform.rs`) should carry that.
- **One size.** Figures are arbitrary rectangles capped at a 2400 px long edge,
  and the `width/height` the CMS is sent are recomputed at *publish* time from
  the snip rect × the current display's scale — publish from another monitor
  and they are wrong. Every figure becomes an aspect-locked drag resampled to
  one fixed size, with the size recorded on the ledger row as a fact about the
  file.
- **WebP.** Screenshots of text and UI are what figures are, and lossless WebP
  is the right codec for them: sharp text, usually smaller than a high-quality
  JPEG. ImageIO cannot encode WebP (`sips -s format webp` on macOS 15.6 writes
  nothing), so the encode goes through the `image` crate, which is already in
  the dependency graph.

## Decisions

1. **A break is a pause inside the chapter, not a boundary.** On the *drag* —
   not on the chord, so a cancelled overlay leaves the take untouched — the
   open chapter's writers stay installed and **pause**: buffers are dropped
   until the take picks back up, and from then on every buffer is **retimed**
   back by the length of the break (`capture/pause.rs`). The chapter comes out
   as one continuous file with the break simply absent — no new chapter, no
   new take, no frozen frame. The figure sits *inside* chapter *N* at the
   offset it was taken, which is where its moment is in the file and the
   transcript. Nothing in `drafts/vN` changes shape, so the cut and render
   pipelines need no change.
2. **The "different stream" is a second writer on the same mic, not a second
   capture session.** An audio-only `AVAssetWriter` (`.m4a`) hangs off the
   already-running `AvDelegate` as its own sink, fed audio and nothing else,
   beside the paused chapter state. The mic format is already locked and warm;
   `av_delegate.rs` documents the chapter of static a writer built against a
   cold, renegotiating mic once produced, and a fresh `AVCaptureSession` on the
   same device would walk back into it.
3. **⌃⇧S is a three-state toggle**: idle → overlay; overlay up → cancel; on a
   break → end the aside and **pick the take back up** where it left off. The
   chord is pressed while looking at another app, so it must also be the way
   back. `Stop` ends the break and finishes the chapter at the pause point;
   `New Chapter` ends the break and cuts; `Retake` is refused during a break;
   `Quit` finishes the aside the way it finishes a chapter.
4. **Blurbs are written automatically** when the aside's own transcript lands.
   The aside transcript is the primary context; the chapter's ±25 s is
   secondary. "Write Blurbs" survives as a retry for figures whose pass failed.
   The prompt id stays `blog.figure` so tuned prompts in the library are not
   orphaned.
5. **Capture and break status live on the Record tab; the review list stays on
   the Blog tab**, because that is where figures are used. The Blog tab loses
   its Capture button and each row gains the explanation text.
6. **What reaches the CMS is the image only** — via `blog::figures::upload`,
   for figures the article places — with alt, caption, width and height. The
   audio and its transcript are inputs to the writing, not content.

## Data model

`{project}/figures.jsonl` stays append-only. Three row kinds:

| row | appended when | carries |
|---|---|---|
| `capture` | the image file exists | `n`, `file`, `at`, `chapter`, `offset`, `rect`, **`width`, `height`** |
| `aside` | the `.m4a` has finished | `file` (the image, as the key), `audio` |
| `blurb` | a caption was written | `file`, `caption`, `alt` — last one wins |

`Figure` gains `width`/`height` (0 on rows written before this existed, in
which case consumers fall back to the old `rect × scale`), `audio`, and `said`
— the explanation, read from `figures/figure-NN.transcript.json` at load time
rather than copied into the ledger, so the transcript file stays the single
source of truth. The transcript is the same `{status, text, words}` shape every
chapter writes; `notes::transcribe::transcript_path` derives its name from the
audio's stem, and `closed_chapter_numbers` only scans `drafts/vN` for
`chapter-NN`, so an aside can never be mistaken for a chapter.

`Longform` (`src/longform.rs`) — the evidence Blog, Substack and Posts all read
— gets `ChapterContext.figures: Vec<FigureContext { n, moment, caption, said }>`
for the figures taken during that chapter, and `Longform.loose_figures`
for the ones no transcribed chapter claims (taken before recording started, or
after a chapter whose transcript failed). Only blurbed figures are offered,
the same rule `blog::figures::offers` already applies: a figure with no caption
would publish with an empty `<figcaption>`.

`figure::copy_into` already copies the whole `figures/` directory, so the
image, the audio and the transcript travel into a new version together.

## Stages

Each stage leaves the app working and the tests green.

### 1. Ledger and shared evidence — no behaviour change

- `figure/mod.rs`: `Row::Aside`, `width`/`height` on the capture row,
  `Figure.{width,height,audio,said}`, `pixel_size()`, `explained()`,
  `audio_path_for`, `append_aside`; `load()` folds the aside row and reads the
  transcript.
- `figure/shot.rs`: measure the encoded bytes (`thumbnail::still::jpeg_dimensions`)
  and put the size on the capture before it is announced.
- `notes`: a path-based transcript loader beside the chapter-numbered one.
- `longform.rs`: `FigureContext`, `ChapterContext.figures`,
  `Longform.loose_figures`, and a pure `attach_figures` with tests.
- `figure/pane.rs`, `blog/figures.rs`: prefer the recorded size, keep the
  fallback for old rows.

### 2. The break and the aside

- `capture/pause.rs` (new): `Pause { floor, paused_at, removed }` per writer
  state — `pause(now)`, `resume(now)`, `place(pts) -> Option<CMTime>` — and
  `retime(buffer, pts)`, which shifts a buffer's own first timing entry so an
  audio buffer keeps its per-sample duration. Late buffers from before a pause
  still land; late buffers from during one are dropped (the `floor` moves to
  each resume). Pure unit tests, plus a hardware test that records 1 s, breaks
  1 s, records 1 s and checks the file is 2 s long.
- `capture/av_delegate.rs`, `capture/screen_delegate.rs`: `AvState.pause` and
  `ScreenState.pause` replace the anchor check; appends go through `place`,
  retimed once anything has been removed. Composed H/V sinks take the placed
  time too. The delegate gains an `aside: Mutex<Option<AsideState>>` slot fed
  from the audio branch before the chapter lock — `install_aside` refuses a
  second one, `take_aside` hands the writer back.
- `router/mod.rs`: `pause_chapter()` / `resume_chapter()` feed both states
  from one real instant, the way `anchors()` opens them.
- `capture/av.rs`: `AudioWriter` + `create_audio_writer(audio_settings, path)`
  — `AVFileTypeAppleM4A`, audio input only, from the session's settled
  settings.
- `figure/aside.rs` (new): `Aside::start(&Connection, path)` anchors at
  `now_on(sync_clock)` and installs the aside sink; `Aside::finish(self)` takes
  it out, finishes, repairs the container metadata and hands the path back for
  the caller to transcribe — the same split `Router::finish_chapter` has, and
  what keeps the hardware smoke test from uploading audio.
- `app/clock.rs`: the record clock pauses with the chapter — `position()`
  reports file time, speech over the break is not the chapter's, the detail
  line shows the break running.
- Hardware smoke tests: the `.m4a` plays and `ffprobe` agrees with `elapsed()`;
  a paused-and-resumed chapter is the length of what was recorded.

### 2b. The image — aspect-locked, one size, WebP

- `figure/snip/mod.rs`: `aspect_rect` grows a **4:3** box from the press
  toward the pointer — the further axis sets the size, the other follows — and
  shrinks to fit the display rather than clamping an edge. The label shows the
  pixels under the box and an arrow when they will be scaled to the target.
- `figure/encode.rs` (new): `CGImage` → PNG (Core Image) → `image` crate →
  centre-crop to 4:3 → Lanczos resample to exactly **1600×1200** → lossless
  WebP → `figures/figure-NN.webp`. The PNG hop is deliberate: Core Image's
  bitmap row order is a convention, PNG is a format. Small drags are upscaled
  (a 1080p display cannot fit a 1600×1200 drag at 1×) and the label says so.
  `thumbnail::still` keeps its JPEG path; it is a different job.
- `width`/`height` on the capture row are now always set; `FigureMedia`, the
  pane and `blog/figures.rs` read them and `display_scale()` goes.
  `blurb/wire.rs`'s data-URL mime follows the file extension. Old `.jpg`
  figures keep resolving because `load()` uses the `file` in the ledger.

### 3. The gesture — pause on drag, aside, chord to pick back up

- `app/figures.rs`: `figure_snipped` becomes: read `clock.position()` → hide
  the overlay → fire the shutter → `router.pause_chapter()` + `clock.pause()`
  → `Aside::start`. The second chord → `aside.finish()` → append the `aside`
  row → spawn its transcript → `router.resume_chapter()` + `clock.resume()`.
- A figure taken while nothing is recording still works: nothing to pause,
  the aside still records.
- `App.aside: Option<Break>`; every path that finishes a chapter
  (`finish_open_chapter`) ends a break first without resuming — the file ends
  at the pause point; New Chapter ends it and cuts; Retake is refused during
  one; Quit ends it before stopping capture.

### 4. Transcript, automatic blurb, chronological prompt

- `figure/blurb/auto.rs` (new): after an aside finishes, wait for its
  transcript to reach a terminal status (the `notes` poll pattern), write the
  one blurb, append. A silent or skipped transcript captions from the picture
  alone and says so in the status, naming the figure.
- `blurb/mod.rs`: `context_for` leads with the author's explanation, then the
  chapter window if any; `SYSTEM_PROMPT` reworded — the transcript is now
  *about* the picture. `spawn` (the batch) stays as the retry.
- `blog/generate.rs`: `build_user_prompt` emits figures **inline and in
  order** — `<Chapter 3> … <Figure 02 — taken at the end of chapter 3>
  Author's explanation: … Caption: … <Chapter 4>` — from `Longform`, and the
  "Figures available" list is derived from the same set so the two cannot
  disagree. `FigureOffer` gains `said`.

### 5. The UI move and docs

- `ui/mod.rs`: a "Figure  ⌃⇧S" button in the Record group (the
  `RECORD/NOTES/SESSION` ranges shift — the comment there warns about exactly
  this); `set_recording` gains a break state: *"On a break — figure 03
  captured, explaining 0:42 · ⌃⇧S to resume chapter 04"*. Figure status moves
  from `set_blog_status` to the record status line.
- `templates/blog.html`, `figure/pane.rs`: drop the Capture button, show `said`
  under each row, hint says "⌃⇧S while recording…", Write Blurbs reads as a
  retry.
- `hotkeys.rs` doc for `CaptureFigure`; `docs/desktop-workflow.md` step 1 gets
  a Figures paragraph.
- **Before this stage**: confirm the CMS schema has `content.image`. The payload
  emits it and the landing renders it, but the CMS lacked the component when
  last checked, and figures cannot publish until it exists.

## Out of scope

- Recording the screen during a break. The pair is a picture and a voice.
- Changing where `figures.jsonl` lives. It is project-scoped while chapter
  numbers are version-scoped; that predates this plan and is left alone.
- Lossy WebP. If a project ever snips photographs rather than screens,
  `cwebp` is installed and a quality knob is a small follow-on.
