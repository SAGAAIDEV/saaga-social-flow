//! Upload rendered videos (and transcripts) to public S3 for Buffer.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{bail, Context, Result};

use crate::session::Session;

pub mod progress;
mod s3;
pub(crate) use s3::screencast_home;
pub mod schema;

pub use progress::Progress;
pub use schema::{load, DistributeLinks};

pub enum DistributeEvent {
    Status(String),
    /// One repaint of the current file's upload bar, tagged with the asset id.
    Progress(String, Progress),
    Ready(PathBuf, DistributeLinks),
    /// Distinct from a `Status` carrying the same words: the app has to know the
    /// thread is gone so it can re-enable the button and hide the bar.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Long,
    Chapter,
    File,
    /// The chosen thumbnail. Uploads down the same path as `File` — the uploader
    /// only knows three kinds — but is labelled apart in `links.json`, because a
    /// consumer looking for a picture must not have to guess from the extension.
    Image,
}

#[derive(Debug, Clone)]
struct Asset {
    id: String,
    kind: AssetKind,
    path: PathBuf,
    content_type: &'static str,
    orientation: Option<String>,
    chapter: Option<u32>,
}

pub fn spawn_distribute(session: Session, tx: Sender<DistributeEvent>) {
    // Kept back from the closure so a thread that never starts still reports —
    // otherwise the app waits on an upload that does not exist.
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("distribute-s3".into())
        .spawn(move || match run(&session, &tx) {
            Ok((path, links)) => {
                eprintln!("stream-recorder: distribute ready → {}", path.display());
                let _ = tx.send(DistributeEvent::Ready(path, links));
            }
            Err(err) => {
                eprintln!("stream-recorder: distribute failed: {err:#}");
                let _ = tx.send(DistributeEvent::Failed(format!(
                    "Distribute failed: {err:#}"
                )));
            }
        })
    {
        eprintln!("stream-recorder: could not start distribute job: {err}");
        let _ = unstarted.send(DistributeEvent::Failed(format!(
            "Could not start the upload job: {err}"
        )));
    }
}

fn run(session: &Session, tx: &Sender<DistributeEvent>) -> Result<(PathBuf, DistributeLinks)> {
    let status = |msg: &str| {
        let _ = tx.send(DistributeEvent::Status(msg.to_string()));
    };
    let assets = collect_assets(&session.render_dir(), &session.dir, &session.root)?;
    if assets.is_empty() {
        bail!(
            "no rendered videos in {} — run Render first",
            session.render_dir().display()
        );
    }
    status(&format!("Uploading {} file(s) to S3…", assets.len()));
    // The folder, not the title: this becomes an S3 key prefix, and renaming a
    // project must never scatter its uploads across two of them.
    let project = session.folder();
    let version = session.version.unwrap_or(1);
    let dest = session.distribute_dir();
    let progress = |id: &str, update: Progress| {
        let _ = tx.send(DistributeEvent::Progress(id.to_string(), update));
    };
    let links = s3::upload_assets(&assets, &project, version, &dest, &status, &progress)?;
    Ok((dest.join(schema::LINKS_JSON), links))
}

