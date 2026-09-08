use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::progress::{self, Progress};
use super::schema::{DistributeLinks, DistributedAsset};
use super::{Asset, AssetKind};

pub fn upload_assets(
    assets: &[Asset],
    project: &str,
    version: u32,
    dest: &Path,
    status: &dyn Fn(&str),
    progress: &dyn Fn(&str, Progress),
) -> Result<DistributeLinks> {
    let mut items = Vec::new();
    for (index, asset) in assets.iter().enumerate() {
        status(&format!(
            "Uploading {} ({}/{})…",
            asset.id,
            index + 1,
            assets.len()
        ));
        let url = upload_one(asset, project, version, &|update| progress(&asset.id, update))?;
        items.push(DistributedAsset {
            id: asset.id.clone(),
            kind: match asset.kind {
                AssetKind::Long | AssetKind::Chapter => "video".into(),
                AssetKind::File => "transcript".into(),
                AssetKind::Image => "image".into(),
            },
            orientation: asset.orientation.clone(),
            chapter: asset.chapter,
            url,
            file: asset
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
        });
    }
    let links = DistributeLinks {
        project: project.to_string(),
        version,
        items,
    };
    super::schema::save(dest, &links)?;
    Ok(links)
}

fn upload_one(
    asset: &Asset,
    project: &str,
    version: u32,
    on_progress: &dyn Fn(Progress),
) -> Result<String> {
    let home = screencast_home();
    let mut cmd = Command::new("uv");
    cmd.current_dir(&home)
        .args(["run", "python", "-m", "screencast.platforms.upload_cli"])
        .args(["--file", &asset.path.to_string_lossy()])
        .args(["--project", project])
        .args(["--version", &version.to_string()])
        .args(["--kind", asset.kind.as_str()])
        .args(["--content-type", asset.content_type]);
    if let Some(orientation) = &asset.orientation {
        cmd.args(["--orientation", orientation]);
    }
    if let Some(chapter) = asset.chapter {
        cmd.args(["--chapter", &chapter.to_string()]);
    }
    if let Some(name) = asset
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    {
        cmd.args(["--filename", &name]);
    }
    // Piped, not `output()`: the bar repaints with `\r` and only newlines at the
    // end, so buffering the child would hold every update until the upload is
    // already finished — which is the whole problem this solves.
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("running upload_cli from {}", home.display()))?;

    // stdout carries only the final URL, but it still has to be drained on its own
    // thread: filling either pipe's buffer would deadlock the child.
    let mut out_pipe = child.stdout.take().context("upload_cli stdout")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = out_pipe.read_to_string(&mut buf);
        buf
    });

    let mut err_pipe = child.stderr.take().context("upload_cli stderr")?;
    let mut stderr = String::new();
    let mut pending = String::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = err_pipe.read(&mut chunk).unwrap_or(0);
        if read == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&chunk[..read]);
        stderr.push_str(&text);
        pending.push_str(&text);
        // Keep whatever follows the last separator: it is a partial repaint that
        // the next read completes.
        let tail = match pending.rfind(['\r', '\n']) {
            Some(at) => pending.split_off(at + 1),
            None => continue,
        };
        for segment in progress::segments(&pending) {
            match progress::parse(segment) {
                Some(update) => on_progress(update),
                None => eprintln!("{segment}"),
            }
        }
        pending = tail;
    }
    for segment in progress::segments(&pending) {
        if let Some(update) = progress::parse(segment) {
            on_progress(update);
        } else {
            eprintln!("{segment}");
        }
    }

    let status = child
        .wait()
        .with_context(|| format!("waiting for upload_cli for {}", asset.id))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("upload_cli stdout reader panicked"))?
        .trim()
        .to_string();
    let stderr = stderr.trim().to_string();
    if !status.success() {
        bail!(
            "S3 upload failed for {} ({})",
            asset.id,
            if stderr.is_empty() {
                stdout.clone()
            } else {
                // The trailing lines carry the actual error; the bar above is noise.
                stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ")
            }
        );
    }
    if stdout.is_empty() {
        bail!("S3 upload for {} printed no URL", asset.id);
    }
    Ok(stdout.lines().last().unwrap_or(&stdout).to_string())
}

pub(crate) fn screencast_home() -> PathBuf {
    std::env::var("SCREENCAST_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../screencast")
        })
}

impl AssetKind {
    fn as_str(self) -> &'static str {
        match self {
            AssetKind::Long => "long",
            AssetKind::Chapter => "chapter",
            // The uploader takes long | chapter | file and nothing else, so an
            // image rides in as a plain file — its content type carries the rest.
            AssetKind::File | AssetKind::Image => "file",
        }
    }
}
