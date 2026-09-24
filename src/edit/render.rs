//! Render prepared HyperFrames compositions and assemble the long form.
//!
//! Each job is an independent headless-browser render writing to its own file, so
//! they run on a small pool rather than one at a time. The pool is deliberately
//! *small*: a render is memory- and CPU-hungry, and oversubscribing turns a fast
//! machine into a swapping one. The pool is sized by the memory the machine can
//! spare when the render starts — see [`plan_pool`] for the day that stopped
//! being a figure of speech — and only capped by the cores. Concat order comes
//! from the plan, never from the order jobs happen to finish.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};

use super::compose::{Job, Kind, Plan, Segment};
use super::{cut, gpu};

/// HyperFrames' encoder quality for every deliverable this renders: the title
/// cards spliced into the horizontal longform, and the vertical chapters that
/// are the shorts and the vertical longform. Of `draft`, `standard` and `high`
/// this ran at `draft`, which is the preview setting — the shorts came out
/// Constrained Baseline at under 4 Mbps and read as compressed. `high` costs
/// render time and buys the picture; the render is already incremental, so it
/// is paid once per composition.
pub const QUALITY: &str = "high";
/// The quantiser HyperFrames encodes at, in place of the 15 that `high` picks.
/// The cut's own figure — see `cut::DELIVERABLE_CRF` — because a vertical
/// chapter is the cut composited and encoded again here, and a second
/// generation looser than the first undoes the first.
pub const CRF: &str = "12";
/// How HyperFrames pulls frames out of the footage it composites: JPEG, at the
/// extractor's quality 95 (ffmpeg `-q:v 2`).
///
/// This was PNG, to keep the cut out of a third lossy codec on its way into
/// Chrome. That cost more than it bought: a PNG frame of the portrait cut is
/// about 1.1 MB, so a 158 s chapter extracted to 5 GB. Locally those frames are
/// injected into Chrome as data URIs, part of what put 8 to 9 GB behind each
/// render, and a GPU machine writes every frame of a chapter to its disk.
/// JPEG frames are roughly a tenth the size. Shared by both paths, so a chapter
/// looks the same wherever it was drawn.
pub const VIDEO_FRAME_FORMAT: &str = "jpg";

/// Everything about the encode a rendered file depends on, as one line.
///
/// Written to the workspace as [`super::compose::QUALITY_FILE`] and declared a
/// source of every job, so a change to any of these re-renders everything
/// exactly once.
pub fn encoder_stamp() -> String {
    format!("{QUALITY} crf={CRF} frames={VIDEO_FRAME_FORMAT} hyperframes={HF_VERSION}")
}
/// The renderer version, pinned exactly in `renderer/package.json` (a test holds
/// the two together). Part of [`encoder_stamp`], so moving it re-renders every
/// composition once rather than leaving a project half on each version.
pub const HF_VERSION: &str = "0.8.62";
/// The assembled cut, in whichever orientation's directory it lands.
pub const LONGFORM: &str = "longform.mp4";
/// Beside each longform, the list of parts it was joined from.
///
/// One more input for the join's freshness check. The parts' own mtimes are not
/// enough: a layout change that touches no part — the opening title card coming
/// out — left the previous longform newer than everything it was made of, and
/// so "already current" with the card still in it.
const PARTS_FILE: &str = "longform.parts.txt";

/// One render: which workspace it runs in, what it renders, where it lands.
struct Task<'a> {
    workspace: &'a Path,
    job: &'a Job,
    dest: PathBuf,
}

