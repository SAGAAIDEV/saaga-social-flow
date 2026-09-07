# Live face-tracking translate for the camera stream

## Context

The stream-recorder's ops graph (`src/ops/`) currently only ships analyzers —
`Passthrough` and `Stats` — that read frames but never change what reaches the
encoder. A sibling project, `face-track/`, already proves out face detection
and smoothing as an offline batch tool (decode camera.mp4 → detect with YuNet
ONNX → smooth → write crop coordinates). The next step is to bring that
capability live: detect the face in each captured camera frame and apply a
smoothed translate so the recorded chapter file itself keeps the face
centered, instead of computing coordinates for a later offline pass.

This is deliberately a **translate only** — full-frame shift, not a crop to a
smaller canvas. A future downstream stage (crop/composite) can consume the
same sidecar data or sit after this op in a later preset; that stage is out of
scope here.

The codebase's own `ops/sidecar.rs` doc comment already names this exact
feature ("the shape `screencast`'s edit stage already expects of
`face_track.json`, which is what stage 5 will emit through this exact path"),
and `ops/graph.rs`'s `StreamCtx` doc comment already anticipates "the contract
stage 2 builds a pixel buffer pool from." This plan **is** that stage 2/5.

macOS's native Vision framework (`VNDetectFaceRectanglesRequest`) is preferred
over porting the sibling project's ONNX/YuNet detector: it's Neural-Engine
accelerated, needs no bundled model or external runtime (`face-track`'s ONNX
path requires `brew install onnxruntime` as a real host dependency), and —
per direct crates.io verification below — its Rust binding resolves cleanly
against this project's already-pinned dependency versions.

## Phase 0 — verification spike (half a day, do this first)

Checked `objc2-vision`'s crates.io sparse index directly (not just the local
cache, which had never resolved it): **`objc2-vision` 0.3.2 exists**, and its
own dependency requirements (`objc2 >=0.6.2,<0.8.0`, `objc2-av-foundation
^0.3.2`, `objc2-core-video ^0.3.2`, `objc2-core-media ^0.3.2`,
`objc2-core-image ^0.3.2`, `objc2-core-ml ^0.3.2`, `objc2-foundation ^0.3.2`)
are **already satisfied exactly** by this repo's `Cargo.lock` (objc2 0.6.4,
every framework crate at 0.3.2). Its default features already include
`VNDetectFaceRectanglesRequest`, `VNRequestHandler`, `VNObservation`,
`VNFaceObservationAccepting` — no feature-list surgery needed.
`objc2-core-image` (needed separately, for `CIImage`/`CIContext`/
`CIAffineTransform` — a dependency's own deps aren't usable directly) is also
already 0.3.2-compatible.

Still confirm before building on it:

1. `cargo add objc2-vision objc2-core-image && cargo build`. Expect a clean
   resolve/build given the version match above. A hard failure here means
   stop and go straight to the ONNX fallback (below) — don't debug a wrong
   existence assumption.
2. Confirm `VNImageRequestHandler` (construct from a `CIImage` + orientation
   + options), `VNSequenceRequestHandler` (the stateful, single-thread-only
   variant Apple recommends for a live stream — if awkward/missing in 0.3.2,
   fall back to a fresh `VNImageRequestHandler` per sampled frame, same
   architecture otherwise), `VNDetectFaceRectanglesRequest`, and
   `VNFaceObservation.boundingBox()` are all reachable with a workable Rust
   shape (`cargo doc -p objc2-vision --no-deps` or read the vendored source).
3. Build a `CIImage` from a real captured `CVPixelBuffer`
   (`CIImage::new_with_cv_image_buffer`) and feed that to
   `VNImageRequestHandler`, rather than handing Vision the pixel buffer
   directly. This matters: `VNImageRequestHandler`'s `cvPixelBuffer:`
   initializer only accepts a specific pixel-format allowlist, and this
   codebase's camera path has historically negotiated `2vuy` (4:2:2 packed —
   see `ops/stats.rs`'s `luma_mean` doc comment), which isn't on it. Going
   through Core Image sidesteps the whole format question, and Core Image is
   needed anyway for the translate render (below), so one dependency covers
   both needs.

**Fallback if Phase 0 fails:** port `face-track/src/detect.rs`'s YuNet/ONNX
detector (`ort = "=2.0.0-rc.10"`, `load-dynamic`, vendor
`face-track/models/yunet.onnx`). The integration point downstream (detector
thread, shared cell, One-Euro smoothing, translate-application, sidecar) is
identical either way — only the detector internals swap. Feeding YuNet a
pre-letterboxed BGR24 640×640 buffer is also cheaper than it first looks:
build it via the same `CIImage` pipeline this plan already needs (scale +
pad + render to bytes), not a bespoke resize/color-convert path.

## Stage-2 adaptor infrastructure (prerequisite, either detector choice)

