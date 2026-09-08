//! Publishing the longform to YouTube directly, without Buffer.
//!
//! Separate from [`crate::schedule`] on purpose, and not only in code. Buffer is
//! a queue: it holds a post until the channel's next slot, it accepts one asset
//! and a little metadata, and it refuses a custom thumbnail outright. That suits
//! the short vertical cuts, which are many and want spacing out.
//!
//! The longform is one video that wants a title, a description, a category, a
//! privacy setting and a designed thumbnail, published when it is ready. All of
//! that is one upload against the YouTube Data API, and none of it fits through
//! a queue built for social posts — so this owns its own client, its own events
//! and its own ledger, and the Buffer plan skips the row.
//!
//! Tokens come from `auth-server`, which already owns every social OAuth token
//! and refreshes on read.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::session::Session;

pub mod metadata;
mod oauth;
mod token_store;
pub mod youtube;

/// Which YouTube channel the stored grant is bound to, if there is one.
///
/// Reads the token store and nothing else — no refresh, no round trip — because
/// this answers a question the YouTube tab asks on every refresh, and the tab
/// must not need the network to say "not connected yet".
///
/// Returns the channel *title* for the same reason the connect guard records it:
/// `UCX6g-NfcY2x-…` tells nobody they are about to publish to the wrong channel,
/// and "Andrew Melnychuk-Oseen" tells them immediately.
pub fn connected_channel() -> Option<String> {
    let token = token_store::load().ok().flatten()?;
    let title = token.channel_title.unwrap_or_else(|| "untitled".into());
    Some(match token.channel_id {
        Some(id) => format!("{title} ({id})"),
        None => title,
    })
}

/// Append-only, beside the schedule ledger and for the same reason: what went out
/// is the one fact a second press must not be free to contradict.
pub const UPLOADS_JSONL: &str = "youtube.jsonl";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upload {
    pub video_id: String,
    pub url: String,
    pub title: String,
    /// The render this came from, by content, so a re-render is a new upload and
    /// an unchanged one is recognised.
    pub source_hash: String,
    pub uploaded_at: String,
    /// Whether the chosen thumbnail made it on. Separate because the video can
    /// succeed and the thumbnail fail, and that is worth being able to retry.
    #[serde(default)]
    pub thumbnail_set: bool,
    /// What it went up as.
    ///
    /// Recorded rather than inferred from the current setting, because the
    /// setting is a standing preference that outlives any one upload: reading
    /// it back later would report what the *next* video would do, not what this
    /// one did. Defaults to public when absent, which is what every row written
    /// before this field existed actually was.
    #[serde(default)]
    pub privacy: youtube::Privacy,
}

pub enum PublishEvent {
    Status(String),
    Uploaded(Upload),
    /// The OAuth flow finished. Terminal like the other two — the app has to
    /// know the thread is gone before it re-enables the button.
    Connected,
    Failed(String),
}

pub fn spawn_upload(session: Session, tx: Sender<PublishEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("youtube-upload".into())
        .spawn(move || match run(&session, &tx) {
            Ok(upload) => {
                eprintln!("stream-recorder: youtube → {}", upload.url);
                let _ = tx.send(PublishEvent::Uploaded(upload));
            }
            Err(err) => {
                eprintln!("stream-recorder: youtube upload failed: {err:#}");
                let _ = tx.send(PublishEvent::Failed(format!(
                    "YouTube upload failed: {err:#}"
                )));
            }
        })
    {
        eprintln!("stream-recorder: could not start the youtube job: {err}");
        let _ = unstarted.send(PublishEvent::Failed(format!(
            "Could not start the YouTube job: {err}"
        )));
    }
}

fn run(session: &Session, tx: &Sender<PublishEvent>) -> Result<Upload> {
    let status = |msg: String| {
        let _ = tx.send(PublishEvent::Status(msg));
    };

    let video = session.render_dir().join("horizontal/longform.mp4");
    if !video.is_file() {
        bail!("no longform rendered yet — run Render first");
    }
    // Validate all critical artwork before publishing any video.
    let jpeg = chosen_thumbnail(session)
        .context("the artwork set is missing or stale — press Render video and thumbnails first")?;
    let source_hash = hash_of_file(&video)?;

    // The ledger, not the API, is what stops a double upload: YouTube will
    // happily accept the same video twice and give it two ids, and the second
    // one is a duplicate on the channel that nothing here would ever clean up.
    if let Some(mut prior) = uploaded(session, &source_hash) {
        status("Updating the thumbnail on the existing YouTube video…".into());
        youtube::set_thumbnail(&youtube::access_token()?, &prior.video_id, &jpeg)?;
        prior.thumbnail_set = true;
        append(session, &prior)?;
        return Ok(prior);
    }

    let meta = video_meta(session)?;
    status(format!("Uploading “{}” to YouTube…", meta.title));
    let token = youtube::access_token()?;
    let video_id = youtube::upload_video(&token, &video, &meta)?;
    let url = format!("https://www.youtube.com/watch?v={video_id}");

    // Recorded before the thumbnail: the video is up either way, and a row
    // written only on complete success would lose the id if this next call fails.
    let mut upload = Upload {
        video_id: video_id.clone(),
        url: url.clone(),
        title: meta.title.clone(),
        source_hash,
        uploaded_at: crate::schedule::ledger::now_rfc3339(),
        thumbnail_set: false,
        privacy: meta.privacy,
    };
    append(session, &upload)?;

    status("Setting the thumbnail…".into());
    // Refresh after the video transfer; use the image frozen before it began.
    youtube::access_token().and_then(|token| youtube::set_thumbnail(&token, &video_id, &jpeg))
        .with_context(|| format!("Video is uploaded at {url}, but its thumbnail failed. Press Upload again to retry the thumbnail"))?;
    upload.thumbnail_set = true;
    append(session, &upload)?;
    Ok(upload)
}