/// Renders what the plan needs and assembles both longforms.
///
/// `progress` is told `(done, total)` over every composition in the plan,
/// current ones included — so a re-cut of one chapter reports most of the set
/// done before a single render starts, which is the truth of it. It is called
/// from the worker threads, hence `Sync`.
pub fn render_plan(
    plan: &Plan,
    publish: &Path,
    status: &(dyn Fn(&str) + Sync),
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<PathBuf> {
    std::fs::create_dir_all(publish).with_context(|| format!("creating {}", publish.display()))?;
    let h_dir = publish.join("horizontal");
    let v_dir = publish.join("vertical");
    std::fs::create_dir_all(&h_dir).with_context(|| format!("creating {}", h_dir.display()))?;
    std::fs::create_dir_all(&v_dir).with_context(|| format!("creating {}", v_dir.display()))?;

    // Destinations are derived from the plan up front, so the concat below reads
    // them in chapter order no matter which worker finished first. A passthrough
    // segment is already where it needs to be.
    let h_out: Vec<PathBuf> = plan
        .h_segments
        .iter()
        .map(|segment| match segment {
            Segment::Render(job) => h_dir.join(format!("{}.mp4", job.id)),
            Segment::Passthrough(path) => path.clone(),
        })
        .collect();
    let v_out: Vec<PathBuf> = plan
        .v_jobs
        .iter()
        .map(|job| v_dir.join(format!("{}.mp4", job.id)))
        .collect();
    let tasks: Vec<Task> = plan
        .h_segments
        .iter()
        .zip(h_out.iter().cloned())
        .filter_map(|(segment, dest)| {
            segment.job().map(|job| Task {
                workspace: &plan.horizontal,
                job,
                dest,
            })
        })
        .chain(
            plan.v_jobs
                .iter()
                .zip(v_out.iter().cloned())
                .map(|(job, dest)| Task {
                    workspace: &plan.vertical,
                    job,
                    dest,
                }),
        )
        .collect();

    let (skipped, pending): (Vec<Task>, Vec<Task>) = tasks
        .into_iter()
        .partition(|task| is_fresh(&task.dest, &task.job.sources));
    if !skipped.is_empty() {
        eprintln!(
            "stream-recorder: {} render(s) already current, skipping: {}",
            skipped.len(),
            skipped
                .iter()
                .map(|task| task.job.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let all = skipped.len() + pending.len();
    progress(skipped.len(), all);
    // Checked once, before anything is dispatched: an expired `aws sso login`
    // is one message, not one failure per chapter. No fallback to this Mac —
    // a surprise local render is exactly what the box is there to prevent.
    let cloud = plan.targets.cloud;
    let on_done = |finished| progress(skipped.len() + finished, all);
    match cloud {
        true if !pending.is_empty() => {
            status("Checking the AWS sign-in for GPU rendering…");
            gpu::check_ready()?;
            let board = Board::new(pending.len(), "on AWS GPU");
            let tasks: Vec<gpu::Task> = pending
                .iter()
                .map(|task| gpu::Task {
                    workspace: task.workspace,
                    job: task.job,
                    dest: &task.dest,
                })
                .collect();
            gpu::render_all(&tasks, &board, status, &on_done)?;
        }
        _ => render_all(&pending, status, &on_done)?,
    }

    // The one step after the renders with a wait worth naming: the cards are
    // conformed to the footage and everything is joined. Each longform only
    // when it was asked for, and only when a part is newer than the last join —
    // so ticking the vertical longform after a render that already drew the
    // shorts costs the join and nothing else.
    if !plan.h_segments.is_empty() {
        let longform = h_dir.join(LONGFORM);
        if is_fresh(&longform, &join_inputs(&h_dir, &h_out)?) {
            eprintln!("stream-recorder: the horizontal longform is already current");
        } else {
            status("Assembling the longform…");
            let joinable = conform_for_concat(plan, &h_out, &h_dir)?;
            join(&joinable, &longform)?;
        }
    }
    // No `conform_for_concat` for the verticals, and that is not an oversight:
    // every part of the horizontal longform is a different animal — rendered
    // title cards spliced between passthrough camera footage — while the
    // verticals are all the same composition out of the same renderer, so they
    // already agree on profile, pixel format and frame rate.
    if plan.targets.vertical && !v_out.is_empty() {
        let longform = v_dir.join(LONGFORM);
        if is_fresh(&longform, &join_inputs(&v_dir, &v_out)?) {
            eprintln!("stream-recorder: the vertical longform is already current");
        } else {
            status("Assembling the vertical longform…");
            join(&v_out, &longform)?;
        }
    }
    Ok(publish.to_path_buf())
}

/// Records `parts` beside the longform in `dir`, touching the file only when the
/// list changes, and returns everything the join is fresh against — see
/// [`PARTS_FILE`].
fn join_inputs(dir: &Path, parts: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let list = dir.join(PARTS_FILE);
    let body = parts
        .iter()
        .map(|part| part.display().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    super::compose::write_if_changed(&list, &body)?;
    let mut inputs = parts.to_vec();
    inputs.push(list);
    Ok(inputs)
}

/// Joins the parts into one file, or copies the only part there is.
///
/// Nothing to join is not a failure: a project with no vertical cuts should
/// render its horizontal longform and say nothing about the verticals it never
/// had.
fn join(parts: &[PathBuf], dest: &Path) -> Result<()> {
    match parts {
        [] => Ok(()),
        [only] => std::fs::copy(only, dest)
            .map(|_| ())
            .with_context(|| format!("copying {} to {}", only.display(), dest.display())),
        many => cut::concat_videos(many, dest),
    }
}

/// The longform's parts, all encoded the same way so the concat can copy streams.
///
/// The cut chapters set the standard — they are the footage, and re-encoding them to
/// match a title card would be the very waste this avoids. So the cards are conformed to
/// *them*: same profile, pixel format, frame rate and timescale. The concat demuxer does
/// not decode, so a card still carrying HyperFrames' Constrained Baseline at 30fps
/// between two High-profile 25fps chapters is a join that stutters or refuses to mux.
fn conform_for_concat(plan: &Plan, h_out: &[PathBuf], h_dir: &Path) -> Result<Vec<PathBuf>> {
    // The frame rate every part is brought to: the first real chapter's.
    let reference = plan.h_segments.iter().find_map(|segment| match segment {
        Segment::Passthrough(path) => Some(path.clone()),
        Segment::Render(_) => None,
    });
    let Some(reference) = reference else {
        // Cards only, which is not a longform anyone asked for — but it is still
        // internally consistent, so leave it alone.
        return Ok(h_out.to_vec());
    };
    let fps = cut::probe_frame_rate(&reference)
        .with_context(|| format!("reading the frame rate of {}", reference.display()))?;

    let mut parts = Vec::with_capacity(h_out.len());
    for (segment, rendered) in plan.h_segments.iter().zip(h_out) {
        match segment {
            Segment::Passthrough(path) => parts.push(path.clone()),
            Segment::Render(job) => {
                let joined = h_dir.join(format!("{}.joinable.mp4", job.id));
                if is_fresh(&joined, std::slice::from_ref(rendered)) {
                    parts.push(joined);
                    continue;
                }
                cut::conform(rendered, &joined, fps)
                    .with_context(|| format!("conforming {} for the longform", job.id))?;
                parts.push(joined);
            }
        }
    }
    Ok(parts)
}

/// Runs every task on a bounded pool, reporting *all* failures rather than the
/// first. One broken chapter should not hide the state of the other nineteen.
///
/// `on_done` hears how many have finished, success or failure, as each one does.
/// `status` carries the same count as words, because a bar alone over a
/// twenty-minute render reads as a hang. Render on AWS does not come through
/// here — see [`gpu::render_all`].
fn render_all(
    tasks: &[Task],
    status: &(dyn Fn(&str) + Sync),
    on_done: &(dyn Fn(usize) + Sync),
) -> Result<()> {
    if tasks.is_empty() {
        return Ok(());
    }
    let pool = choose_pool()?;
    let renders = pool.renders.min(tasks.len());
    let total = tasks.len();
    eprintln!(
        "stream-recorder: rendering {total} composition(s) on {renders} worker(s) with {} \
         Chrome capture worker(s) each",
        pool.workers
    );

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let board = Board::new(total, "on this Mac");
    status(&board.line());
    let alive = AtomicUsize::new(renders);
    let failures: Mutex<Vec<(String, anyhow::Error)>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..renders {
            scope.spawn(|| loop {
                // Chrome grows over a render and the other applications do not
                // stand still, so the room measured at the start is not the room
                // there is now. A worker that would start its next render into
                // less than one render's worth of spare memory retires instead,
                // shrinking the pool — unless it is the last one, because a render
                // that never finishes helps no one.
                if let Some(available) = available_memory() {
                    if available < HEADROOM + render_cost(pool.workers)
                        && retire_unless_last(&alive)
                    {
                        eprintln!(
                            "stream-recorder: {} of memory available, below the {} a render \
                             needs; one worker is retiring and {} carry on",
                            gib(available),
                            gib(HEADROOM + render_cost(pool.workers)),
                            alive.load(Ordering::Relaxed)
                        );
                        break;
                    }
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(task) = tasks.get(index) else {
                    alive.fetch_sub(1, Ordering::Relaxed);
                    break;
                };
                board.start(&task.job.id);
                status(&board.line());
                let outcome = render_job(task.workspace, task.job, &task.dest, pool.workers);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                board.finish(&task.job.id, outcome.is_ok());
                status(&board.line());
                on_done(finished);
                match outcome {
                    Ok(()) => eprintln!(
                        "stream-recorder: [{finished}/{total}] rendered {}",
                        task.job.id
                    ),
                    Err(err) => {
                        eprintln!(
                            "stream-recorder: [{finished}/{total}] FAILED {}: {err:#}",
                            task.job.id
                        );
                        // A poisoned lock means another worker panicked; keep the
                        // failure list rather than panicking this thread too.
                        failures
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push((task.job.id.clone(), err));
                    }
                }
            });
        }
    });

    let mut failures = failures
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failures.is_empty() {
        return Ok(());
    }
    // Workers finish out of order; sort so the same run always reports the same way.
    failures.sort_by(|a, b| a.0.cmp(&b.0));
    let mut message = format!("{} of {total} render(s) failed:", failures.len());
    for (id, err) in &failures {
        message.push_str(&format!("\n  - {id}: {err:#}"));
    }
    bail!(message)
}

/// What the status line says while the pool runs: how many of how many are
/// done, and each job in flight — with its percentage when the job reports one,
/// which the GPU machines do from the render's own progress line.
pub(super) struct Board {
    total: usize,
    /// "on this Mac", "on AWS GPU" — where the line says the renders are.
    place: &'static str,
    state: Mutex<BoardState>,
}

#[derive(Default)]
struct BoardState {
    done: usize,
    failed: usize,
    /// In start order, so the line does not shuffle as jobs report.
    in_flight: Vec<(String, Option<u8>)>,
    /// What the whole render is doing besides its jobs — the cloud path's
    /// "3 GPU machine(s): 2 rendering, 1 installing".
    note: Option<String>,
}

impl Board {
    pub(super) fn new(total: usize, place: &'static str) -> Self {
        Board {
            total,
            place,
            state: Mutex::new(BoardState::default()),
        }
    }

    pub(super) fn note(&self, note: &str) {
        self.lock().note = Some(note.to_string());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BoardState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn start(&self, id: &str) {
        self.lock().in_flight.push((id.to_string(), None));
    }

    /// Records a job's progress; true when the whole percent changed, so the
    /// status line is only repainted when it would read differently.
    pub(super) fn advance(&self, id: &str, fraction: f64) -> bool {
        let percent = (fraction.clamp(0.0, 1.0) * 100.0).floor() as u8;
        let mut state = self.lock();
        match state.in_flight.iter_mut().find(|(job, _)| job == id) {
            Some((_, seen)) if *seen != Some(percent) => {
                *seen = Some(percent);
                true
            }
            _ => false,
        }
    }

    pub(super) fn finish(&self, id: &str, ok: bool) {
        let mut state = self.lock();
        state.in_flight.retain(|(job, _)| job != id);
        state.done += 1;
        if !ok {
            state.failed += 1;
        }
    }

    /// "Rendering on AWS GPU: 2/6 done, 4 left — chapter-02 41%, chapter-03 12%"
    pub(super) fn line(&self) -> String {
        let state = self.lock();
        let mut line = format!(
            "Rendering {}: {}/{} done, {} left",
            self.place,
            state.done,
            self.total,
            self.total - state.done
        );
        if state.failed > 0 {
            line.push_str(&format!(" ({} failed)", state.failed));
        }
        if let Some(note) = &state.note {
            line.push_str(&format!(" · {note}"));
        }
        if !state.in_flight.is_empty() {
            let jobs: Vec<String> = state
                .in_flight
                .iter()
                .map(|(id, percent)| match percent {
                    Some(p) => format!("{id} {p}%"),
                    None => id.clone(),
                })
                .collect();
            line.push_str(" — ");
            line.push_str(&jobs.join(", "));
        }
        line
    }
}

/// Memory kept free for everything that is not the render — the app, the
/// system, and whatever the operator is doing in the meantime, which is often a
/// live call. Bytes.
const HEADROOM: u64 = 4 << 30;
/// What one render costs before its first Chrome capture worker: the hyperframes
/// node process, its ffmpeg, and Chrome's browser and GPU processes. Bytes,
/// measured at the peak the out-of-memory snapshot of 2026-09-15 recorded.
const RENDER_BASE: u64 = 2560 << 20;
/// What each Chrome capture worker adds: one renderer process, which ran at 2.0
/// to 3.2 GB compositing PNG footage frames at portrait resolution — not the
/// quarter gigabyte hyperframes' own `--workers` help text promises. Bytes.
const WORKER_COST: u64 = 3200 << 20;

/// How many renders run at once, and how many Chrome capture workers each gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pool {
    renders: usize,
    workers: usize,
}

/// What a render of `workers` capture workers costs at its peak, in bytes.
fn render_cost(workers: usize) -> u64 {
    RENDER_BASE + WORKER_COST * workers as u64
}

/// The pool for this machine, right now.
///
/// `SCREENCAST_RENDER_WORKERS` pins the number of renders and skips the memory
/// check — for the operator who has looked at Activity Monitor and knows better —
/// each with one capture worker. Otherwise the memory decides; when it cannot
/// be read, the cores do, as they did before, and a line says so.
fn choose_pool() -> Result<Pool> {
    if let Ok(raw) = std::env::var("SCREENCAST_RENDER_WORKERS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            return Ok(Pool {
                renders: n.clamp(1, 8),
                workers: 1,
            });
        }
    }
    match available_memory() {
        Some(available) => plan_pool(available, cores()),
        None => {
            eprintln!(
                "stream-recorder: could not read free memory from vm_stat; sizing the render \
                 pool by cores alone"
            );
            Ok(Pool {
                renders: pool_size(cores()),
                workers: 1,
            })
        }
    }
}

/// Sizes the pool against the memory the machine can spare, under the ceiling
/// the cores set.
///
/// The cores used to set the whole pool, and the pool they produced — four
/// renders of two workers each — was right for the CPU and wrong for everything
/// else. A render is 8 to 9 GB, ten times what hyperframes' help text says, and
/// four of them landed on a 64 GB machine already carrying a browser, a VM and a
/// full swap file. macOS wrote three out-of-memory reports and the machine froze
/// for a quarter of an hour. Memory is the constraint that binds, so it decides
/// the count and the cores only cap it.
///
/// Every render gets one capture worker unless there is memory for a second
/// across the whole pool. Capture is memory-bound and the encoder is the CPU
/// sink, so the second worker is the first thing to give up.
///
/// An error rather than a pool of one when even one render does not fit: a
/// render that pushes the machine into swap finishes hours late, if at all, and
/// takes everything else down with it. The message says what to close.
fn plan_pool(available: u64, cores: usize) -> Result<Pool> {
    let spend = available.saturating_sub(HEADROOM);
    let fits = usize::try_from(spend / render_cost(1)).unwrap_or(usize::MAX);
    if fits == 0 {
        bail!(
            "not enough free memory to render: {} available, and one render needs about {} \
             with {} kept for the rest of the machine — close some applications (browser \
             tabs, Docker, stale terminals) and press Re-render missing",
            gib(available),
            gib(render_cost(1)),
            gib(HEADROOM)
        );
    }
    let renders = fits.min(pool_size(cores));
    let mut workers = 1;
    while workers < workers_per_render(cores) && spend / render_cost(workers + 1) >= renders as u64
    {
        workers += 1;
    }
    Ok(Pool { renders, workers })
}

/// Memory the machine could hand to new processes without swapping, in bytes:
/// free pages, purgeable pages, and the file cache, which is what macOS drops
/// first under pressure. Inactive pages are deliberately not counted — most are
/// anonymous memory that would have to be compressed or swapped out to reclaim,
/// and counting them read the frozen machine as having 14 GB to spare when the
/// same snapshot's free pages and file cache came to 3.
///
/// `None` when `vm_stat` is missing or says something unexpected.
fn available_memory() -> Option<u64> {
    let out = Command::new("vm_stat").output().ok()?;
    parse_vm_stat(&String::from_utf8_lossy(&out.stdout))
}

/// See [`available_memory`]; this is the part a test can feed.
fn parse_vm_stat(text: &str) -> Option<u64> {
    let page_size: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> Option<u64> {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim() != name {
                return None;
            }
            value.trim().trim_end_matches('.').parse().ok()
        })
    };
    let available = pages("Pages free")?
        + pages("Pages purgeable").unwrap_or(0)
        + pages("File-backed pages").unwrap_or(0);
    Some(available * page_size)
}

