//! Render on AWS: GPU machines started for one render and gone after it.
//!
//! Why not Lambda: HyperFrames' chunked Lambda path forces SwiftShader, and the
//! talking-head compositions draw at ~5-10 s a frame in software — every chunk
//! hit Lambda's 900 s ceiling. On an NVIDIA T4 the same 3190-frame chapter
//! rendered whole in 299 s (benchmark of 2026-09-23). Software on 16 vCPUs did
//! 480 frames in 45 minutes, which rules out Fargate as well.
//!
//! One press, start to finish:
//!
//! 1. **Stage** every pending job — see [`super::stage`]: the job page as
//!    `index.html` beside hard links to only the footage it uses — and hash
//!    each file. Hard links share an inode, so a file used by five jobs is read
//!    once.
//! 2. **Upload** the files S3 does not have yet under `blobs/<sha256>`. A
//!    re-render that reuses the footage uploads nothing but the pages.
//! 3. **Split** the jobs over machines — one per two heavy jobs, up to
//!    [`MAX_MACHINES`], longest first onto the least loaded — and write the
//!    manifest to `runs/<id>/manifest.json`.
//! 4. **Launch** one machine per shard from the stack's launch template
//!    (`infra/gpu-render`). Each runs `worker/worker.py`: install and download at
//!    once, render on the GPU two at a time, upload each output, power off —
//!    which the template turns into a terminate.
//! 5. **Watch** `runs/<id>/status/<shard>.json` and download each output the
//!    moment its job says done, to exactly where a local render writes it, so
//!    the join, review, YouTube and Distribute never know the difference.
//!
//! Every machine this press launched is terminated when it returns, however it
//! returns; each one also powers itself off after [`MAX_MINUTES`] regardless,
//! so nothing can bill overnight.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aws_sdk_ec2::error::ProvideErrorMetadata;
use aws_sdk_ec2::types::{
    InstanceNetworkInterfaceSpecification, InstanceStateName, LaunchTemplateSpecification,
    ResourceType, Tag, TagSpecification,
};
use aws_sdk_s3::primitives::ByteStream;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compose::{Job, Kind};
use super::render::{self, Board};

/// The deployment `infra/gpu-render` creates: its `name_prefix`, which is also
/// the SSM path the stack's coordinates are read from.
/// `SAAGA_GPU_RENDER_STACK` names another.
pub const DEFAULT_STACK: &str = "saaga-gpu-render";
const DEFAULT_REGION: &str = "us-east-1";
/// Most machines one render starts. Each is 8 vCPUs, so 8 is 64 of the
/// account's 768-vCPU G quota.
const MAX_MACHINES: usize = 8;
/// Jobs one machine renders at once. One, for speed: two to a machine shared
/// the eight cores and the T4, and chapter-03 took 458 s beside another where
/// it took 299 s alone (2026-09-23). A second machine costs cents; the wait is
/// what the operator pays for.
const CONCURRENCY: usize = 1;
/// Chrome capture workers per render — what the 299 s benchmark ran with.
const WORKERS: usize = 4;
/// A job whose inputs pass this is a chapter's worth of footage; a machine is
/// started for every [`CONCURRENCY`] of them. Cards and pages are far smaller.
const HEAVY_BYTES: u64 = 20 << 20;
/// Every machine powers itself off after this, whatever it is doing.
const MAX_MINUTES: u32 = 90;
/// A render that reports no progress for this long is killed on the machine.
const STALL_SECONDS: u64 = 600;
/// A machine that has not written a status file this long after launch never
/// got as far as the worker.
///
/// Such a machine, or one that failed before rendering a frame — the install
/// racing Ubuntu's own updater for the package lock, a mirror blip — is
/// replaced once: a fresh machine is cents and a minute, and the operator
/// otherwise has to press Render again for a fault that was never theirs.
const BOOT_DEADLINE: Duration = Duration::from_secs(8 * 60);
const POLL: Duration = Duration::from_secs(5);
/// The chrome-headless-shell the benchmark rendered with. Pinned, so a render
/// does not change because Chrome shipped.
const CHROME_VERSION: &str = "154.0.8037.57";
/// One JSON line per finished cloud job, beside the renders.
pub const LEDGER: &str = "gpu.jsonl";

/// One job to render in the cloud, and where its output belongs.
pub struct Task<'a> {
    pub workspace: &'a Path,
    pub job: &'a Job,
    pub dest: &'a Path,
}