/// The upload owns its copy; social generation happens later in the workflow.
fn video_meta(session: &Session) -> Result<youtube::VideoMeta> {
    let metadata = metadata::load(session);
    metadata.validate()?;
    let config = crate::config::load();
    Ok(youtube::VideoMeta {
        title: metadata.title.trim().to_string(),
        description: metadata.description.trim().to_string(),
        category_id: config.youtube_category_id,
        privacy: config.youtube_privacy,
    })
}

fn chosen_thumbnail(session: &Session) -> Option<Vec<u8>> {
    let path =
        crate::card::assets::selected(&session.root, crate::card::assets::Kind::Horizontal).ok()?;
    std::fs::read(path).ok()
}

/// The most recent upload of this exact render, if there is one.
pub fn uploaded(session: &Session, source_hash: &str) -> Option<Upload> {
    load(session)
        .into_iter()
        .rev()
        .find(|row| row.source_hash == source_hash)
}

pub fn load(session: &Session) -> Vec<Upload> {
    let Ok(text) = std::fs::read_to_string(session.root.join(UPLOADS_JSONL)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn append(session: &Session, upload: &Upload) -> Result<()> {
    use std::io::Write;

    let path = session.root.join(UPLOADS_JSONL);
    let line = serde_json::to_string(upload).context("serializing the upload row")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

/// A render identified by its bytes, so "already uploaded" survives a rename and
/// notices a re-render.
fn hash_of_file(path: &PathBuf) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(crate::agent::prompt::hash_of_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(root: &std::path::Path) -> Session {
        Session {
            root: root.to_path_buf(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-publish-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn upload(hash: &str, id: &str) -> Upload {
        Upload {
            video_id: id.into(),
            url: format!("https://www.youtube.com/watch?v={id}"),
            title: "A video".into(),
            source_hash: hash.into(),
            uploaded_at: "2026-08-16T12:00:00Z".into(),
            thumbnail_set: false,
            privacy: youtube::Privacy::Public,
        }
    }

    /// A ledger written before visibility was recorded still loads, and reads
    /// as public — which is what those uploads actually were, since public was
    /// the only thing the constant it replaced could produce.
    #[test]
    fn a_row_from_before_this_field_reads_as_public() {
        let row: Upload = serde_json::from_str(
            r#"{"video_id":"abc","url":"https://y/abc","title":"A video",
                "source_hash":"h","uploaded_at":"2026-08-16T12:00:00Z"}"#,
        )
        .expect("an older row is still a valid one");
        assert_eq!(row.privacy, youtube::Privacy::Public);
        assert!(!row.thumbnail_set);
    }

    /// The guard that matters: YouTube will take the same video twice and give it
    /// two ids, and nothing here would ever clean the second one up.
    #[test]
    fn the_same_render_is_recognised_as_already_uploaded() {
        let root = temp("dupe");
        let session = session(&root);
        append(&session, &upload("aaaa", "vid-1")).unwrap();
        assert_eq!(uploaded(&session, "aaaa").unwrap().video_id, "vid-1");
        // A different render is different work.
        assert!(uploaded(&session, "bbbb").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The thumbnail is appended as a second row for the same upload, so the
    /// newest row wins and a retry does not read as a second video.
    #[test]
    fn a_thumbnail_retry_updates_rather_than_duplicates() {
        let root = temp("thumb");
        let session = session(&root);
        append(&session, &upload("aaaa", "vid-1")).unwrap();
        let mut done = upload("aaaa", "vid-1");
        done.thumbnail_set = true;
        append(&session, &done).unwrap();

        let found = uploaded(&session, "aaaa").unwrap();
        assert!(found.thumbnail_set);
        assert_eq!(load(&session).len(), 2, "the history is kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_ledger_reads_as_nothing_uploaded() {
        let root = temp("empty");
        assert!(load(&session(&root)).is_empty());
        assert!(uploaded(&session(&root), "aaaa").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A title over the limit is cut before the bytes go up, not rejected after.
    #[test]
    fn an_overlong_title_is_cut_to_what_youtube_accepts() {
        let meta = youtube::VideoMeta {
            title: "x".repeat(140),
            description: "d".into(),
            category_id: "28".into(),
            privacy: youtube::Privacy::Public,
        };
        let body = meta.body();
        assert_eq!(
            body["snippet"]["title"].as_str().unwrap().chars().count(),
            100
        );
        assert_eq!(body["status"]["privacyStatus"], "public");
        assert_eq!(body["snippet"]["categoryId"], "28");
    }
}