/// Takes one worker out of `alive` and says so — unless it is the last, which
/// stays whatever the memory looks like. Two workers deciding at the same
/// moment cannot both go: the update is atomic, so one of them sees a count of
/// one and keeps working.
fn retire_unless_last(alive: &AtomicUsize) -> bool {
    alive
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            if n > 1 {
                Some(n - 1)
            } else {
                None
            }
        })
        .is_ok()
}

/// Bytes as a figure a status line can carry: "5.6 GB".
fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
}

/// Chrome processes to allow across the whole render.
///
/// Two cores held back for the machine itself, so a render leaves it usable.
fn capture_budget(cores: usize) -> usize {
    cores.saturating_sub(2).clamp(2, 8)
}

/// How many `hyperframes` processes run at once.
///
/// Half the budget, so each one gets at least two capture workers of its own — the point
/// being that the product of this and [`workers_per_render`] is what actually lands on
/// the CPU, not this number alone.
fn pool_size(cores: usize) -> usize {
    (capture_budget(cores) / 2).clamp(1, 4)
}

/// What to pass as `--workers`.
///
/// This is the fix for a real mistake. Each `hyperframes` render spawns its own pool of
/// Chrome processes — `workerCount: 3` in its own progress log — so a pool of four
/// renders was quietly running twelve capture browsers on ten cores. They then fought
/// each other for CPU and the whole render ran slower than a smaller pool would have,
/// while the comment here claimed it stayed "well under the core count".
///
/// Dividing the budget rather than guessing means the two numbers cannot disagree.
fn workers_per_render(cores: usize) -> usize {
    (capture_budget(cores) / pool_size(cores)).max(1)
}