pub fn stack() -> String {
    std::env::var("SAAGA_GPU_RENDER_STACK")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_STACK.to_string())
}

fn region() -> String {
    std::env::var("AWS_REGION")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// Fails once, up front, when AWS cannot be reached — one message rather than
/// a failure per chapter.
pub fn check_ready() -> Result<()> {
    let status = Command::new("aws")
        .args(["sts", "get-caller-identity"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => bail!(
            "AWS is not signed in — run `aws sso login --profile dev`, then press Render again \
             (or untick Render on AWS to draw on this Mac)"
        ),
        Err(err) => bail!("could not run the aws CLI to check the sign-in: {err}"),
    }
}

// ---------------------------------------------------------------------------
// The manifest and status files — the contract with worker/worker.py
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Manifest {
    run: String,
    settings: Settings,
    jobs: Vec<ManifestJob>,
    /// `shards[k]` is the job ids machine `k` renders.
    shards: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct Settings {
    hf_version: &'static str,
    chrome: &'static str,
    quality: &'static str,
    crf: &'static str,
    frame_format: &'static str,
    concurrency: usize,
    workers: usize,
    stall_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestJob {
    id: String,
    resolution: &'static str,
    /// Path inside the job's project → the blob it is.
    files: BTreeMap<String, String>,
    /// Bytes of input, the stand-in for how long it renders.
    weight: u64,
}

#[derive(Debug, Default, Deserialize)]
struct ShardStatus {
    #[serde(default)]
    phase: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    jobs: HashMap<String, JobStatus>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct JobStatus {
    #[serde(default)]
    state: String,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    seconds: Option<f64>,
}

/// The cloud name for a job: unique across both workspaces, and safe as a file
/// name on either end.
fn cloud_id(job: &Job) -> String {
    let side = match job.kind {
        Kind::Vertical => "vertical",
        Kind::Card | Kind::Body => "horizontal",
    };
    format!("{side}-{}", job.id)
}

/// Shards `jobs` (by weight) over as many machines as the heavy ones call for:
/// longest first, each onto the least loaded machine.
fn split(jobs: &[ManifestJob]) -> Vec<Vec<String>> {
    let heavy = jobs.iter().filter(|j| j.weight >= HEAVY_BYTES).count();
    let machines = heavy
        .div_ceil(CONCURRENCY)
        .clamp(1, MAX_MACHINES)
        .min(jobs.len().max(1));
    let mut order: Vec<&ManifestJob> = jobs.iter().collect();
    order.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.id.cmp(&b.id)));
    let mut shards: Vec<(u64, Vec<String>)> = vec![(0, Vec::new()); machines];
    for job in order {
        let least = shards
            .iter_mut()
            .min_by_key(|(load, jobs)| (*load, jobs.len()))
            .expect("at least one machine");
        least.0 += job.weight;
        least.1.push(job.id.clone());
    }
    shards.into_iter().map(|(_, jobs)| jobs).collect()
}

// ---------------------------------------------------------------------------
// Staging and hashing
// ---------------------------------------------------------------------------

/// Every file under `root`, relative, with its sha256 — each inode hashed once
/// across the whole press through `seen`.
fn hash_tree(
    root: &Path,
    seen: &mut HashMap<(u64, u64), String>,
    blobs: &mut HashMap<String, (PathBuf, u64)>,
) -> Result<(BTreeMap<String, String>, u64)> {
    let mut files = BTreeMap::new();
    let mut weight = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            let meta = std::fs::metadata(&path)?;
            let digest = match seen.get(&(meta.dev(), meta.ino())) {
                Some(digest) => digest.clone(),
                None => {
                    let digest = sha256_file(&path)?;
                    seen.insert((meta.dev(), meta.ino()), digest.clone());
                    digest
                }
            };
            let rel = path
                .strip_prefix(root)
                .expect("walked from root")
                .to_string_lossy()
                .into_owned();
            weight += meta.len();
            blobs.entry(digest.clone()).or_insert((path, meta.len()));
            files.insert(rel, digest);
        }
    }
    Ok((files, weight))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)
            .with_context(|| format!("hashing {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// What each machine runs at boot: fetch the worker, run it, power off. The
/// hard deadline is set first, so even a worker that never starts cannot keep
/// the machine up.
fn user_data(bucket: &str, run: &str, shard: usize) -> String {
    let script = format!(
        "#!/bin/bash\n\
         exec > /var/log/render-boot.log 2>&1\n\
         set -x\n\
         shutdown -h +{MAX_MINUTES}\n\
         mkdir -p /render\n\
         aws s3 cp s3://{bucket}/worker/worker.py /render/worker.py\n\
         python3 /render/worker.py --bucket {bucket} --run {run} --shard {shard}\n\
         aws s3 cp /var/log/render-boot.log s3://{bucket}/runs/{run}/logs/boot-{shard}.log || true\n\
         shutdown -h now\n"
    );
    base64::engine::general_purpose::STANDARD.encode(script)
}

fn run_id(tasks: &[Task]) -> String {
    let project = tasks
        .first()
        .and_then(|t| project_folder(t.dest))
        .unwrap_or_else(|| "render".to_string());
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        project
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>()
    )
}

/// The project folder a destination belongs to: the component before `render`.
fn project_folder(dest: &Path) -> Option<String> {
    let parts: Vec<String> = dest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let at = parts.iter().rposition(|p| p == "render")?;
    at.checked_sub(1).map(|i| parts[i].clone())
}

// ---------------------------------------------------------------------------
// The press
// ---------------------------------------------------------------------------

struct Stack {
    bucket: String,
    launch_template: String,
    subnets: Vec<String>,
    security_group: String,
}

struct Clients {
    s3: aws_sdk_s3::Client,
    ec2: aws_sdk_ec2::Client,
    ssm: aws_sdk_ssm::Client,
}

/// Terminates every machine this press launched when it goes out of scope —
/// on success, on an error, on a panic.
struct Machines<'a> {
    ec2: &'a aws_sdk_ec2::Client,
    runtime: &'a tokio::runtime::Runtime,
    ids: Vec<String>,
}

impl Drop for Machines<'_> {
    fn drop(&mut self) {
        if self.ids.is_empty() {
            return;
        }
        let ids = std::mem::take(&mut self.ids);
        let result = self.runtime.block_on(
            self.ec2
                .terminate_instances()
                .set_instance_ids(Some(ids.clone()))
                .send(),
        );
        match result {
            Ok(_) => eprintln!(
                "stream-recorder: terminated render machine(s) {}",
                ids.join(", ")
            ),
            Err(err) => eprintln!(
                "stream-recorder: could not terminate render machine(s) {} ({}) — each powers \
                 itself off within {MAX_MINUTES} minutes; `terraform output stop_all` in \
                 infra/gpu-render stops them now",
                ids.join(", "),
                err.message().unwrap_or("no message")
            ),
        }
    }
}