/// `root` is the project folder rather than a stage: the thumbnail is chosen once
/// for the project, not per version, and that is where it lives.
fn collect_assets(render_dir: &Path, drafts: &Path, root: &Path) -> Result<Vec<Asset>> {
    let mut assets = Vec::new();
    let longform = render_dir.join("horizontal/longform.mp4");
    if longform.is_file() {
        assets.push(Asset {
            id: "longform".into(),
            kind: AssetKind::Long,
            path: longform,
            content_type: "video/mp4",
            orientation: Some("landscape".into()),
            chapter: None,
        });
        if let Some(text) = combined_transcript(drafts) {
            let txt = render_dir.join("horizontal/transcript.txt");
            if let Some(parent) = txt.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&txt, text).with_context(|| format!("writing {}", txt.display()))?;
            assets.push(Asset {
                id: "longform-transcript".into(),
                kind: AssetKind::File,
                path: txt,
                content_type: "text/plain; charset=utf-8",
                orientation: None,
                chapter: None,
            });
        }
    }
    for n in 1..=99 {
        let video = render_dir.join(format!("vertical/chapter-{n:02}.mp4"));
        if !video.is_file() {
            continue;
        }
        assets.push(Asset {
            id: format!("chapter-{n:02}"),
            kind: AssetKind::Chapter,
            path: video,
            content_type: "video/mp4",
            orientation: Some("portrait".into()),
            chapter: Some(n),
        });
        if let Some(text) = chapter_transcript(drafts, n) {
            let txt = render_dir.join(format!("vertical/chapter-{n:02}.txt"));
            std::fs::write(&txt, text).with_context(|| format!("writing {}", txt.display()))?;
            assets.push(Asset {
                id: format!("chapter-{n:02}-transcript"),
                kind: AssetKind::File,
                path: txt,
                content_type: "text/plain; charset=utf-8",
                orientation: None,
                chapter: Some(n),
            });
        }
    }
    // Last, and only alongside a video: a thumbnail is a cover for something, so
    // on its own it is not a distribution — and an empty list is what makes `run`
    // say "run Render first" instead of uploading a lone picture.
    if !assets.is_empty() {
        if root.join(crate::card::assets::MANIFEST).exists() {
            let set = crate::card::assets::ready(root)?;
            for (kind, id, orientation) in [
                (crate::card::assets::Kind::Horizontal, "thumbnail", "landscape"),
                (crate::card::assets::Kind::Vertical, "thumbnail-vertical", "portrait"),
                (crate::card::assets::Kind::Og, "og-image", "landscape"),
            ] {
                let source = set.path(root, kind)?;
                // The upload helper uses the basename as its object key.
                let export_dir = root.join("thumbnails/exports");
                std::fs::create_dir_all(&export_dir)?;
                let path = export_dir.join(format!("{}-{}.jpg", set.id, kind.name()));
                std::fs::copy(source, &path)?;
                assets.push(Asset { id: id.into(), kind: AssetKind::Image, path,
                    content_type: "image/jpeg", orientation: Some(orientation.into()), chapter: None });
            }
        } else if let Some(thumbnail) = chosen_thumbnail(root) {
            assets.push(thumbnail);
        }
    }
    Ok(assets)
}

/// The candidate the AI experiments last activated, if it is still on disk.
///
/// It keeps its content-addressed filename, so activating a different candidate
/// publishes a different URL rather than overwriting the old one — a post that
/// already went out must not have its cover changed underneath it. The stable
/// handle is the `thumbnail` id in `links.json`, which is what the planner reads.
fn chosen_thumbnail(root: &Path) -> Option<Asset> {
    let rows = crate::thumbnail::schema::load(root);
    let candidate = crate::thumbnail::schema::active(&rows)?;
    let path = candidate.path(root);
    if !path.is_file() {
        eprintln!(
            "stream-recorder: chosen thumbnail {} is missing from {}",
            candidate.id,
            path.display()
        );
        return None;
    }
    Some(Asset {
        id: "thumbnail".into(),
        kind: AssetKind::Image,
        path,
        content_type: "image/jpeg",
        orientation: Some("landscape".into()),
        chapter: None,
    })
}

fn chapter_transcript(drafts: &Path, n: u32) -> Option<String> {
    let path = drafts.join(format!("chapter-{n:02}.transcript.json"));
    let parsed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    parsed
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s}\n"))
}