/// True when `dest` is a non-empty file newer than every declared input.
///
/// A job with no declared sources is never fresh: silence must not be read as
/// "nothing changed", or an edit would ship the previous render.
///
/// Shared with the cut and the waveform cache. The rule is the same wherever derived
/// output sits beside its inputs, and having one answer to "is this still current" is
/// what lets a one-chapter hand edit cost one chapter's work.
pub(crate) fn is_fresh(dest: &Path, sources: &[PathBuf]) -> bool {
    if sources.is_empty() {
        return false;
    }
    let Ok(meta) = dest.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    let Ok(rendered_at) = meta.modified() else {
        return false;
    };
    sources.iter().all(|source| {
        // An input we cannot stat is an input we cannot vouch for.
        matches!(
            source.metadata().and_then(|meta| meta.modified()),
            Ok(changed_at) if changed_at <= rendered_at
        )
    })
}

fn render_job(workspace: &Path, job: &Job, dest: &Path, workers: usize) -> Result<()> {
    dest.parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .with_context(|| format!("creating {}", dest.display()))?;
    let tmp = dest.with_file_name(format!(".{}.rendering.mp4", job.id));
    let _ = std::fs::remove_file(&tmp);
    let mut cmd = hyperframes_command()?;
    cmd.current_dir(workspace).args([
        "render",
        "--composition",
        &job.composition,
        "--quality",
        QUALITY,
        "--crf",
        CRF,
        "--video-frame-format",
        VIDEO_FRAME_FORMAT,
        // Bounded rather than left on `auto`, which sizes itself against the whole
        // machine and cannot know how many other renders this pool is running.
        "--workers",
        &workers.to_string(),
        "--resolution",
        match job.kind {
            Kind::Vertical => "portrait",
            Kind::Card | Kind::Body => "landscape",
        },
        "--output",
    ]);
    cmd.arg(&tmp);
    eprintln!("stream-recorder: {}", command_line(&cmd));

    // hyperframes narrates a render to its terminal — the Chrome pool, the frame
    // count, and the reason when it gives up. Until this was captured that went
    // to whichever terminal launched the app, and the app itself could only
    // report an exit status: five of twenty-one renders failed and the status
    // line could not say why. So each job writes beside its output, the tail of
    // that log rides on the error, and the file stays only when it is needed.
    let log_path = dest.with_extension("log");
    let log =
        File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .with_context(|| format!("sharing {}", log_path.display()))?;
    cmd.stdout(Stdio::from(log)).stderr(Stdio::from(log_err));
    let status = cmd
        .status()
        .with_context(|| format!("starting hyperframes render for {}", job.id))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "hyperframes render failed for {} ({status}); see {}{}",
            job.composition,
            log_path.display(),
            log_tail(&log_path, 3)
        );
    }
    publish_render(job, &tmp, dest, &log_path)
}