/// Renders `tasks` on GPU machines, reporting through `board` and `status`,
/// and calling `on_done` with the running count as each finishes.
pub(super) fn render_all(
    tasks: &[Task],
    board: &Board,
    status: &(dyn Fn(&str) + Sync),
    on_done: &(dyn Fn(usize) + Sync),
) -> Result<()> {
    if tasks.is_empty() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting tokio for the GPU render")?;
    let clients = runtime.block_on(async {
        let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region()))
            .load()
            .await;
        // The same credentials every call below uses, asked for once: the CLI
        // `check_ready` runs can still hold cached role credentials after the
        // SSO session behind them has expired, and then the first SDK call
        // fails with a page of provider-chain detail instead of one sentence.
        use aws_credential_types::provider::ProvideCredentials;
        if let Some(provider) = shared.credentials_provider() {
            if let Err(err) = provider.provide_credentials().await {
                eprintln!("stream-recorder: AWS credentials: {err}");
                bail!(
                    "the AWS sign-in has expired — run `aws sso login --profile dev`, then press \
                     Render again (or untick Render on AWS GPU to draw on this Mac)"
                );
            }
        }
        Ok::<_, anyhow::Error>(Clients {
            s3: aws_sdk_s3::Client::new(&shared),
            ec2: aws_sdk_ec2::Client::new(&shared),
            ssm: aws_sdk_ssm::Client::new(&shared),
        })
    })?;
    let stack = runtime.block_on(read_stack(&clients.ssm))?;

    // 1. Stage and hash.
    board.note("staging");
    status(&board.line());
    let mut seen = HashMap::new();
    let mut blobs: HashMap<String, (PathBuf, u64)> = HashMap::new();
    let mut jobs = Vec::new();
    let mut by_id: HashMap<String, &Task> = HashMap::new();
    for task in tasks {
        let project = super::stage::stage(task.workspace, task.job)?;
        let (files, weight) = hash_tree(&project, &mut seen, &mut blobs)?;
        let id = cloud_id(task.job);
        jobs.push(ManifestJob {
            id: id.clone(),
            resolution: match task.job.kind {
                Kind::Vertical => "portrait",
                Kind::Card | Kind::Body => "landscape",
            },
            files,
            weight,
        });
        by_id.insert(id, task);
    }

    // 2. Upload what S3 does not have.
    runtime.block_on(upload_blobs(
        &clients.s3,
        &stack.bucket,
        &blobs,
        board,
        status,
    ))?;

    // 3. Split, and write the manifest.
    let run = run_id(tasks);
    let shards = split(&jobs);
    let manifest = Manifest {
        run: run.clone(),
        settings: Settings {
            hf_version: render::HF_VERSION,
            chrome: CHROME_VERSION,
            quality: render::QUALITY,
            crf: render::CRF,
            frame_format: render::VIDEO_FRAME_FORMAT,
            concurrency: CONCURRENCY,
            workers: WORKERS,
            stall_seconds: STALL_SECONDS,
        },
        jobs,
        shards: shards.clone(),
    };
    runtime
        .block_on(
            clients
                .s3
                .put_object()
                .bucket(&stack.bucket)
                .key(format!("runs/{run}/manifest.json"))
                .content_type("application/json")
                .body(ByteStream::from(serde_json::to_vec(&manifest)?))
                .send(),
        )
        .map_err(|err| aws_error("writing the render manifest", &err))?;

    // 4. Launch — and from here every exit terminates what was launched.
    let mut machines = Machines {
        ec2: &clients.ec2,
        runtime: &runtime,
        ids: Vec::new(),
    };
    board.note(&format!("starting {} GPU machine(s)", shards.len()));
    status(&board.line());
    eprintln!(
        "stream-recorder: GPU render {run}: {} job(s) on {} machine(s) — {}",
        by_id.len(),
        shards.len(),
        shards
            .iter()
            .enumerate()
            .map(|(k, jobs)| format!("#{k}: {}", jobs.join(" ")))
            .collect::<Vec<_>>()
            .join("; ")
    );
    // The machine each shard is on now, and when it was started. Every
    // machine ever launched stays in `machines` for the terminate at the end.
    let mut current: Vec<String> = Vec::new();
    let mut launched_at: Vec<Instant> = Vec::new();
    let mut replaced = vec![false; shards.len()];
    for shard in 0..shards.len() {
        let id = runtime.block_on(launch(&clients.ec2, &stack, &run, shard))?;
        machines.ids.push(id.clone());
        current.push(id);
        launched_at.push(Instant::now());
    }
    let launched = Instant::now();

    // 5. Watch, and bring each output home as it lands.
    let mut finished: HashSet<String> = HashSet::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut started: HashSet<String> = HashSet::new();
    let mut last_states = Instant::now() - Duration::from_secs(60);
    let mut states: HashMap<String, (InstanceStateName, String)> = HashMap::new();
    let mut aws_trouble: Option<Instant> = None;
    let deadline = launched + Duration::from_secs(u64::from(MAX_MINUTES) * 60 + 300);
    let total = by_id.len();
    while finished.len() < total {
        if Instant::now() > deadline {
            for id in by_id.keys().filter(|id| !finished.contains(*id)) {
                failures.push((
                    id.clone(),
                    format!("still unfinished after {MAX_MINUTES} minutes"),
                ));
            }
            break;
        }
        std::thread::sleep(POLL);
        if last_states.elapsed() >= Duration::from_secs(20) {
            match runtime.block_on(instance_states(&clients.ec2, &machines.ids)) {
                Ok(now) => {
                    states = now;
                    last_states = Instant::now();
                }
                Err(err) => eprintln!("stream-recorder: reading the machines' state: {err:#}"),
            }
        }
        let mut phases: BTreeMap<String, usize> = BTreeMap::new();
        for (shard, shard_jobs) in shards.iter().enumerate() {
            let instance = current[shard].clone();
            let instance = &instance;
            let doc = match runtime.block_on(read_status(&clients.s3, &stack.bucket, &run, shard)) {
                Ok(doc) => {
                    aws_trouble = None;
                    doc
                }
                Err(err) => {
                    // A throttle or a signed-out Mac: the machines carry on
                    // regardless. Give it a while before calling the render lost.
                    let since = *aws_trouble.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_secs(10 * 60) {
                        bail!(
                            "lost contact with AWS for 10 minutes ({err:#}) — sign in with `aws \
                             sso login --profile dev` and press Render again; finished renders \
                             are kept"
                        );
                    }
                    None
                }
            };
            let gone = states.get(instance).filter(|(state, _)| {
                matches!(
                    state,
                    InstanceStateName::ShuttingDown | InstanceStateName::Terminated
                )
            });
            // A machine that died without rendering anything gets one replacement.
            let fell_over = match &doc {
                None => gone.is_some() || launched_at[shard].elapsed() > BOOT_DEADLINE,
                Some(doc) => {
                    doc.phase == "failed"
                        && !doc
                            .jobs
                            .values()
                            .any(|j| j.state == "done" || j.progress > 0.0)
                }
            };
            if fell_over && !replaced[shard] {
                replaced[shard] = true;
                let why = doc
                    .as_ref()
                    .map(|d| d.message.clone())
                    .unwrap_or_else(|| "it never reported".to_string());
                eprintln!(
                    "stream-recorder: GPU machine {instance} (shard {shard}) failed before \
                     rendering ({why}) — starting a replacement"
                );
                let _ = runtime.block_on(
                    clients
                        .s3
                        .delete_object()
                        .bucket(&stack.bucket)
                        .key(format!("runs/{run}/status/{shard}.json"))
                        .send(),
                );
                let _ = runtime.block_on(
                    clients
                        .ec2
                        .terminate_instances()
                        .instance_ids(instance.as_str())
                        .send(),
                );
                let id = runtime.block_on(launch(&clients.ec2, &stack, &run, shard))?;
                machines.ids.push(id.clone());
                current[shard] = id;
                launched_at[shard] = Instant::now();
                *phases.entry("booting".into()).or_default() += 1;
                continue;
            }
            match &doc {
                None if gone.is_some() || launched_at[shard].elapsed() > BOOT_DEADLINE => {
                    let why = match gone {
                        Some((_, reason)) => format!("the machine stopped before it started ({reason})"),
                        None => format!(
                            "the machine never reported in {} minutes — see runs/{run}/logs/boot-{shard}.log",
                            BOOT_DEADLINE.as_secs() / 60
                        ),
                    };
                    let open: Vec<String> = shard_jobs
                        .iter()
                        .filter(|id| !finished.contains(*id))
                        .cloned()
                        .collect();
                    for id in open {
                        failures.push((id.clone(), why.clone()));
                        board.finish(&id, false);
                        finished.insert(id);
                        on_done(finished.len());
                    }
                    continue;
                }
                None => {
                    *phases.entry("booting".into()).or_default() += 1;
                    continue;
                }
                Some(doc) => *phases.entry(doc.phase.clone()).or_default() += 1,
            }
            let doc = doc.expect("matched above");
            for id in shard_jobs {
                if finished.contains(id) {
                    continue;
                }
                let job = doc.jobs.get(id).cloned().unwrap_or_default();
                match job.state.as_str() {
                    "rendering" | "uploading" => {
                        if started.insert(id.clone()) {
                            board.start(id);
                        }
                        board.advance(id, job.progress);
                    }
                    "done" => {
                        let task = by_id[id];
                        let outcome = runtime.block_on(bring_home(
                            &clients.s3,
                            &stack.bucket,
                            &run,
                            id,
                            task,
                        ));
                        if let Err(err) = &outcome {
                            failures.push((id.clone(), format!("{err:#}")));
                        } else {
                            append_ledger(task, &run, instance, job.seconds);
                        }
                        finished.insert(id.clone());
                        board.finish(id, outcome.is_ok());
                        on_done(finished.len());
                        eprintln!(
                            "stream-recorder: [{}/{total}] {} {id} (GPU, {}s)",
                            finished.len(),
                            if outcome.is_ok() {
                                "rendered"
                            } else {
                                "FAILED"
                            },
                            job.seconds.unwrap_or_default().round()
                        );
                    }
                    "failed" => {
                        let err = job.error.clone().unwrap_or_else(|| doc.message.clone());
                        eprintln!(
                            "stream-recorder: [{}/{total}] FAILED {id}: {err}",
                            finished.len() + 1
                        );
                        let _ = runtime.block_on(save_log(
                            &clients.s3,
                            &stack.bucket,
                            &run,
                            id,
                            by_id[id],
                        ));
                        failures.push((id.clone(), err));
                        finished.insert(id.clone());
                        board.finish(id, false);
                        on_done(finished.len());
                    }
                    _ if gone.is_some() => {
                        failures.push((
                            id.clone(),
                            format!("the machine stopped mid-render ({})", gone.unwrap().1),
                        ));
                        finished.insert(id.clone());
                        board.finish(id, false);
                        on_done(finished.len());
                    }
                    _ => {}
                }
            }
        }
        board.note(&describe_phases(&phases));
        status(&board.line());
    }
    drop(machines);

    if failures.is_empty() {
        return Ok(());
    }
    failures.sort();
    let mut message = format!("{} of {total} GPU render(s) failed:", failures.len());
    for (id, err) in &failures {
        message.push_str(&format!("\n  - {id}: {err}"));
    }
    bail!(message)
}

