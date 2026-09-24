//! Upload rendered videos (and transcripts) to public S3 for Buffer.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{bail, Context, Result};

use crate::session::Session;

mod s3;
pub mod schema;

pub use schema::{load, DistributeLinks};

/// One repaint of the upload bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub pct: f64,
    pub seen_mb: f64,
    pub total_mb: f64,
    pub label: String,
}

impl Progress {
    fn at(seen: u64, total: u64, label: &str) -> Self {
        let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        Self {
            pct: if total == 0 {
                100.0
            } else {
                (seen as f64 / total as f64 * 100.0).min(100.0)
            },
            seen_mb: mb(seen.min(total)),
            total_mb: mb(total),
            label: label.to_string(),
        }
    }
}

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

/// What the Buffer tab asks before Build Plan: is every video this render
/// produced on S3, as the file is now?
///
/// Decided from disk alone — no network, no hashing — so the tab can ask on
/// every repaint. `links.json` is written once, after the last object is up,
/// so a video file newer than it was rendered after the upload it records, and
/// a video with no row was never uploaded. The keys carry a content hash, so a
/// changed file really is at a different URL from the one on record: the plan
/// would hand Buffer the old cut.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub rows: Vec<CheckRow>,
    /// Where the record is, whether or not it exists yet.
    pub links: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub id: String,
    pub state: Hosting,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Hosting {
    /// On S3 as the file is now.
    Hosted(String),
    /// On S3, but the file was rendered again since — the URL is the old cut.
    Changed(String),
    /// Never uploaded.
    Missing,
}

impl Check {
    /// Every video is up and current. False for a project with no videos:
    /// nothing to post from is not "all posted".
    pub fn complete(&self) -> bool {
        !self.rows.is_empty()
            && self
                .rows
                .iter()
                .all(|row| matches!(row.state, Hosting::Hosted(_)))
    }

    pub fn hosted(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| matches!(row.state, Hosting::Hosted(_)))
            .count()
    }

    /// The one line the Buffer tab shows above the plan, and the reason the
    /// plan's gate gives while it is shut. Names the videos that are behind,
    /// and says which way each is behind, because the two have different
    /// causes: one upload never ran, the other ran before a re-render.
    pub fn summary(&self) -> String {
        if self.rows.is_empty() {
            return "No videos rendered yet — run Render first".to_string();
        }
        let total = self.rows.len();
        let behind: Vec<&CheckRow> = self
            .rows
            .iter()
            .filter(|row| !matches!(row.state, Hosting::Hosted(_)))
            .collect();
        if behind.is_empty() {
            return format!("All {total} video(s) on S3.");
        }
        if behind.len() == total && behind.iter().all(|row| row.state == Hosting::Missing) {
            return format!("None of the {total} video(s) are on S3 yet — press Upload to S3.");
        }
        let names = behind
            .iter()
            .map(|row| match row.state {
                Hosting::Changed(_) => format!("{} (rendered since the upload)", row.id),
                _ => format!("{} (never uploaded)", row.id),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} of {total} video(s) not on S3: {names} — press Upload to S3.",
            behind.len()
        )
    }
}

/// See [`Check`]. `targets` are the Render boxes, as for the upload itself, and
/// the posts on disk are read the same way the upload reads them, so the check
/// expects exactly the videos the upload would send.
pub fn check(session: &Session, targets: crate::config::RenderTargets) -> Check {
    let dir = session.distribute_dir();
    let links_path = dir.join(schema::LINKS_JSON);
    let links = schema::load(&dir).ok();
    let uploaded_at = std::fs::metadata(&links_path)
        .and_then(|meta| meta.modified())
        .ok();
    let wanted = wanted_videos(session);
    let rows = expected_videos(&session.render_dir(), targets, &wanted)
        .into_iter()
        .map(|video| {
            let state = match links.as_ref().and_then(|links| links.url_for(&video.id)) {
                None => Hosting::Missing,
                Some(url) => {
                    let rendered_at = std::fs::metadata(&video.path)
                        .and_then(|meta| meta.modified())
                        .ok();
                    match (rendered_at, uploaded_at) {
                        (Some(rendered), Some(uploaded)) if rendered > uploaded => {
                            Hosting::Changed(url.to_string())
                        }
                        _ => Hosting::Hosted(url.to_string()),
                    }
                }
            };
            CheckRow {
                id: video.id,
                state,
            }
        })
        .collect();
    Check {
        rows,
        links: links_path,
    }
}