/// Moves a finished render from `tmp` to `dest`, refusing an empty one, and
/// drops the log that is only kept for failures.
fn publish_render(job: &Job, tmp: &Path, dest: &Path, log_path: &Path) -> Result<()> {
    if !tmp.is_file() || tmp.metadata()?.len() == 0 {
        let _ = std::fs::remove_file(tmp);
        bail!(
            "hyperframes produced no output for {}; see {}{}",
            job.id,
            log_path.display(),
            log_tail(log_path, 3)
        );
    }
    std::fs::rename(tmp, dest).with_context(|| format!("publishing {}", dest.display()))?;
    let _ = std::fs::remove_file(log_path);
    Ok(())
}

/// The last `n` lines of a render log with anything on them, ready to hang off
/// an error: one indented line each, or nothing when the log is empty or gone.
pub(super) fn log_tail(log: &Path, n: usize) -> String {
    let Ok(text) = std::fs::read_to_string(log) else {
        return String::new();
    };
    tail_lines(&text, n)
        .into_iter()
        .map(|line| format!("\n      {line}"))
        .collect()
}

/// The last `n` non-blank lines of `text`, in order, each trimmed.
///
/// Progress output rewrites its line with carriage returns rather than newlines,
/// so a `\r` counts as a line break too — otherwise the tail of a hyperframes
/// log is one enormous line of frame counters with the error somewhere inside.
fn tail_lines(text: &str, n: usize) -> Vec<&str> {
    let mut lines: Vec<&str> = text
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(n)
        .collect();
    lines.reverse();
    lines
}