/// "2 rendering, 1 installing" — what the machines are doing, for the line.
fn describe_phases(phases: &BTreeMap<String, usize>) -> String {
    let machines: usize = phases.values().sum();
    let parts: Vec<String> = phases
        .iter()
        .map(|(phase, n)| format!("{n} {phase}"))
        .collect();
    format!("{machines} GPU machine(s): {}", parts.join(", "))
}

async fn read_stack(ssm: &aws_sdk_ssm::Client) -> Result<Stack> {
    let prefix = format!("/{}/", stack());
    let found = ssm
        .get_parameters_by_path()
        .path(&prefix)
        .send()
        .await
        .map_err(|err| aws_error("reading the GPU render stack from SSM", &err))?;
    let params: HashMap<String, String> = found
        .parameters()
        .iter()
        .filter_map(|p| {
            let name = p.name()?.strip_prefix(&prefix)?.to_string();
            Some((name, p.value()?.to_string()))
        })
        .collect();
    let get = |name: &str| params.get(name).cloned();
    match (
        get("bucket"),
        get("launch-template-id"),
        get("subnet-ids"),
        get("security-group-id"),
    ) {
        (Some(bucket), Some(launch_template), Some(subnets), Some(security_group)) => Ok(Stack {
            bucket,
            launch_template,
            subnets: subnets.split(',').map(str::to_string).collect(),
            security_group,
        }),
        _ => bail!(
            "no GPU render stack at SSM {prefix} in {} — apply infra/gpu-render with terraform",
            region()
        ),
    }
}