`Frame::replace()` (`ops/frame.rs`) currently just sets a field — nothing
downstream reads it back out. `Graph::run()` (`ops/graph.rs`) builds a local
`Frame` inside a `catch_unwind` closure and drops it; both delegates
(`capture/av_delegate.rs`, `capture/screen_delegate.rs`) unconditionally
`appendSampleBuffer` the **original** sample buffer regardless of what any op
did. Making a translate actually reach the encoded file requires:

**`Cargo.toml`** — add `objc2-vision = "0.3"` and `objc2-core-image = "0.3"`
(default features).

**`src/capture/av.rs`**
- `ChapterWriter` gains `pixel_buffer_adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>`.
- `create_chapter_writer` builds it via
  `AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes`
  **between the two `addInput` calls and `startWriting()`** — building it
  after `startWriting` throws an uncatchable `NSException`. Needs a new
  `camera_size: (usize, usize)` parameter to size the pool's attributes.
- **Pool pixel format: `kCVPixelFormatType_32BGRA`**, fixed regardless of the
  camera's native capture format. Core Image renders efficiently to BGRA,
  and fixing it decouples the pool's format from whatever the camera
  actually negotiates.
- New `Connection::camera_frame_size() -> Result<(usize, usize)>`. The
  camera's live size isn't known upfront (`StreamCtx::size` is `None` for
  camera precisely because `capture::av` never calls `setVideoSettings`).
  `av_delegate.rs`'s warmup path already unwraps `sample_buffer.image_buffer()`
  to count seen video buffers — cache that first buffer's
  `CVPixelBufferGetWidth`/`GetHeight` there (a small `Mutex<Option<(usize,
  usize)>>` on `AvDelegateIvars`, set-once).

**`src/router.rs`**
- `Router::build_chapter` currently opens the graph *before* building the
  writer. For camera, this must reverse: the writer (and its adaptor/pool)
  must exist before the graph opens, because `StreamCtx` is how the pool
  reaches `FaceTrack::open()`.
- **New failure-cleanup path this reordering introduces**: today, a graph-open
  failure returns before any writer/file exists — a clean abort. With the
  writer built first, a graph-open failure now happens *after*
  `startWriting()` has already created the file. The new error path must call
  `chapter_writer.writer.cancelWriting()` (documented elsewhere in this
  codebase to delete the output file — correct here, since a chapter that
  never truly started should leave nothing behind) before propagating.
- Build the adaptor unconditionally for every camera chapter regardless of
  which graph preset runs — decouples "the plumbing exists" from "an op uses
  it," and costs nothing extra since the append branch only touches it when
  `outcome.replaced` is true.
- `Router` owns a process-lifetime `CIContext` (per `ops/mod.rs`'s own
  guidance: "anything expensive to build... should be built once and handed
  in through `StreamCtx`"), built once in `start_at`, cloned into each
  chapter's `StreamCtx`.
- New `Router` field `face_track: bool` selects `graphs::face_track_graph`
  vs `graphs::default_graph` in `open_graph` for the camera stream.

**`src/ops/graph.rs`**
- `Graph::run` changes from returning bare `Flow` to a `RunOutcome { flow,
  pixels: CFRetained<CVImageBuffer>, replaced: bool, pts: CMTime }`, so a
  replaced buffer surfaces to the caller instead of being dropped with the
  local `Frame`.
- **Correctness requirement on the bypass/error/panic paths**: today, on an
  op error or panic, `run()` falls back to `Flow::Continue` and the delegate
  appends the original sample buffer — that fallback must be preserved
  exactly. `RunOutcome` on any bypass/error/panic path must report
  `replaced: false` and the **original, unmodified** input pixels — never a
  partially-applied transform from an op that failed partway through. A
  crashing `FaceTrack` must never leak a corrupted frame into the recording;
  it may only ever cost that chapter's translate (same "loud, not fatal"
  posture the rest of this module already has).
- Existing tests that call `graph.run(...)` and compare against `Flow`
  directly need updating to read `.flow` off the new return type.

**`src/ops/frame.rs`**
- `StreamCtx` gains `pixel_buffer_pool: Option<CFRetained<CVPixelBufferPool>>`
  and `ci_context: Option<Retained<CIContext>>` — both `Some` for camera,
  `None` for screen (mirrors the existing `size`/`fps` asymmetry already
  documented there).

**`src/capture/av_delegate.rs`**
- `AvState` gains `pixel_buffer_adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>`.
- `handle_sample_buffer`'s append branches on `outcome.replaced`:
  replaced → `pixel_buffer_adaptor.appendPixelBuffer_withPresentationTime(&outcome.pixels, outcome.pts)`;
  not replaced → today's unchanged `appendSampleBuffer(sample_buffer)`. For
  every existing preset (`default_graph`, `stats_graph`), `replaced` is
  always `false`, so this is byte-for-byte the current behavior — the
  invariant "ops that never call replace cost nothing" holds.