/// The repo's `renderer/` package: the pinned HyperFrames CLI in its
/// `node_modules`.
///
/// `CARGO_MANIFEST_DIR` for a checkout, the executable's own directory for an
/// installed release — the same two places [`super::compose::components_root`]
/// looks, for the same reason.
pub fn renderer_root() -> PathBuf {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("renderer");
    if checkout.join("package.json").is_file() {
        return checkout;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("renderer")))
        .filter(|dir| dir.join("package.json").is_file())
        .unwrap_or(checkout)
}

/// The installed CLI, which `scripts/setup.sh` puts there with `npm ci`.
pub fn hyperframes_bin() -> PathBuf {
    renderer_root().join("node_modules/.bin/hyperframes")
}

/// No `npx` fallback: the renderer is a dependency of this repo, pinned by its
/// lockfile, and a render that quietly fetched some other copy would be a
/// render nobody can reproduce.
fn hyperframes_command() -> Result<Command> {
    let bin = hyperframes_bin();
    if !bin.is_file() {
        bail!(
            "the HyperFrames renderer is not installed at {} — run scripts/setup.sh",
            bin.display()
        );
    }
    Ok(Command::new(bin))
}

pub(super) fn command_line(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&arg.to_string_lossy());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The status line a long cloud render shows: the count, and each job in
    /// flight with its percentage once it has one.
    #[test]
    fn board_line_counts_and_shows_jobs_in_flight() {
        let board = Board::new(6, "on AWS GPU");
        assert_eq!(board.line(), "Rendering on AWS GPU: 0/6 done, 6 left");
        board.start("chapter-02");
        board.start("chapter-03");
        assert!(board.advance("chapter-02", 0.415));
        assert!(
            !board.advance("chapter-02", 0.419),
            "same whole percent, no repaint"
        );
        assert_eq!(
            board.line(),
            "Rendering on AWS GPU: 0/6 done, 6 left — chapter-02 41%, chapter-03"
        );
        board.finish("chapter-02", true);
        board.finish("chapter-03", false);
        assert_eq!(
            board.line(),
            "Rendering on AWS GPU: 2/6 done, 4 left (1 failed)"
        );
        assert_eq!(
            Board::new(1, "on this Mac").line(),
            "Rendering on this Mac: 0/1 done, 1 left"
        );
    }

    /// One version, stated twice: here for the encoder stamp and the doctor,
    /// in `renderer/package.json` for npm. A bump that moves one and not the
    /// other would render on a version the stamp does not name.
    #[test]
    fn hf_version_matches_the_renderer_package() {
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("renderer/package.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["dependencies"]["hyperframes"].as_str(),
            Some(HF_VERSION),
            "hyperframes in renderer/package.json"
        );
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-render-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The bug this replaced: the pool counted `hyperframes` *processes*, each of which
    /// then spawned three Chromes of its own, so four renders put twelve capture
    /// browsers on ten cores and they fought each other. What has to hold is the
    /// product, not either number alone.
    #[test]
    fn the_pool_and_its_workers_multiply_to_within_the_budget() {
        for cores in [1usize, 2, 4, 8, 10, 16, 64] {
            let total = pool_size(cores) * workers_per_render(cores);
            assert!(
                total <= capture_budget(cores),
                "{cores} cores: {} × {} = {total} exceeds a budget of {}",
                pool_size(cores),
                workers_per_render(cores),
                capture_budget(cores)
            );
            assert!(pool_size(cores) >= 1, "{cores} cores renders nothing");
            assert!(
                workers_per_render(cores) >= 1,
                "{cores} cores has no workers"
            );
        }
    }

    /// Two cores held back, so a render leaves the machine usable — and never more
    /// browsers than there is memory for, however many cores turn up.
    #[test]
    fn the_budget_leaves_the_machine_headroom() {
        assert_eq!(capture_budget(10), 8, "ten cores, two spare");
        assert_eq!(capture_budget(8), 6);
        assert_eq!(capture_budget(64), 8, "capped rather than unbounded");
        assert_eq!(capture_budget(1), 2, "never fewer than two");
        // The real machine this runs on: four renders of two workers each.
        assert_eq!(pool_size(10), 4);
        assert_eq!(workers_per_render(10), 2);
    }

    /// The two `vm_stat` readings this was calibrated on: the machine as it froze
    /// on 2026-09-15 — 3 GB to spare by this measure, 14 GB had inactive pages
    /// been counted — and the same machine the next evening, with the page size
    /// read from the header rather than assumed.
    #[test]
    fn free_memory_is_free_pages_plus_purgeable_plus_the_file_cache() {
        let frozen = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                      Pages free:                               10595.\n\
                      Pages active:                            713441.\n\
                      Pages inactive:                          704226.\n\
                      Pages speculative:                         7998.\n\
                      Pages purgeable:                              0.\n\
                      \"Translation faults\":                 58183286204.\n\
                      File-backed pages:                       205311.\n\
                      Anonymous pages:                        1220354.\n";
        let available = parse_vm_stat(frozen).unwrap();
        assert_eq!(available, (10595 + 205311) * 16384);
        assert_eq!(gib(available), "3.3 GB");

        let evening = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\n\
                       Pages free:                             1367468.\n\
                       Pages purgeable:                          18712.\n\
                       File-backed pages:                       830903.\n";
        assert_eq!(
            parse_vm_stat(evening).unwrap(),
            (1367468 + 18712 + 830903) * 4096,
            "the page size comes from the header"
        );

        assert_eq!(parse_vm_stat(""), None);
        assert_eq!(
            parse_vm_stat(
                "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages active: 5.\n"
            ),
            None,
            "no free-page count is no answer, not zero"
        );
    }

    /// The pool the freeze would have had, and the pool a clear machine gets.
    #[test]
    fn the_pool_is_sized_by_spare_memory_and_capped_by_cores() {
        const GB: u64 = 1 << 30;
        // The frozen machine: 3.3 GB to spare is no render at all, and the error
        // names the figures rather than a worker count.
        let err = plan_pool(3 * GB + GB / 3, 10).unwrap_err().to_string();
        assert!(err.contains("not enough free memory"), "{err}");
        assert!(err.contains("3.3 GB available"), "{err}");
        assert!(err.contains("Re-render missing"), "{err}");

        // The same machine the next evening: room for four renders of one
        // worker, not four of two — that second worker is the first to go.
        assert_eq!(
            plan_pool(34 * GB, 10).unwrap(),
            Pool {
                renders: 4,
                workers: 1
            }
        );
        // A clear 64 GB machine can afford the second worker everywhere.
        assert_eq!(
            plan_pool(60 * GB, 10).unwrap(),
            Pool {
                renders: 4,
                workers: 2
            }
        );
        // A 16 GB laptop with 10 GB free renders one at a time, and does render.
        assert_eq!(
            plan_pool(10 * GB, 8).unwrap(),
            Pool {
                renders: 1,
                workers: 1
            }
        );
        // The line for one render, from both sides.
        assert!(plan_pool(HEADROOM + render_cost(1) - 1, 8).is_err());
        assert_eq!(
            plan_pool(HEADROOM + render_cost(1), 8).unwrap(),
            Pool {
                renders: 1,
                workers: 1
            }
        );
        // A workstation with memory to burn is still held to the core budget.
        assert_eq!(
            plan_pool(200 * GB, 64).unwrap(),
            Pool {
                renders: 4,
                workers: 2
            }
        );
    }

    /// Whatever the memory says, the pool's footprint stays within the core
    /// budget the previous sizing was written for, and within the memory it was
    /// given — the two constraints this replaces one with.
    #[test]
    fn a_memory_sized_pool_fits_both_budgets() {
        for cores in [1usize, 2, 4, 8, 10, 16, 64] {
            for available in [10u64, 16, 32, 64, 128, 512] {
                let Ok(pool) = plan_pool(available << 30, cores) else {
                    continue;
                };
                assert!(
                    pool.renders * pool.workers <= capture_budget(cores),
                    "{cores} cores, {available} GB: {pool:?} exceeds the core budget"
                );
                assert!(
                    HEADROOM + pool.renders as u64 * render_cost(pool.workers) <= available << 30,
                    "{cores} cores, {available} GB: {pool:?} does not fit in memory"
                );
            }
        }
    }

    /// Retiring frees memory only while someone is left to finish the work.
    #[test]
    fn the_last_worker_never_retires() {
        let alive = AtomicUsize::new(3);
        assert!(retire_unless_last(&alive));
        assert!(retire_unless_last(&alive));
        assert_eq!(alive.load(Ordering::Relaxed), 1);
        assert!(!retire_unless_last(&alive), "the last worker stays");
        assert!(!retire_unless_last(&alive));
        assert_eq!(alive.load(Ordering::Relaxed), 1);
    }

    fn card(id: &str) -> Job {
        Job {
            id: id.to_string(),
            kind: Kind::Card,
            composition: format!("compositions/{id}.html"),
            sources: Vec::new(),
        }
    }

    /// The longform's order, and which of its parts a browser touches.
    ///
    /// This is the shape of the fix: bodies are the cut files themselves, in place, and
    /// only the cards are rendered. Getting the order wrong would ship a video whose
    /// chapters play in the wrong sequence, which no test of the renderer would catch.
    #[test]
    fn only_the_cards_render_and_the_bodies_pass_through_in_order() {
        let dir = temp("segments");
        let cuts: Vec<PathBuf> = (1..=3)
            .map(|n| {
                let p = dir.join(format!("chapter-{n:02}-horizontal.mp4"));
                std::fs::write(&p, b"cut").unwrap();
                p
            })
            .collect();
        let plan = Plan {
            targets: Default::default(),
            horizontal: dir.clone(),
            vertical: dir.clone(),
            // What `prepare` builds: no card before the first chapter.
            h_segments: vec![
                Segment::Passthrough(cuts[0].clone()),
                Segment::Render(card("seg-02-card")),
                Segment::Passthrough(cuts[1].clone()),
                Segment::Render(card("seg-03-card")),
                Segment::Passthrough(cuts[2].clone()),
            ],
            v_jobs: vec![Job {
                id: "chapter-01".into(),
                kind: Kind::Vertical,
                composition: "compositions/chapter-01.html".into(),
                sources: Vec::new(),
            }],
        };

        // Three of the five horizontal segments never reach a browser.
        assert_eq!(plan.render_count(), 3, "two cards and one vertical");
        assert_eq!(
            plan.h_segments.iter().filter(|s| s.job().is_none()).count(),
            3,
            "every chapter body is a passthrough"
        );
        // And the order is card-between-chapters, opening on footage.
        let ids: Vec<String> = plan
            .h_segments
            .iter()
            .map(|s| match s {
                Segment::Render(job) => job.id.clone(),
                Segment::Passthrough(p) => p.file_name().unwrap().to_string_lossy().into_owned(),
            })
            .collect();
        assert_eq!(
            ids,
            [
                "chapter-01-horizontal.mp4",
                "seg-02-card",
                "chapter-02-horizontal.mp4",
                "seg-03-card",
                "chapter-03-horizontal.mp4",
            ]
        );
        assert!(!plan.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plan of nothing but cards has no chapter to take its frame rate from, and must
    /// pass the parts through rather than fail reading a reference that is not there.
    #[test]
    fn a_plan_with_no_chapters_needs_no_reference_frame_rate() {
        let dir = temp("cardsonly");
        let plan = Plan {
            targets: Default::default(),
            horizontal: dir.clone(),
            vertical: dir.clone(),
            h_segments: vec![Segment::Render(card("seg-02-card"))],
            v_jobs: Vec::new(),
        };
        let rendered = vec![dir.join("seg-02-card.mp4")];
        assert_eq!(
            conform_for_concat(&plan, &rendered, &dir).unwrap(),
            rendered
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The join is fresh against its part list as well as its parts: a layout
    /// change that touches no part — the opening title card coming out — used
    /// to leave the old longform "current" with the card still in it.
    #[test]
    fn a_changed_part_list_makes_the_longform_stale() {
        let dir = temp("parts");
        let parts: Vec<PathBuf> = ["a.mp4", "b.mp4"]
            .iter()
            .map(|name| {
                let part = dir.join(name);
                std::fs::write(&part, b"x").unwrap();
                part
            })
            .collect();
        let inputs = join_inputs(&dir, &parts).unwrap();
        let longform = dir.join(LONGFORM);
        std::fs::write(&longform, b"joined").unwrap();
        assert!(is_fresh(&longform, &inputs), "just joined from these parts");
        // Same parts, same list: the list is not rewritten, so still current.
        assert!(is_fresh(&longform, &join_inputs(&dir, &parts).unwrap()));
        // One part gone and nothing else touched: the list changes, the join is due.
        assert!(!is_fresh(
            &longform,
            &join_inputs(&dir, &parts[1..]).unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What rides on a failed render's error: the reason hyperframes gave, not
    /// the frame counters it printed on the way there.
    #[test]
    fn the_log_tail_is_the_last_lines_with_anything_on_them() {
        let log = "starting\nframe 1\rframe 2\rframe 3\n\n  Error: page crashed  \n\n";
        assert_eq!(
            tail_lines(log, 3),
            vec!["frame 2", "frame 3", "Error: page crashed"]
        );
        assert_eq!(tail_lines("", 3), Vec::<&str>::new());
        assert_eq!(tail_lines("one line", 3), vec!["one line"]);

        let dir = temp("tail");
        let path = dir.join("chapter-06.log");
        assert_eq!(
            log_tail(&path, 3),
            "",
            "a log that was never written adds nothing"
        );
        std::fs::write(&path, "a\nb\nc\nd\n").unwrap();
        assert_eq!(log_tail(&path, 3), "\n      b\n      c\n      d");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_output_is_never_fresh() {
        let dir = temp("missing");
        let source = dir.join("in.html");
        std::fs::write(&source, "x").unwrap();
        assert!(!is_fresh(&dir.join("out.mp4"), &[source]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_output_is_never_fresh() {
        let dir = temp("empty");
        let source = dir.join("in.html");
        let dest = dir.join("out.mp4");
        std::fs::write(&source, "x").unwrap();
        std::fs::write(&dest, "").unwrap();
        assert!(
            !is_fresh(&dest, &[source]),
            "a zero-byte render is a failed render"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The guard that keeps silence from meaning "unchanged".
    #[test]
    fn a_job_with_no_declared_sources_is_never_fresh() {
        let dir = temp("nosources");
        let dest = dir.join("out.mp4");
        std::fs::write(&dest, "video").unwrap();
        assert!(!is_fresh(&dest, &[]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unstattable_source_is_never_fresh() {
        let dir = temp("ghost");
        let dest = dir.join("out.mp4");
        std::fs::write(&dest, "video").unwrap();
        assert!(
            !is_fresh(&dest, &[dir.join("does-not-exist.html")]),
            "an input we cannot stat is an input we cannot vouch for"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_output_newer_than_its_sources_is_fresh() {
        let dir = temp("fresh");
        let source = dir.join("in.html");
        let dest = dir.join("out.mp4");
        std::fs::write(&source, "composition").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&dest, "video").unwrap();
        assert!(is_fresh(&dest, &[source]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_touched_after_the_render_invalidates_it() {
        let dir = temp("stale");
        let source = dir.join("in.html");
        let dest = dir.join("out.mp4");
        std::fs::write(&source, "composition").unwrap();
        std::fs::write(&dest, "video").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // The chapter was re-cut after the render landed.
        std::fs::write(&source, "composition v2").unwrap();
        assert!(!is_fresh(&dest, &[source]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A job is only fresh when *every* input is older — one changed asset is enough.
    #[test]
    fn one_stale_source_among_many_invalidates_the_job() {
        let dir = temp("multi");
        let html = dir.join("in.html");
        let video = dir.join("cam.mp4");
        let dest = dir.join("out.mp4");
        std::fs::write(&html, "composition").unwrap();
        std::fs::write(&video, "camera").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&dest, "video").unwrap();
        assert!(is_fresh(&dest, &[html.clone(), video.clone()]));
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&video, "camera v2").unwrap();
        assert!(!is_fresh(&dest, &[html, video]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