async fn upload_blobs(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    blobs: &HashMap<String, (PathBuf, u64)>,
    board: &Board,
    status: &(dyn Fn(&str) + Sync),
) -> Result<()> {
    // Ask about all of them at once; a HEAD is cheap and there are dozens.
    let mut checks = tokio::task::JoinSet::new();
    for digest in blobs.keys() {
        let s3 = s3.clone();
        let bucket = bucket.to_string();
        let digest = digest.clone();
        checks.spawn(async move {
            let present = s3
                .head_object()
                .bucket(&bucket)
                .key(format!("blobs/{digest}"))
                .send()
                .await
                .is_ok();
            (digest, present)
        });
    }
    let mut missing = Vec::new();
    while let Some(joined) = checks.join_next().await {
        let (digest, present) = joined.context("checking S3 for an input")?;
        if !present {
            missing.push(digest);
        }
    }
    if missing.is_empty() {
        eprintln!(
            "stream-recorder: all {} render input(s) already in S3",
            blobs.len()
        );
        return Ok(());
    }
    let total: u64 = missing.iter().map(|d| blobs[d].1).sum();
    eprintln!(
        "stream-recorder: uploading {} of {} render input(s), {} MB",
        missing.len(),
        blobs.len(),
        total >> 20
    );
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(6));
    let mut uploads = tokio::task::JoinSet::new();
    for digest in missing {
        let (path, bytes) = blobs[&digest].clone();
        let s3 = s3.clone();
        let bucket = bucket.to_string();
        let permits = permits.clone();
        uploads.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let body = ByteStream::from_path(&path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            s3.put_object()
                .bucket(&bucket)
                .key(format!("blobs/{digest}"))
                .body(body)
                .send()
                .await
                .map_err(|err| aws_error(&format!("uploading {}", path.display()), &err))?;
            Ok::<u64, anyhow::Error>(bytes)
        });
    }
    let mut sent = 0u64;
    while let Some(joined) = uploads.join_next().await {
        sent += joined.context("uploading a render input")??;
        board.note(&format!(
            "uploading footage {} of {} MB",
            sent >> 20,
            total >> 20
        ));
        status(&board.line());
    }
    Ok(())
}