/// One video a render left that Buffer posts, by the id `links.json` records
/// it under.
struct Video {
    id: String,
    path: PathBuf,
    /// `None` for the longform.
    chapter: Option<u32>,
}

/// The videos the posts on disk are written for, by id. Empty when there are
/// no posts yet, which costs nothing: the boxes still decide.
fn wanted_videos(session: &Session) -> Vec<String> {
    crate::posts::load_manifest(&session.posts_dir())
        .map(|manifest| {
            manifest
                .items
                .into_iter()
                .map(|video| video.video_id)
                .collect()
        })
        .unwrap_or_default()
}

/// The longform and the chapter shorts on disk, in upload order. Side-effect
/// free, unlike [`collect_assets`], which also writes the transcripts and copies
/// the artwork beside them — so this is what a repaint may ask, and what the
/// upload builds on so the two can never disagree about which videos exist.
///
/// A chapter goes when the shorts box is on — or when the posts name it. The
/// vertical longform is the chapters joined, so with that box on and the
/// shorts box off the chapter files are rendered all the same, and the posts
/// stage writes copy for every one of them; the upload used to leave them out
/// on the strength of the box, and every one of those posts then planned as
/// "no distributed url". Posts written for a video are the clearest statement
/// that it is meant to go out, so they outrank the box here.
fn expected_videos(
    render_dir: &Path,
    targets: crate::config::RenderTargets,
    wanted: &[String],
) -> Vec<Video> {
    let mut out = Vec::new();
    let longform = render_dir.join("horizontal/longform.mp4");
    if targets.horizontal && longform.is_file() {
        out.push(Video {
            id: "longform".into(),
            path: longform,
            chapter: None,
        });
    }
    for n in 1..=99 {
        let id = format!("chapter-{n:02}");
        if !targets.shorts && !wanted.contains(&id) {
            continue;
        }
        let video = render_dir.join(format!("vertical/chapter-{n:02}.mp4"));
        if video.is_file() {
            out.push(Video {
                id,
                path: video,
                chapter: Some(n),
            });
        }
    }
    out
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
    let targets = crate::config::render_targets(session);
    let wanted = wanted_videos(session);
    let assets = collect_assets(
        &session.render_dir(),
        &session.dir,
        &session.root,
        targets,
        &wanted,
    )?;
    if assets.is_empty() {
        if !targets.horizontal && !targets.shorts {
            bail!(
                "the horizontal longform and the chapter clips are both switched off above the Render \
                 button — nothing to upload"
            );
        }
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
///
/// `targets` are the Render boxes and `wanted` the videos the posts name — see
/// [`expected_videos`] for how the two decide which chapters go.
fn collect_assets(
    render_dir: &Path,
    drafts: &Path,
    root: &Path,
    targets: crate::config::RenderTargets,
    wanted: &[String],
) -> Result<Vec<Asset>> {
    let mut assets = Vec::new();
    for video in expected_videos(render_dir, targets, wanted) {
        match video.chapter {
            None => {
                assets.push(Asset {
                    id: video.id,
                    kind: AssetKind::Long,
                    path: video.path,
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
                    std::fs::write(&txt, text)
                        .with_context(|| format!("writing {}", txt.display()))?;
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
            Some(n) => {
                assets.push(Asset {
                    id: video.id,
                    kind: AssetKind::Chapter,
                    path: video.path,
                    content_type: "video/mp4",
                    orientation: Some("portrait".into()),
                    chapter: Some(n),
                });
                if let Some(text) = chapter_transcript(drafts, n) {
                    let txt = render_dir.join(format!("vertical/chapter-{n:02}.txt"));
                    std::fs::write(&txt, text)
                        .with_context(|| format!("writing {}", txt.display()))?;
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
        }
    }
    // Last, and only alongside a video: a thumbnail is a cover for something, so
    // on its own it is not a distribution — and an empty list is what makes `run`
    // say "run Render first" instead of uploading a lone picture.
    if !assets.is_empty() {
        if root.join(crate::card::assets::MANIFEST).exists() {
            let set = crate::card::assets::ready(root)?;
            for (kind, id, orientation) in [
                (
                    crate::card::assets::Kind::Horizontal,
                    "thumbnail",
                    "landscape",
                ),
                (
                    crate::card::assets::Kind::Vertical,
                    "thumbnail-vertical",
                    "portrait",
                ),
                (crate::card::assets::Kind::Og, "og-image", "landscape"),
            ] {
                let source = set.path(root, kind)?;
                // The upload helper uses the basename as its object key.
                let export_dir = root.join("thumbnails/exports");
                std::fs::create_dir_all(&export_dir)?;
                let path = export_dir.join(format!("{}-{}.jpg", set.id, kind.name()));
                std::fs::copy(source, &path)?;
                assets.push(Asset {
                    id: id.into(),
                    kind: AssetKind::Image,
                    path,
                    content_type: "image/jpeg",
                    orientation: Some(orientation.into()),
                    chapter: None,
                });
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
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
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

    /// The Render boxes decide what ships, not what is on disk: a render made
    /// with the shorts on and the box unticked since must not send them.
    #[test]
    fn switched_off_outputs_are_not_collected_even_when_their_files_exist() {
        use crate::config::RenderTargets;
        let root = temp("targets");
        let render = root.join("render");
        let drafts = root.join("drafts");
        std::fs::create_dir_all(render.join("horizontal")).unwrap();
        std::fs::create_dir_all(render.join("vertical")).unwrap();
        std::fs::create_dir_all(&drafts).unwrap();
        std::fs::write(render.join("horizontal/longform.mp4"), b"vid").unwrap();
        std::fs::write(render.join("vertical/chapter-01.mp4"), b"v1").unwrap();

        let no_shorts = RenderTargets {
            horizontal: true,
            vertical: true,
            shorts: false,
            cloud: false,
        };
        let ids: Vec<String> = collect_assets(&render, &drafts, &root, no_shorts, &[])
            .unwrap()
            .into_iter()
            .map(|asset| asset.id)
            .collect();
        assert!(ids.iter().any(|id| id == "longform"), "{ids:?}");
        assert!(!ids.iter().any(|id| id.starts_with("chapter-")), "{ids:?}");

        let no_longform = RenderTargets {
            horizontal: false,
            vertical: true,
            shorts: true,
            cloud: false,
        };
        let ids: Vec<String> = collect_assets(&render, &drafts, &root, no_longform, &[])
            .unwrap()
            .into_iter()
            .map(|asset| asset.id)
            .collect();
        assert!(!ids.iter().any(|id| id == "longform"), "{ids:?}");
        assert!(ids.iter().any(|id| id == "chapter-01"), "{ids:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
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
        let assets = collect_assets(&render, &drafts, &root, Default::default(), &[]).unwrap();
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
        let assets = collect_assets(
            &root.join("render"),
            &root.join("drafts"),
            &root,
            Default::default(),
            &[],
        )
        .unwrap();
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

        let assets = collect_assets(
            &render,
            &root.join("drafts"),
            &root,
            Default::default(),
            &[],
        )
        .unwrap();
        let thumb = assets
            .iter()
            .find(|a| a.id == "thumbnail")
            .expect("the activated thumbnail");
        assert_eq!(thumb.kind, AssetKind::Image);
        assert_eq!(thumb.content_type, "image/jpeg");
        assert!(thumb
            .path
            .ends_with("thumbnails/candidates/thumb-abc123.jpg"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A picture is a cover for something. With nothing rendered there is nothing
    /// to cover, and `run` has to keep saying "run Render first".
    #[test]
    fn a_thumbnail_alone_is_not_a_distribution() {
        let root = temp("thumbnail-only");
        activated_thumbnail(&root, "thumb-abc123");
        let assets = collect_assets(
            &root.join("render"),
            &root.join("drafts"),
            &root,
            Default::default(),
            &[],
        )
        .unwrap();
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

        let assets = collect_assets(
            &render,
            &root.join("drafts"),
            &root,
            Default::default(),
            &[],
        )
        .unwrap();
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
        let assets = collect_assets(
            &render,
            &root.join("drafts"),
            &root,
            Default::default(),
            &[],
        )
        .unwrap();
        for id in ["thumbnail", "thumbnail-vertical", "og-image"] {
            let asset = assets.iter().find(|asset| asset.id == id).unwrap();
            assert_eq!(asset.kind, AssetKind::Image);
            assert!(asset
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&set.id));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod check_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn session(tag: &str) -> Session {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-distribute-check-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("render/horizontal")).unwrap();
        std::fs::create_dir_all(root.join("render/vertical")).unwrap();
        Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    fn all() -> crate::config::RenderTargets {
        crate::config::RenderTargets {
            horizontal: true,
            vertical: true,
            shorts: true,
            cloud: false,
        }
    }

    fn touched(path: &Path, when: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn record(session: &Session, ids: &[&str]) -> PathBuf {
        let links = DistributeLinks {
            project: "p".into(),
            version: 1,
            items: ids
                .iter()
                .map(|id| schema::DistributedAsset {
                    id: id.to_string(),
                    kind: "video".into(),
                    orientation: None,
                    chapter: None,
                    url: format!("https://cdn/{id}-abcd1234.mp4"),
                    file: None,
                })
                .collect(),
        };
        schema::save(&session.distribute_dir(), &links).unwrap()
    }

    /// Nothing recorded: every video is missing, and the line says to press
    /// the button rather than listing them one by one.
    #[test]
    fn with_no_record_every_video_is_missing() {
        let session = session("missing");
        std::fs::write(session.render_dir().join("horizontal/longform.mp4"), b"v").unwrap();
        std::fs::write(session.render_dir().join("vertical/chapter-01.mp4"), b"v").unwrap();
        let got = check(&session, all());
        assert_eq!(got.rows.len(), 2);
        assert!(got.rows.iter().all(|row| row.state == Hosting::Missing));
        assert!(!got.complete());
        assert_eq!(got.hosted(), 0);
        assert_eq!(
            got.summary(),
            "None of the 2 video(s) are on S3 yet — press Upload to S3."
        );
        assert!(
            got.links.ends_with("distribute/links.json"),
            "{:?}",
            got.links
        );
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// A record newer than every video is the good state; a video rendered
    /// after it is behind, by name and for the right reason, and so is one the
    /// record never saw.
    #[test]
    fn the_record_is_measured_against_each_video_s_own_time() {
        let session = session("stale");
        let now = SystemTime::now();
        let longform = session.render_dir().join("horizontal/longform.mp4");
        let first = session.render_dir().join("vertical/chapter-01.mp4");
        let second = session.render_dir().join("vertical/chapter-02.mp4");
        for path in [&longform, &first, &second] {
            std::fs::write(path, b"v").unwrap();
            touched(path, now - Duration::from_secs(600));
        }
        let recorded = record(&session, &["longform", "chapter-01"]);
        touched(&recorded, now - Duration::from_secs(300));

        let got = check(&session, all());
        assert!(matches!(got.rows[0].state, Hosting::Hosted(ref url) if url.contains("longform")));
        assert_eq!(got.rows[2].state, Hosting::Missing);
        assert_eq!(got.hosted(), 2);
        assert!(got
            .summary()
            .starts_with("1 of 3 video(s) not on S3: chapter-02 (never uploaded)"));

        // Chapter one is cut again after the upload: same id, different bytes,
        // and a URL on record that now points at the old cut.
        touched(&first, now);
        let got = check(&session, all());
        assert!(matches!(got.rows[1].state, Hosting::Changed(_)));
        let summary = got.summary();
        assert!(
            summary.starts_with("2 of 3 video(s) not on S3:"),
            "{summary}"
        );
        assert!(
            summary.contains("chapter-01 (rendered since the upload)"),
            "{summary}"
        );
        assert!(summary.contains("chapter-02 (never uploaded)"), "{summary}");
        assert!(summary.ends_with("press Upload to S3."), "{summary}");

        // Everything up: one line, no names.
        record(&session, &["longform", "chapter-01", "chapter-02"]);
        let got = check(&session, all());
        assert!(got.complete(), "{got:?}");
        assert_eq!(got.summary(), "All 3 video(s) on S3.");
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// The case from the field: the vertical longform box on, the shorts box
    /// off, three chapters rendered as its parts and posts written for each —
    /// and the upload leaving them all behind, so every chapter post planned
    /// as "no distributed url". A chapter the posts name is expected on S3
    /// whatever the box says; one nothing names still follows the box.
    #[test]
    fn a_chapter_the_posts_name_is_expected_even_with_the_shorts_box_off() {
        let session = session("wanted");
        std::fs::write(session.render_dir().join("horizontal/longform.mp4"), b"v").unwrap();
        std::fs::write(session.render_dir().join("vertical/chapter-01.mp4"), b"v").unwrap();
        std::fs::write(session.render_dir().join("vertical/chapter-02.mp4"), b"v").unwrap();
        crate::posts::schema::save_manifest(
            &session.posts_dir(),
            &crate::posts::schema::PostsManifest {
                version: None,
                prompt_version: None,
                prompt_hash: String::new(),
                items: vec![crate::posts::schema::VideoPosts {
                    video_id: "chapter-01".into(),
                    video_type: "vertical".into(),
                    video_path: None,
                    posts: vec![crate::posts::schema::PlatformPost {
                        platform: "tiktok".into(),
                        title: None,
                        content: "Watch this.".into(),
                        tags: Vec::new(),
                    }],
                }],
            },
        )
        .unwrap();
        let no_shorts = crate::config::RenderTargets {
            shorts: false,
            ..all()
        };
        let got = check(&session, no_shorts);
        let ids: Vec<_> = got.rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["longform", "chapter-01"]);
        assert_eq!(got.rows[1].state, Hosting::Missing);
        assert!(
            got.summary().contains("press Upload to S3"),
            "{}",
            got.summary()
        );
        // And the upload sends the same list, so the plan can never be told
        // "no url" for a video the check called complete.
        let assets = collect_assets(
            &session.render_dir(),
            &session.dir,
            &session.root,
            no_shorts,
            &["chapter-01".to_string()],
        )
        .unwrap();
        let videos: Vec<_> = assets
            .iter()
            .filter(|asset| matches!(asset.kind, AssetKind::Long | AssetKind::Chapter))
            .map(|asset| asset.id.as_str())
            .collect();
        assert_eq!(videos, vec!["longform", "chapter-01"]);
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// A video whose Render box is off is not expected on S3, and a project
    /// with nothing rendered is not "complete" — there is nothing to post.
    #[test]
    fn the_boxes_decide_which_videos_are_expected() {
        let session = session("targets");
        std::fs::write(session.render_dir().join("horizontal/longform.mp4"), b"v").unwrap();
        std::fs::write(session.render_dir().join("vertical/chapter-01.mp4"), b"v").unwrap();
        let no_shorts = crate::config::RenderTargets {
            shorts: false,
            ..all()
        };
        let ids: Vec<_> = check(&session, no_shorts)
            .rows
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, vec!["longform"]);
        let none = crate::config::RenderTargets {
            horizontal: false,
            vertical: false,
            shorts: false,
            cloud: false,
        };
        let got = check(&session, none);
        assert!(got.rows.is_empty());
        assert!(!got.complete());
        assert_eq!(got.summary(), "No videos rendered yet — run Render first");
        let _ = std::fs::remove_dir_all(&session.root);
    }
}