- This branch depends on a whole-chapter invariant already documented on
  `Frame::replace`: the append API must be chosen once per sink at chapter
  open, never alternated per frame (alternating produces a silent
  colour-shift, not an error). `FaceTrack::apply()` must therefore call
  `replace()` unconditionally on **every** frame of a chapter it's attached
  to, holding the last known translate steady on frames it didn't freshly
  detect — never replace conditionally.
- Screen path (`screen_delegate.rs`) is untouched — out of scope, no
  face-tracking need there.

## The `FaceTrack` op (`src/ops/face_track.rs`, new file)

**Detector thread**: `open()` spawns one dedicated thread (joined in
`close()`) owning a `VNSequenceRequestHandler` (or a per-frame
`VNImageRequestHandler` if Phase 0 finds the sequence handler unusable — same
architecture either way) plus two `OneEuro` filter instances (dx, dy) ported
near-verbatim from `face-track/src/smooth.rs` (defaults `min_cutoff=0.5,
beta=0.02` — the one component of that project directly reusable as-is,
since it's already causal/streaming).

**Handoff, capture queue → detector thread**: `std::sync::mpsc::sync_channel(1)`.
`apply()` calls `try_send`, discarding on `Full`/`Disconnected` — this is what
keeps `apply()` non-blocking; a genuine `send` would violate the capture-queue
contract the moment detection falls behind. Depth 1 (not 0, not unbounded):
0 would drop every frame arriving mid-detection; unbounded would let a
stalled detector accumulate an ever-growing backlog of retained buffers.
What crosses the channel is the retained `CVImageBuffer` itself via
`ops::tap::SharedPixels` (already exists, already `unsafe impl Send + Sync`
with the exact justification needed — reuse it rather than inventing a
parallel wrapper; its `#[allow(dead_code)]` comes off as this becomes its
first real caller), plus `CMTime` and a frame index. Sending the retained
buffer (not a pre-extracted byte copy) means the lock/`CIImage`
construction/Vision call all happen off the capture queue entirely — the
whole point of the dedicated thread, and consistent with why `Stats` is kept
out of `default_graph` (a full-plane read on the capture queue contends the
same mutex the audio callback needs).

**Detection cadence**: every 2nd frame (constant, not configurable in this
pass) — ~15 fps of detection against 30 fps capture. `apply()` still calls
`frame.replace()` on **every** frame (required by the whole-chapter
replace-invariant above), holding the last smoothed value steady between
detections; One-Euro's own smoothing already masks any step at the sampling
boundary.

**Shared cell**: `Arc<Mutex<Smoothed>>` where `Smoothed { dx: f32, dy: f32,
confidence: f32, face_found: bool }` — plain `Copy` data, no CoreVideo/ObjC
types, trivially `Send`. `apply()` reads via `try_lock()` only, falling back
to its own cached last-good value on `Err` — this is what makes "must not
block" airtight rather than merely usually-fast. The detector thread's write
side uses a plain `lock()` (harmless to block there briefly).

**Applying the translate**: every `apply()` call — build a `CIImage` from
`frame.pixels()`, apply `CGAffineTransformMakeTranslation(dx, dy)` via
`CIFilter`'s `CIAffineTransform`, request a fresh buffer from
`StreamCtx.pixel_buffer_pool` (`CVPixelBufferPoolCreatePixelBuffer` — request
new each frame, the pool recycles internally), render via
`StreamCtx.ci_context` into it, then `frame.replace(new_pixels)`. Core Image
over a hand-rolled plane-shift memcpy: the camera's format isn't guaranteed
(historically `2vuy`, not guaranteed to stay that way), and Core Image
handles format conversion + affine transform + pool-render generically — one
mechanism serves both the Vision `CIImage` bridge and this render, instead of
two format-aware code paths.

**Clamping**: `MAX_SHIFT_FRACTION: f32 = 0.12` — clamp `|dx| ≤ 0.12 ×
frame_width`, `|dy| ≤ 0.12 × frame_height`. Unclamped is unsafe: a translate
has no new source content at the trailing edge, so an aggressive shift (a
false-positive detection at a frame corner, a face walking off-frame) reveals
a black band on the opposite edge — visually worse than doing nothing. 12%
bounds the worst case to a modest margin loss.

**Sidecar**: accumulate `Vec<FrameRec { frame_index: u64, t_ms: f64, dx: f32,
dy: f32, detected: bool, confidence: f32 }>` in `apply()` (not on the
detector thread — `apply()` knows the true per-frame index/PTS and what was
actually applied). ~540KB for a 10-minute chapter at 30fps — trivial to hold
in memory, flush once at `close()`. `close()` calls
`sidecar.record("face_track", json!({ "frames": [...], "sampled_every": 2,
"max_shift_fraction": 0.12, "frames_seen": ..., "frames_detected": ...,
"clamped_count": ... }))` → lands as `chapter-NN.face_track.json`, the exact
name `ops/sidecar.rs`'s own doc comment already commits to. Note in the JSON
or file docs that `detected` reflects the most recent completed detection
(up to one sample-stride stale), since detection is sampled while
`face-track`'s offline tool runs every frame.

## Wiring: opt-in preset

**`src/ops/graphs.rs`** — new `face_track_graph(stream: StreamId) -> Graph`
(Source → Passthrough → FaceTrack), mirroring `stats_graph`'s existing
precedent exactly. `default_graph` is untouched.

**Selection plug point** — none exists today (`open_graph` hardcodes
`default_graph`, no config/CLI field for graph choice). Add:
- `src/cli.rs`: `#[arg(long)] face_track: bool` on `Command::Record` (the
  real session path; leave the standalone `Command::Av` smoke-test path
  alone).
- Thread through `src/main.rs` → `src/app.rs` → `Router::start_at`.
- Not persisted in `config.rs` — that file only persists device selection
  today; this is a per-session toggle like `--reselect-camera`, not a
  device preference. Can become sticky later with a one-line addition if
  wanted.

## Testing

**Unit** (no camera/permission prompt, using
`ops::frame::test_support::pixel_buffer`):
- `OneEuro` filter damping behavior, defaults match `0.5`/`0.02`.
- Clamping as a pure function against known boundary inputs at 0.12.
- `FaceTrack::apply()` against a synthetic BGRA buffer + a hand-built pool/
  `CIContext`: assert `was_replaced()` true on every call, one `FrameRec` per
  call, a detection injected directly into the shared cell produces the
  expected clamped shift.
- `graphs::face_track_graph` mirrors `the_stats_graph_analyzes_after_passing_through`:
  `op_names() == ["passthrough", "face_track"]`.
- `Graph::run`'s new `RunOutcome`: extend existing `graph.rs` tests with one
  asserting a replace-calling test op surfaces `replaced: true` and the
  swapped buffer, and one asserting the bypass/panic path surfaces
  `replaced: false` with the **original** pixels.

**Hardware integration**, extending the existing `#[ignore]`-gated pattern in
`router.rs` (`chapter_flow_produces_encoded_streams`): new test recording
briefly with `face_track: true` against a real camera with a face in frame.
Assert `chapter-01.face_track.json` exists/parses with a plausible frame
count, and — the check that actually proves the translate reached the
encoded file, not just the sidecar — decode a frame via `ffmpeg` and confirm
it isn't pixel-identical to an untracked recording of the same static scene.

## Risks / open decisions (flagged, not resolved here)

- **Camera orientation for Vision's `orientation:` param.** No orientation
  logic exists anywhere in this codebase today. Hardcoding `.up` is correct
  for the target hardware (desk-mounted USB webcams) but is an assumption
  worth documenting explicitly at the call site.
- **`AVAssetWriterInputPixelBufferAdaptor.pixelBufferPool` availability
  timing** — unverified whether it's populated immediately after
  `startWriting()` or only after `startSessionAtSourceTime()` (which happens
  later than `build_chapter` returns). Worth an early empirical log-check
  before assuming eager availability.
- **`VNSequenceRequestHandler` availability in objc2-vision 0.3.2**
  specifically is unverified pending Phase 0; the fallback (per-frame
  `VNImageRequestHandler`) works either way but has an uncharacterized perf
  cost worth profiling if needed.
- Translate never changes frame dimensions in this design (pool fixed at
  camera's native size for the chapter) — a true crop-to-smaller-canvas is a
  separate, later feature.

## File manifest

| Path | New/Modified | Purpose |
|---|---|---|
| `Cargo.toml` | Modified | Add `objc2-vision`, `objc2-core-image` |
| `src/ops/face_track.rs` | New | The `FaceTrack` op |
| `src/ops/graph.rs` | Modified | `Graph::run` → `RunOutcome`; bypass path preserves original pixels |
| `src/ops/frame.rs` | Modified | `StreamCtx` gains `pixel_buffer_pool`, `ci_context` |
| `src/ops/graphs.rs` | Modified | New `face_track_graph` preset |
| `src/capture/av.rs` | Modified | `ChapterWriter` gains the pixel buffer adaptor; `camera_frame_size()` |
| `src/capture/av_delegate.rs` | Modified | `AvState` carries the adaptor; append branches on `replaced` |
| `src/router.rs` | Modified | Writer-before-graph ordering (camera); `face_track: bool`; owns process-lifetime `CIContext` |
| `src/cli.rs`, `src/app.rs`, `src/main.rs` | Modified | `--face-track` flag threaded to `Router::start_at` |