async fn launch(
    ec2: &aws_sdk_ec2::Client,
    stack: &Stack,
    run: &str,
    shard: usize,
) -> Result<String> {
    let mut last = String::new();
    // Another Availability Zone when one is out of T4s.
    for subnet in &stack.subnets {
        let tags = TagSpecification::builder()
            .resource_type(ResourceType::Instance)
            .tags(
                Tag::builder()
                    .key("Name")
                    .value(format!("{}-machine", stack_name()))
                    .build(),
            )
            .tags(Tag::builder().key("Role").value(stack_name()).build())
            .tags(Tag::builder().key("RenderRun").value(run).build())
            .tags(
                Tag::builder()
                    .key("RenderShard")
                    .value(shard.to_string())
                    .build(),
            )
            .build();
        let result = ec2
            .run_instances()
            .launch_template(
                LaunchTemplateSpecification::builder()
                    .launch_template_id(&stack.launch_template)
                    .version("$Default")
                    .build(),
            )
            .min_count(1)
            .max_count(1)
            .user_data(user_data(&stack.bucket, run, shard))
            .network_interfaces(
                InstanceNetworkInterfaceSpecification::builder()
                    .device_index(0)
                    .subnet_id(subnet)
                    .groups(&stack.security_group)
                    .associate_public_ip_address(true)
                    .delete_on_termination(true)
                    .build(),
            )
            .tag_specifications(tags)
            .send()
            .await;
        match result {
            Ok(out) => {
                let id = out
                    .instances()
                    .first()
                    .and_then(|i| i.instance_id())
                    .context("RunInstances returned no instance")?
                    .to_string();
                eprintln!("stream-recorder: GPU machine {id} for shard {shard} in {subnet}");
                return Ok(id);
            }
            Err(err) => {
                let code = err.code().unwrap_or_default().to_string();
                last = format!("{code}: {}", err.message().unwrap_or("no message"));
                if matches!(
                    code.as_str(),
                    "InsufficientInstanceCapacity" | "Unsupported"
                ) {
                    eprintln!("stream-recorder: {subnet}: {last} — trying the next zone");
                    continue;
                }
                break;
            }
        }
    }
    bail!("could not start a GPU machine: {last}")
}

