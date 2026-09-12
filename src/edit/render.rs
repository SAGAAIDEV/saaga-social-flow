//! Render prepared HyperFrames compositions and assemble the long form.
//!
//! Each job is an independent headless-browser render writing to its own file, so
//! they run on a small pool rather than one at a time. The pool is deliberately
//! *small*: a render is memory- and CPU-hungry, and oversubscribing turns a fast
//! machine into a swapping one. Concat order comes from the plan, never from the
//! order jobs happen to finish.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};

use super::compose::{Job, Kind, Plan, Segment};
use super::cut;

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
/// How HyperFrames pulls frames out of the footage it composites: PNG, in place
/// of the JPEG it picks for anything without alpha, so the cut is not run
/// through a third lossy codec on its way into Chrome.
pub const VIDEO_FRAME_FORMAT: &str = "png";

/// Everything about the encode a rendered file depends on, as one line.
///
/// Written to the workspace as [`super::compose::QUALITY_FILE`] and declared a
/// source of every job, so a change to any of these re-renders everything
/// exactly once.
pub fn encoder_stamp() -> String {
    format!("{QUALITY} crf={CRF} frames={VIDEO_FRAME_FORMAT}")
}
/// Pinned so the cache path, the npx fallback and the preflight check cannot
/// drift apart into three different renderers.
pub const HF_VERSION: &str = "0.7.107";
/// The assembled cut, in whichever orientation's directory it lands.
pub const LONGFORM: &str = "longform.mp4";

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
    status: &dyn Fn(&str),
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
    render_all(&pending, &|finished| {
        progress(skipped.len() + finished, all)
    })?;

    // The one step after the renders with a wait worth naming: the cards are
    // conformed to the footage and everything is joined. Each longform only
    // when it was asked for, and only when a part is newer than the last join —
    // so ticking the vertical longform after a render that already drew the
    // shorts costs the join and nothing else.
    if !plan.h_segments.is_empty() {
        let longform = h_dir.join(LONGFORM);
        if is_fresh(&longform, &h_out) {
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
        if is_fresh(&longform, &v_out) {
            eprintln!("stream-recorder: the vertical longform is already current");
        } else {
            status("Assembling the vertical longform…");
            join(&v_out, &longform)?;
        }
    }
    Ok(publish.to_path_buf())
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
fn render_all(tasks: &[Task], on_done: &(dyn Fn(usize) + Sync)) -> Result<()> {
    if tasks.is_empty() {
        return Ok(());
    }
    let workers = worker_count().min(tasks.len());
    let total = tasks.len();
    eprintln!("stream-recorder: rendering {total} composition(s) on {workers} worker(s)");

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let failures: Mutex<Vec<(String, anyhow::Error)>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(task) = tasks.get(index) else { break };
                let outcome = render_job(task.workspace, task.job, &task.dest);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
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

/// How many renders to run at once. `SCREENCAST_RENDER_WORKERS` overrides.
fn worker_count() -> usize {
    if let Ok(raw) = std::env::var("SCREENCAST_RENDER_WORKERS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            return n.clamp(1, 8);
        }
    }
    pool_size(cores())
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

fn render_job(workspace: &Path, job: &Job, dest: &Path) -> Result<()> {
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
        &workers_per_render(cores()).to_string(),
        "--resolution",
        match job.kind {
            Kind::Vertical => "portrait",
            Kind::Card => "landscape",
        },
        "--output",
    ]);
    cmd.arg(&tmp);
    eprintln!("stream-recorder: {}", command_line(&cmd));
    let status = cmd
        .status()
        .with_context(|| format!("starting hyperframes render for {}", job.id))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "hyperframes render failed for {} ({status})",
            job.composition
        );
    }
    if !tmp.is_file() || tmp.metadata()?.len() == 0 {
        let _ = std::fs::remove_file(&tmp);
        bail!("hyperframes produced no output for {}", job.id);
    }
    std::fs::rename(&tmp, dest).with_context(|| format!("publishing {}", dest.display()))?;
    Ok(())
}

fn hyperframes_command() -> Result<Command> {
    if let Some(home) = std::env::var_os("HOME") {
        let cached = PathBuf::from(home)
            .join(".screencast/cache/hyperframes")
            .join(HF_VERSION)
            .join("cli/node_modules/.bin/hyperframes");
        if cached.is_file() {
            return Ok(Command::new(cached));
        }
    }
    let mut cmd = Command::new("npx");
    cmd.args(["--yes", &format!("hyperframes@{HF_VERSION}")]);
    Ok(cmd)
}

fn command_line(cmd: &Command) -> String {
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