fn combined_transcript(drafts: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for n in 1..=99 {
        if let Some(text) = chapter_transcript(drafts, n) {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-dist-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_finds_longform_and_chapters() {
        let root = temp("collect");
        let render = root.join("render");
        let drafts = root.join("drafts");
        std::fs::create_dir_all(render.join("horizontal")).unwrap();
        std::fs::create_dir_all(render.join("vertical")).unwrap();
        std::fs::create_dir_all(&drafts).unwrap();
        std::fs::write(render.join("horizontal/longform.mp4"), b"vid").unwrap();
        std::fs::write(render.join("vertical/chapter-01.mp4"), b"v1").unwrap();
        std::fs::write(
            drafts.join("chapter-01.transcript.json"),
            r#"{"status":"completed","text":"hello there"}"#,
        )
        .unwrap();
        let assets = collect_assets(&render, &drafts, &root).unwrap();
        let ids: Vec<_> = assets.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"longform"));
        assert!(ids.contains(&"longform-transcript"));
        assert!(ids.contains(&"chapter-01"));
        assert!(ids.contains(&"chapter-01-transcript"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_empty_without_renders() {
        let root = temp("empty");
        let assets =
            collect_assets(&root.join("render"), &root.join("drafts"), &root).unwrap();
        assert!(assets.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Writes a candidate and activates it, the way the AI experiments do.
    fn activated_thumbnail(root: &Path, id: &str) {
        let file = format!("thumbnails/candidates/{id}.jpg");
        let path = root.join(&file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"jpg").unwrap();
        let candidate = crate::thumbnail::schema::Candidate {
            id: id.into(),
            model: "google/gemini-3.1-flash-image".into(),
            file,
            created_at: "2026-08-15T19:16:34Z".into(),
            brief_hash: "briefhash".into(),
            still: "stillhash".into(),
            screen: None,
            refs: "refshash".into(),
        };
        crate::thumbnail::schema::append(
            root,
            &crate::thumbnail::schema::Row::Candidate(candidate),
        )
        .unwrap();
        crate::thumbnail::schema::append(
            root,
            &crate::thumbnail::schema::Row::Activated {
                id: id.into(),
                at: "2026-08-15T19:16:53Z".into(),
            },
        )
        .unwrap();
    }

    /// The gap this closes: the thumbnail was chosen, saved, and then never
    /// left the machine, so nothing downstream could put a cover on the video.
    #[test]
    fn the_chosen_thumbnail_ships_with_the_videos() {
        let root = temp("thumbnail");
        let render = root.join("render");
        std::fs::create_dir_all(render.join("horizontal")).unwrap();
        std::fs::write(render.join("horizontal/longform.mp4"), b"vid").unwrap();
        activated_thumbnail(&root, "thumb-abc123");

        let assets = collect_assets(&render, &root.join("drafts"), &root).unwrap();
        let thumb = assets
            .iter()
            .find(|a| a.id == "thumbnail")
            .expect("the activated thumbnail");
        assert_eq!(thumb.kind, AssetKind::Image);
        assert_eq!(thumb.content_type, "image/jpeg");
        assert!(thumb.path.ends_with("thumbnails/candidates/thumb-abc123.jpg"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A picture is a cover for something. With nothing rendered there is nothing
    /// to cover, and `run` has to keep saying "run Render first".
    #[test]
    fn a_thumbnail_alone_is_not_a_distribution() {
        let root = temp("thumbnail-only");
        activated_thumbnail(&root, "thumb-abc123");
        let assets =
            collect_assets(&root.join("render"), &root.join("drafts"), &root).unwrap();
        assert!(assets.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Candidates that were generated but never picked stay home.
    #[test]
    fn an_unchosen_candidate_is_not_uploaded() {
        let root = temp("thumbnail-unchosen");
        let render = root.join("render");
        std::fs::create_dir_all(render.join("horizontal")).unwrap();
        std::fs::write(render.join("horizontal/longform.mp4"), b"vid").unwrap();
        let path = root.join("thumbnails/candidates/thumb-nope.jpg");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"jpg").unwrap();

        let assets = collect_assets(&render, &root.join("drafts"), &root).unwrap();
        assert!(!assets.iter().any(|a| a.id == "thumbnail"));
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn all_artwork_formats_are_exported_with_immutable_names() {
        let root = temp("artwork-exports");
        let render = root.join("render");
        std::fs::create_dir_all(render.join("horizontal")).unwrap();
        std::fs::write(render.join("horizontal/longform.mp4"), b"video").unwrap();
        let set = crate::card::assets::fixture(&root);
        let assets = collect_assets(&render, &root.join("drafts"), &root).unwrap();
        for id in ["thumbnail", "thumbnail-vertical", "og-image"] {
            let asset = assets.iter().find(|asset| asset.id == id).unwrap();
            assert_eq!(asset.kind, AssetKind::Image);
            assert!(asset.path.file_name().unwrap().to_string_lossy().contains(&set.id));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

}