fn stack_name() -> String {
    stack()
}

async fn instance_states(
    ec2: &aws_sdk_ec2::Client,
    ids: &[String],
) -> Result<HashMap<String, (InstanceStateName, String)>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let out = ec2
        .describe_instances()
        .set_instance_ids(Some(ids.to_vec()))
        .send()
        .await
        .map_err(|err| aws_error("reading the render machines' state", &err))?;
    Ok(out
        .reservations()
        .iter()
        .flat_map(|r| r.instances())
        .filter_map(|i| {
            let id = i.instance_id()?.to_string();
            let state = i.state()?.name()?.clone();
            let reason = i
                .state_reason()
                .and_then(|r| r.message())
                .unwrap_or("no reason given")
                .to_string();
            Some((id, (state, reason)))
        })
        .collect())
}

/// One machine's status file; `Ok(None)` before it has written one.
async fn read_status(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    run: &str,
    shard: usize,
) -> Result<Option<ShardStatus>> {
    let got = s3
        .get_object()
        .bucket(bucket)
        .key(format!("runs/{run}/status/{shard}.json"))
        .send()
        .await;
    let object = match got {
        Ok(object) => object,
        Err(err) if err.as_service_error().is_some_and(|e| e.is_no_such_key()) => return Ok(None),
        Err(err) => return Err(aws_error("reading a render machine's status", &err)),
    };
    let bytes = object
        .body
        .collect()
        .await
        .context("reading a status file")?
        .into_bytes();
    Ok(Some(
        serde_json::from_slice(&bytes).context("parsing a status file")?,
    ))
}

/// Downloads a finished job beside its destination and renames it into place,
/// so a dropped connection never leaves a truncated file where a render belongs.
async fn bring_home(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    run: &str,
    id: &str,
    task: &Task<'_>,
) -> Result<()> {
    let tmp = task
        .dest
        .with_file_name(format!(".{}.rendering.mp4", task.job.id));
    let mut object = s3
        .get_object()
        .bucket(bucket)
        .key(format!("runs/{run}/out/{id}.mp4"))
        .send()
        .await
        .map_err(|err| aws_error(&format!("downloading {id}"), &err))?;
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    while let Some(chunk) = object
        .body
        .try_next()
        .await
        .with_context(|| format!("downloading {id}"))?
    {
        file.write_all(&chunk)
            .with_context(|| format!("writing {}", tmp.display()))?;
    }
    drop(file);
    if std::fs::metadata(&tmp)?.len() == 0 {
        let _ = std::fs::remove_file(&tmp);
        bail!("{id} came back empty");
    }
    std::fs::rename(&tmp, task.dest)
        .with_context(|| format!("publishing {}", task.dest.display()))?;
    // A log beside a render is kept only for a failure; this one succeeded.
    let _ = std::fs::remove_file(task.dest.with_extension("log"));
    Ok(())
}

