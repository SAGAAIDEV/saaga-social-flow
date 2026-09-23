//! Stage one render job as a project of its own: the job page as `index.html`,
//! beside hard links to only the files it uses.
//!
//! The GPU machines render one job each — see [`super::gpu`] — and a whole
//! workspace carries every chapter's footage, gigabytes of it. A job page only
//! ever refers to paths from the workspace root (`compositions/…`,
//! `assets/videos/chapter-02/…`), so the page copied to `index.html` beside
//! links to what it uses renders the same, and uploads only its own chapter.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::compose::Job;

/// Builds the project a GPU machine renders for `job`: the job page as `index.html`,
/// and hard links to the rest of the workspace minus other chapters' footage.
///
/// Beside the workspace, not inside it, so a local render of the workspace
/// never sees a second copy of everything. Rebuilt every time — it is links,
/// so it costs nothing — and the upload is content-addressed, so a file S3
/// already holds is not sent again.
pub fn stage(workspace: &Path, job: &Job) -> Result<PathBuf> {
    let name = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    let root = workspace
        .with_file_name(format!(".stage-{name}"))
        .join(&job.id);
    if root.exists() {
        std::fs::remove_dir_all(&root).with_context(|| format!("clearing {}", root.display()))?;
    }
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;

    let page_path = workspace.join(&job.composition);
    let page = std::fs::read_to_string(&page_path)
        .with_context(|| format!("reading {}", page_path.display()))?;
    std::fs::write(root.join("index.html"), &page)?;

    // The footage folders this job touches: named in the page itself (the
    // slot's variable values) or in the sub-compositions it mounts.
    let mut text = page.clone();
    for src in attribute_values(&page, "data-composition-src") {
        if let Ok(sub) = std::fs::read_to_string(workspace.join(&src)) {
            text.push_str(&sub);
        }
    }
    let footage = video_dirs(&text);

    link_tree(
        &workspace.join("compositions"),
        &root.join("compositions"),
        &|_| true,
    )?;
    let videos = workspace.join("assets/videos");
    link_tree(&workspace.join("assets"), &root.join("assets"), &|path| {
        match path.strip_prefix(&videos) {
            // A top-level folder under assets/videos is kept only when used.
            Ok(rel) => rel
                .components()
                .next()
                .map(|first| footage.iter().any(|d| first.as_os_str() == d.as_str()))
                .unwrap_or(true),
            Err(_) => true,
        }
    })?;
    let config = workspace.join("hyperframes.json");
    if config.is_file() {
        link_or_copy(&config, &root.join("hyperframes.json"))?;
    }
    Ok(root)
}

/// Every value of `attr="…"` in `html`.
fn attribute_values(html: &str, attr: &str) -> Vec<String> {
    let needle = format!("{attr}=\"");
    html.match_indices(&needle)
        .filter_map(|(at, _)| {
            let rest = &html[at + needle.len()..];
            rest.find('"').map(|end| rest[..end].to_string())
        })
        .collect()
}

/// The folder names that follow `assets/videos/` anywhere in `text`.
fn video_dirs(text: &str) -> Vec<String> {
    const PREFIX: &str = "assets/videos/";
    let mut dirs: Vec<String> = text
        .match_indices(PREFIX)
        .filter_map(|(at, _)| {
            let rest = &text[at + PREFIX.len()..];
            let end = rest.find(['/', '"', '\'', '\\', ' ']).unwrap_or(rest.len());
            // A bare file under assets/videos/ ends at a quote, not a slash —
            // that is not a folder.
            (rest[end..].starts_with('/') && end > 0).then(|| rest[..end].to_string())
        })
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Mirrors `from` into `to` with hard links, keeping only paths `keep` allows.
/// A missing `from` is nothing to mirror.
fn link_tree(from: &Path, to: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !keep(&path) {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_tree(&path, &target, keep)?;
        } else {
            link_or_copy(&path, &target)?;
        }
    }
    Ok(())
}

/// A hard link, or a copy where the filesystem will not link (another volume).
fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::compose::Kind;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-stage-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn job(id: &str, composition: &str) -> Job {
        Job {
            id: id.to_string(),
            kind: Kind::Body,
            composition: composition.to_string(),
            sources: Vec::new(),
        }
    }

    /// The shape that matters: the job page becomes index.html, its own
    /// chapter's footage comes along, and another chapter's does not.
    #[test]
    fn stage_keeps_only_the_footage_the_job_uses() {
        let root = temp("stage");
        let ws = root.join("horizontal");
        write(
            &ws.join("compositions/seg-02-body.html"),
            r#"<div data-composition-src="compositions/seg-02-body.outline.html"
                data-variable-values='{"cameraSrc":"assets/videos/chapter-02/cam.mp4"}'></div>"#,
        );
        write(
            &ws.join("compositions/seg-02-body.outline.html"),
            "<video src=\"cameraSrc\">",
        );
        write(&ws.join("assets/videos/chapter-02/cam.mp4"), "two");
        write(&ws.join("assets/videos/chapter-03/cam.mp4"), "three");
        write(&ws.join("assets/fonts/Booton.woff2"), "font");
        write(&ws.join("hyperframes.json"), "{}");
        write(&ws.join("index.html"), "blank");

        let staged = stage(&ws, &job("seg-02-body", "compositions/seg-02-body.html")).unwrap();

        assert_eq!(staged, root.join(".stage-horizontal/seg-02-body"));
        assert_eq!(
            std::fs::read_to_string(staged.join("index.html")).unwrap(),
            std::fs::read_to_string(ws.join("compositions/seg-02-body.html")).unwrap()
        );
        assert!(staged.join("assets/videos/chapter-02/cam.mp4").is_file());
        assert!(!staged.join("assets/videos/chapter-03").exists());
        assert!(staged.join("assets/fonts/Booton.woff2").is_file());
        assert!(staged
            .join("compositions/seg-02-body.outline.html")
            .is_file());
        assert!(staged.join("hyperframes.json").is_file());
    }

    /// A second stage of the same job starts clean rather than piling onto
    /// the first, so footage a job stopped using does not ride along.
    #[test]
    fn stage_rebuilds_from_scratch() {
        let root = temp("restage");
        let ws = root.join("vertical");
        write(
            &ws.join("compositions/chapter-01.html"),
            "assets/videos/chapter-01/v.mp4",
        );
        write(&ws.join("assets/videos/chapter-01/v.mp4"), "one");
        let j = job("chapter-01", "compositions/chapter-01.html");
        let staged = stage(&ws, &j).unwrap();
        write(&staged.join("stale.txt"), "left over");
        let again = stage(&ws, &j).unwrap();
        assert!(!again.join("stale.txt").exists());
    }

    #[test]
    fn video_dirs_are_folders_only() {
        let text = r#"assets/videos/chapter-02/a.mp4 "assets/videos/chapter-02/b.mp3"
                      assets/videos/loose.mp4" assets/videos/chapter-10/c.mp4"#;
        assert_eq!(video_dirs(text), vec!["chapter-02", "chapter-10"]);
    }
}