/// A failed job's hyperframes log, beside where its output would have gone.
async fn save_log(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    run: &str,
    id: &str,
    task: &Task<'_>,
) -> Result<()> {
    let object = s3
        .get_object()
        .bucket(bucket)
        .key(format!("runs/{run}/logs/{id}.log"))
        .send()
        .await
        .map_err(|err| aws_error("downloading a render log", &err))?;
    let bytes = object.body.collect().await?.into_bytes();
    std::fs::write(task.dest.with_extension("log"), &bytes)?;
    Ok(())
}

fn append_ledger(task: &Task, run: &str, instance: &str, seconds: Option<f64>) {
    let Some(ledger) = task
        .dest
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join(LEDGER))
    else {
        return;
    };
    let line = serde_json::json!({
        "at": chrono::Local::now().to_rfc3339(),
        "job": task.job.id,
        "composition": task.job.composition,
        "run": run,
        "instance": instance,
        "seconds": seconds,
        "file": task.dest.display().to_string(),
    });
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&ledger) {
        let _ = writeln!(file, "{line}");
    }
}

fn aws_error<E, R>(what: &str, err: &aws_sdk_s3::error::SdkError<E, R>) -> anyhow::Error
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
    R: std::fmt::Debug,
{
    let detail = err
        .as_service_error()
        .and_then(|e| e.message().map(str::to_string))
        .unwrap_or_else(|| format!("{}", aws_sdk_s3::error::DisplayErrorContext(err)));
    anyhow::anyhow!("{what}: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, weight: u64) -> ManifestJob {
        ManifestJob {
            id: id.into(),
            resolution: "portrait",
            files: BTreeMap::new(),
            weight,
        }
    }

    #[test]
    fn a_machine_per_two_heavy_jobs_longest_first() {
        let mb = 1 << 20;
        let jobs = vec![
            job("vertical-chapter-01", 10 * mb),
            job("vertical-chapter-02", 60 * mb),
            job("vertical-chapter-03", 170 * mb),
            job("vertical-chapter-04", 280 * mb),
            job("vertical-chapter-05", 30 * mb),
            job("horizontal-seg-02-card", mb / 2),
            job("horizontal-seg-03-card", mb / 2),
        ];
        let shards = split(&jobs);
        // Four heavy jobs, one machine each; the light ones ride along.
        assert_eq!(shards.len(), 4 / CONCURRENCY);
        // The heaviest two land on different machines.
        let holding = |id: &str| {
            shards
                .iter()
                .position(|s| s.iter().any(|j| j == id))
                .unwrap()
        };
        assert_ne!(
            holding("vertical-chapter-04"),
            holding("vertical-chapter-03")
        );
        // Every job is somewhere, once.
        let mut all: Vec<&String> = shards.iter().flatten().collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), jobs.len());
    }

    #[test]
    fn never_more_machines_than_the_cap_or_the_jobs() {
        let many: Vec<ManifestJob> = (0..40).map(|n| job(&format!("j{n}"), 100 << 20)).collect();
        assert_eq!(split(&many).len(), MAX_MACHINES);
        let light = vec![job("card", 1024)];
        assert_eq!(split(&light).len(), 1);
    }

    #[test]
    fn the_user_data_sets_the_deadline_before_anything_else() {
        let encoded = user_data("bucket", "run-1", 3);
        let script = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        )
        .unwrap();
        let deadline = script.find("shutdown -h +").unwrap();
        let worker = script
            .find("worker.py --bucket bucket --run run-1 --shard 3")
            .unwrap();
        assert!(deadline < worker, "{script}");
        assert!(script.trim_end().ends_with("shutdown -h now"));
    }

    #[test]
    fn a_run_is_named_for_its_project() {
        let dest = Path::new("/x/sessions/2026-09-23_03-51-42/render/v2/vertical/chapter-03.mp4");
        assert_eq!(project_folder(dest).as_deref(), Some("2026-09-23_03-51-42"));
        assert_eq!(
            cloud_id(&Job {
                id: "chapter-03".into(),
                kind: Kind::Vertical,
                composition: "c.html".into(),
                sources: vec![],
            }),
            "vertical-chapter-03"
        );
    }
}
