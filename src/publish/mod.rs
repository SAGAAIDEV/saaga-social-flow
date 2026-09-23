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

use std::path::Path;
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

/// The longest video YouTube will file as a Short: three minutes, since
/// October 2024 (it was sixty seconds before that).
///
/// A portrait upload past this is not rejected — it goes up as an ordinary
/// video. That is the trap: the `/shorts/` URL recorded here would be a lie,
/// the blog would embed it as the page's mobile player on the strength of that
/// row, and the Shorts feed would never show it. So the vertical longform is
/// measured first and skipped when it is over, rather than uploaded and
/// mislabelled. The chapter shorts through Buffer are unaffected; a chapter
/// is nowhere near this long.
pub const SHORT_MAX_SECONDS: f64 = 180.0;

/// Why this vertical cut is not going up as a Short, or `None` when it is.
///
/// Reads the file's length with `ffprobe`. A file whose length cannot be read
/// is skipped too, with the reason: "make sure it is a Short" has to fail
/// closed, and an unreadable file is not one the upload should be guessing at.
pub fn short_block(video: &Path) -> Option<String> {
    match crate::edit::cut::probe_duration_seconds(video) {
        Ok(seconds) => short_length_block(seconds),
        Err(err) => Some(format!(
            "could not read the vertical longform's length ({err:#}) — not uploading it as a Short"
        )),
    }
}

/// The pure half of [`short_block`], for the length alone.
pub fn short_length_block(seconds: f64) -> Option<String> {
    (seconds > SHORT_MAX_SECONDS).then(|| {
        format!(
            "the vertical longform is {} long, over the {} YouTube allows a Short — not uploading \
             it. It stays available for the blog's mobile player",
            mmss(seconds),
            mmss(SHORT_MAX_SECONDS)
        )
    })
}

/// `245.3` → `4:05`. Rounded, because a Short limit is read to the second.
fn mmss(seconds: f64) -> String {
    let whole = seconds.round().max(0.0) as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

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
    /// When the thumbnail last went up, whether with the upload or replaced
    /// later from the tab. `None` on rows from before this was recorded, and
    /// on rows whose thumbnail never set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_at: Option<String>,
    /// Who can see it on YouTube now.
    ///
    /// Recorded rather than inferred from the current setting, because the
    /// setting is a standing preference that outlives any one upload: reading
    /// it back later would report what the *next* video would do, not what this
    /// one did. Defaults to public when absent, which is what every row written
    /// before this field existed actually was. Changed after the upload by
    /// [`run_visibility`], which appends a row with the new value and a
    /// [`Upload::privacy_at`].
    #[serde(default)]
    pub privacy: youtube::Privacy,
    /// When the visibility was last changed on YouTube from the tab. `None` on
    /// a video still at the visibility it went up with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_at: Option<String>,
    /// Which cut this is. Defaults to the longform, which is what every row
    /// written before the Short existed was.
    #[serde(default)]
    pub orientation: Orientation,
}

/// Which cut a ledger row is: the landscape longform, or the vertical edit of
/// the same chapters, published as a Short.
///
/// Its own type rather than [`crate::layouts::Orientation`] because this one
/// is written to disk: a serde derive on the layout enum would make every
/// rename there a wire-format change here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

impl Orientation {
    fn noun(self) -> &'static str {
        match self {
            Orientation::Horizontal => "the longform",
            Orientation::Vertical => "a Short",
        }
    }
}

pub enum PublishEvent {
    Status(String),
    /// One video landed. Not terminal: a press puts up the longform and then
    /// the vertical cut when the project has one, and the button stays off
    /// until [`PublishEvent::Done`].
    Uploaded(Upload),
    /// The poster on a video already up was replaced with the selected artwork.
    /// Not terminal either: the Short's follows the longform's.
    ThumbnailSet(Upload),
    /// Who can see a video already up was changed to match the picker. Not
    /// terminal either, for the same reason.
    VisibilitySet(Upload),
    /// The press is over, however many videos it put up.
    Done,
    /// The OAuth flow finished. Terminal like `Done` and `Failed` — the app has
    /// to know the thread is gone before it re-enables the button.
    Connected,
    Failed(String),
}

pub fn spawn_upload(session: Session, tx: Sender<PublishEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("youtube-upload".into())
        .spawn(move || match run(&session, &tx) {
            Ok(()) => {
                let _ = tx.send(PublishEvent::Done);
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

/// Replaces the poster on what is already up, from the tab, without touching
/// the video. See [`Action::YoutubeThumbnail`](crate::hotkeys::Action).
pub fn spawn_thumbnail(session: Session, tx: Sender<PublishEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("youtube-thumbnail".into())
        .spawn(move || match run_thumbnail(&session, &tx) {
            Ok(()) => {
                let _ = tx.send(PublishEvent::Done);
            }
            Err(err) => {
                eprintln!("stream-recorder: youtube thumbnail failed: {err:#}");
                let _ = tx.send(PublishEvent::Failed(format!(
                    "Replacing the thumbnail failed: {err:#}"
                )));
            }
        })
    {
        eprintln!("stream-recorder: could not start the youtube thumbnail job: {err}");
        let _ = unstarted.send(PublishEvent::Failed(format!(
            "Could not start the thumbnail job: {err}"
        )));
    }
}

/// Changes who can see what is already up, from the Visibility picker, without
/// touching the video. See [`run_visibility`].
pub fn spawn_visibility(session: Session, privacy: youtube::Privacy, tx: Sender<PublishEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("youtube-visibility".into())
        .spawn(move || match run_visibility(&session, privacy, &tx) {
            Ok(()) => {
                let _ = tx.send(PublishEvent::Done);
            }
            Err(err) => {
                eprintln!("stream-recorder: youtube visibility change failed: {err:#}");
                let _ = tx.send(PublishEvent::Failed(format!(
                    "Changing the visibility failed: {err:#}"
                )));
            }
        })
    {
        eprintln!("stream-recorder: could not start the youtube visibility job: {err}");
        let _ = unstarted.send(PublishEvent::Failed(format!(
            "Could not start the visibility job: {err}"
        )));
    }
}

/// The picker's visibility onto the newest longform on YouTube, then onto the
/// Short if there is one.
///
/// The ledger row names the video, as it does for the poster: the video people
/// are already watching is the one whose visibility changed, whatever the
/// render on disk now hashes to. A video already at the picker's setting is
/// left alone — the API call is skipped, and so is the ledger row, so pressing
/// the same choice twice writes nothing.
///
/// The longform is the reason the picker exists, so its failure is the job's
/// failure. The Short's is best effort, as everywhere else: it is a companion
/// to the longform, and a Short left at the old visibility is reported rather
/// than allowed to undo a longform that changed.
fn run_visibility(
    session: &Session,
    privacy: youtube::Privacy,
    tx: &Sender<PublishEvent>,
) -> Result<()> {
    let status = |msg: String| {
        let _ = tx.send(PublishEvent::Status(msg));
    };
    let longform = longform(session).context(
        "nothing on YouTube yet from this project — the visibility applies to the next upload",
    )?;
    if longform.privacy == privacy {
        status(format!(
            "The longform is already {} on YouTube.",
            privacy.label()
        ));
    } else {
        status(format!(
            "Making the longform {} on YouTube…",
            privacy.label()
        ));
        let token = youtube::access_token()?;
        youtube::update_privacy(&token, &longform.video_id, privacy).with_context(|| {
            format!(
                "{} is still {} on YouTube",
                longform.url,
                longform.privacy.label()
            )
        })?;
        let row = record_privacy(session, longform, privacy)?;
        eprintln!(
            "stream-recorder: youtube visibility → {} on {}",
            privacy.as_str(),
            row.url
        );
        let _ = tx.send(PublishEvent::VisibilitySet(row));
    }

    let Some(short) = short(session) else {
        return Ok(());
    };
    if short.privacy == privacy {
        return Ok(());
    }
    status(format!("Making the Short {} on YouTube…", privacy.label()));
    match youtube::access_token()
        .and_then(|token| youtube::update_privacy(&token, &short.video_id, privacy))
    {
        Ok(()) => {
            let row = record_privacy(session, short, privacy)?;
            let _ = tx.send(PublishEvent::VisibilitySet(row));
        }
        Err(err) => {
            eprintln!("stream-recorder: the Short's visibility did not change: {err:#}");
            status(format!(
                "The Short is still {} on YouTube: {err:#}",
                short.privacy.label()
            ));
        }
    }
    Ok(())
}

/// The row that says who can see the video now, and since when.
fn record_privacy(session: &Session, mut row: Upload, privacy: youtube::Privacy) -> Result<Upload> {
    row.privacy = privacy;
    row.privacy_at = Some(crate::schedule::ledger::now_rfc3339());
    append(session, &row)?;
    Ok(row)
}

/// The selected horizontal artwork onto the newest longform on YouTube, then
/// the selected vertical artwork onto the Short if there is one.
///
/// The ledger row, not the render on disk, names the video: the point of this
/// press is that the artwork changed after the upload, and the render may well
/// have too. A re-rendered longform is a new upload's business; the thumbnail on
/// the video people are already watching is this one's.
///
/// The longform's poster is the reason the button exists, so its failure is the
/// press's failure. The Short's is best effort, as on upload: the Shorts feed
/// shows a frame of the video whatever poster is set.
fn run_thumbnail(session: &Session, tx: &Sender<PublishEvent>) -> Result<()> {
    let status = |msg: String| {
        let _ = tx.send(PublishEvent::Status(msg));
    };
    let longform = longform(session)
        .context("nothing on YouTube yet from this project — press Upload to YouTube first")?;
    let jpeg = chosen_thumbnail(session, crate::card::assets::Kind::Horizontal)
        .context("no horizontal artwork selected — pick a thumbnail on the Thumbnails tab first")?;
    status(format!("Replacing the thumbnail on {}…", longform.url));
    let token = youtube::access_token()?;
    youtube::set_thumbnail(&token, &longform.video_id, &jpeg).with_context(|| {
        format!(
            "the thumbnail on {} was not replaced — it still shows the previous one",
            longform.url
        )
    })?;
    let row = record_poster(session, longform)?;
    eprintln!("stream-recorder: youtube thumbnail replaced on {}", row.url);
    let _ = tx.send(PublishEvent::ThumbnailSet(row));

    let Some(short) = short(session) else {
        return Ok(());
    };
    let Some(poster) = chosen_thumbnail(session, crate::card::assets::Kind::Vertical) else {
        status("The Short keeps its poster: no vertical artwork is selected.".into());
        return Ok(());
    };
    status(format!(
        "Replacing the poster on the Short at {}…",
        short.url
    ));
    match youtube::access_token()
        .and_then(|token| youtube::set_thumbnail(&token, &short.video_id, &poster))
    {
        Ok(()) => {
            let row = record_poster(session, short)?;
            let _ = tx.send(PublishEvent::ThumbnailSet(row));
        }
        Err(err) => {
            eprintln!("stream-recorder: the Short's poster did not replace: {err:#}");
            status(format!("The Short's poster did not replace: {err:#}"));
        }
    }
    Ok(())
}

/// The row that says the poster is on, and since when.
fn record_poster(session: &Session, mut row: Upload) -> Result<Upload> {
    row.thumbnail_set = true;
    row.thumbnail_at = Some(crate::schedule::ledger::now_rfc3339());
    append(session, &row)?;
    Ok(row)
}

/// A short's upload: its vertical cut as the Short, and nothing else — it has
/// no longform, and it was recorded to be exactly this one video.
///
/// Over the Shorts limit is a failure here rather than the skip it is for a
/// video's vertical longform: there, the longform is the press's job and is
/// already live; here, the Short *is* the job, and an upload that YouTube files
/// as an ordinary video would be the wrong thing done quietly.
fn run_short(session: &Session, tx: &Sender<PublishEvent>) -> Result<()> {
    let status = |msg: String| {
        let _ = tx.send(PublishEvent::Status(msg));
    };
    let vertical = session.render_dir().join("vertical/longform.mp4");
    if !vertical.is_file() {
        bail!("the short is not rendered yet — run Render first");
    }
    if let Some(seconds) = crate::edit::cut::probe_duration_seconds(&vertical)
        .ok()
        .filter(|&seconds| seconds > SHORT_MAX_SECONDS)
    {
        bail!(
            "the short is {} long, over the {} YouTube allows a Short — trim it on the Edit tab \
             or retake it, then Render",
            mmss(seconds),
            mmss(SHORT_MAX_SECONDS)
        );
    }
    let poster = chosen_thumbnail(session, crate::card::assets::Kind::Vertical);
    let short = upload_one(
        session,
        &vertical,
        Orientation::Vertical,
        poster.as_deref(),
        &status,
    )?;
    eprintln!("stream-recorder: youtube short → {}", short.url);
    let _ = tx.send(PublishEvent::Uploaded(short));
    Ok(())
}

/// The longform first, then the vertical cut as a Short when the project has
/// one. Each lands as its own row and its own event, so a Short that fails
/// leaves the longform live and reported rather than rolled into one failure.
fn run(session: &Session, tx: &Sender<PublishEvent>) -> Result<()> {
    let status = |msg: String| {
        let _ = tx.send(PublishEvent::Status(msg));
    };

    // The Render boxes above the Render button. An output that is switched
    // off is not uploaded even when an earlier render left its file behind:
    // the box is the decision, the file is history.
    if session.is_short() {
        return run_short(session, tx);
    }
    let targets = crate::config::render_targets(session);
    if !targets.horizontal {
        bail!(
            "the horizontal longform is switched off above the Render button — tick it and Render \
             before uploading"
        );
    }
    let video = session.render_dir().join("horizontal/longform.mp4");
    if !video.is_file() {
        bail!("no longform rendered yet — run Render first");
    }
    // Validate all critical artwork before publishing any video.
    let jpeg = chosen_thumbnail(session, crate::card::assets::Kind::Horizontal)
        .context("the artwork set is missing or stale — press Render video and thumbnails first")?;
    let longform = upload_one(
        session,
        &video,
        Orientation::Horizontal,
        Some(&jpeg),
        &status,
    )?;
    eprintln!("stream-recorder: youtube → {}", longform.url);
    let _ = tx.send(PublishEvent::Uploaded(longform.clone()));

    // The same chapters cut portrait. YouTube files it as a Short on its own —
    // portrait, under the length limit — and the blog embeds it as the page's
    // mobile player rather than hosting the file itself.
    let vertical = session.render_dir().join("vertical/longform.mp4");
    if vertical.is_file() && !targets.vertical {
        status(
            "The vertical longform is switched off above the Render button — not uploading it as a \
             Short."
                .into(),
        );
    } else if let Some(why) = vertical.is_file().then(|| short_block(&vertical)).flatten() {
        // Over the Shorts limit, or unmeasurable. A skip, not a failure: the
        // longform is live and that is the press's job; this is the one video
        // that must not go up under the wrong name.
        eprintln!("stream-recorder: {why}");
        status(format!("{}.", capitalize(&why)));
    } else if vertical.is_file() {
        let poster = chosen_thumbnail(session, crate::card::assets::Kind::Vertical);
        let short = upload_one(
            session,
            &vertical,
            Orientation::Vertical,
            poster.as_deref(),
            &status,
        )
        .with_context(|| {
            format!(
                "the longform is live at {}, but the Short did not go up",
                longform.url
            )
        })?;
        eprintln!("stream-recorder: youtube short → {}", short.url);
        let _ = tx.send(PublishEvent::Uploaded(short));
    }
    Ok(())
}

/// One video up, or its existing row refreshed.
///
/// The ledger, not the API, is what stops a double upload: YouTube will happily
/// accept the same video twice and give it two ids, and the second one is a
/// duplicate on the channel that nothing here would ever clean up. A render
/// already up gets its poster set again and its visibility brought in line
/// with the picker, and nothing else.
fn upload_one(
    session: &Session,
    video: &Path,
    orientation: Orientation,
    poster: Option<&[u8]>,
    status: &dyn Fn(String),
) -> Result<Upload> {
    let source_hash = hash_of_file(video)?;
    let meta = video_meta(session, orientation)?;
    if let Some(mut prior) = uploaded(session, &source_hash) {
        if let Some(jpeg) = poster {
            status(format!(
                "Updating the thumbnail on the existing YouTube video for {}…",
                orientation.noun()
            ));
            if set_poster(orientation, &prior.video_id, &prior.url, jpeg)? {
                prior = record_poster(session, prior)?;
            }
        }
        // The picker may have moved since this went up — while a job was
        // running, say, when the change is held for the next press. The
        // picker is the truth for what is up, so a press brings it in line.
        if prior.privacy != meta.privacy {
            status(format!(
                "Making {} {} on YouTube…",
                orientation.noun(),
                meta.privacy.label()
            ));
            match youtube::access_token()
                .and_then(|token| youtube::update_privacy(&token, &prior.video_id, meta.privacy))
            {
                Ok(()) => prior = record_privacy(session, prior, meta.privacy)?,
                Err(err) => match orientation {
                    Orientation::Horizontal => {
                        return Err(err).with_context(|| {
                            format!(
                                "{} is up but still {} on YouTube",
                                prior.url,
                                prior.privacy.label()
                            )
                        })
                    }
                    Orientation::Vertical => {
                        eprintln!("stream-recorder: the Short's visibility did not change: {err:#}")
                    }
                },
            }
        }
        return Ok(prior);
    }

    status(format!(
        "Uploading “{}” to YouTube as {}…",
        meta.title,
        orientation.noun()
    ));
    let token = youtube::access_token()?;
    let video_id = youtube::upload_video(&token, video, &meta)?;
    let url = match orientation {
        Orientation::Horizontal => format!("https://www.youtube.com/watch?v={video_id}"),
        Orientation::Vertical => format!("https://www.youtube.com/shorts/{video_id}"),
    };

    // Recorded before the thumbnail: the video is up either way, and a row
    // written only on complete success would lose the id if this next call fails.
    let mut upload = Upload {
        video_id: video_id.clone(),
        url: url.clone(),
        title: meta.title.clone(),
        source_hash,
        uploaded_at: crate::schedule::ledger::now_rfc3339(),
        thumbnail_set: false,
        thumbnail_at: None,
        privacy: meta.privacy,
        privacy_at: None,
        orientation,
    };
    append(session, &upload)?;

    if let Some(jpeg) = poster {
        status("Setting the thumbnail…".into());
        if set_poster(orientation, &video_id, &url, jpeg)? {
            upload = record_poster(session, upload)?;
        }
    }
    Ok(upload)
}

/// Sets the poster, and says whether it took.
///
/// The longform's designed thumbnail is most of why it goes up here rather
/// than through Buffer, so a thumbnail that will not set fails the upload with
/// the retry spelled out. The Shorts feed shows a frame of the video whatever
/// poster is set, so for a Short the same failure is a line in the log and a
/// `false`.
fn set_poster(orientation: Orientation, video_id: &str, url: &str, jpeg: &[u8]) -> Result<bool> {
    // Refresh after the video transfer; use the image frozen before it began.
    let result =
        youtube::access_token().and_then(|token| youtube::set_thumbnail(&token, video_id, jpeg));
    match (result, orientation) {
        (Ok(()), _) => Ok(true),
        (Err(err), Orientation::Horizontal) => Err(err).with_context(|| {
            format!(
                "Video is uploaded at {url}, but its thumbnail failed. Press Upload again to \
                 retry the thumbnail"
            )
        }),
        (Err(err), Orientation::Vertical) => {
            eprintln!("stream-recorder: the Short's poster did not set: {err:#}");
            Ok(false)
        }
    }
}

/// The upload owns its copy; social generation happens later in the workflow.
///
/// The Short shares the longform's title and description — it is the same
/// video — with `#Shorts` added to the description, which is how YouTube asks
/// to be told and costs nothing if it had already worked that out.
fn video_meta(session: &Session, orientation: Orientation) -> Result<youtube::VideoMeta> {
    let metadata = metadata::load(session);
    metadata.validate()?;
    let config = crate::config::load();
    let description = metadata.description.trim().to_string();
    Ok(youtube::VideoMeta {
        title: metadata.title.trim().to_string(),
        description: match orientation {
            Orientation::Horizontal => description,
            Orientation::Vertical => short_description(&description),
        },
        category_id: config.youtube_category_id,
        privacy: config.youtube_privacy,
    })
}

/// First letter up, for a reason written to read mid-sentence and shown alone.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The description with `#Shorts` on the end, unless it is already in there.
fn short_description(description: &str) -> String {
    if description.to_lowercase().contains("#shorts") {
        return description.to_string();
    }
    match description.is_empty() {
        true => "#Shorts".to_string(),
        false => format!("{description}\n\n#Shorts"),
    }
}

fn chosen_thumbnail(session: &Session, kind: crate::card::assets::Kind) -> Option<Vec<u8>> {
    let path = crate::card::assets::selected(&session.root, kind).ok()?;
    std::fs::read(path).ok()
}

/// The newest upload of the landscape longform: the video the blog embeds and
/// the YouTube tab reports. Rows for the Short are not it.
pub fn longform(session: &Session) -> Option<Upload> {
    load(session)
        .into_iter()
        .rev()
        .find(|row| row.orientation == Orientation::Horizontal)
}

/// The newest upload of the vertical cut as a Short, if one has gone up.
pub fn short(session: &Session) -> Option<Upload> {
    load(session)
        .into_iter()
        .rev()
        .find(|row| row.orientation == Orientation::Vertical)
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
fn hash_of_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(crate::agent::prompt::hash_of_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
            thumbnail_at: None,
            privacy: youtube::Privacy::Public,
            privacy_at: None,
            orientation: Orientation::Horizontal,
        }
    }

    fn short_row(hash: &str, id: &str) -> Upload {
        Upload {
            orientation: Orientation::Vertical,
            url: format!("https://www.youtube.com/shorts/{id}"),
            ..upload(hash, id)
        }
    }

    /// The blog embeds the longform and the YouTube tab reports it; a Short in
    /// the same ledger must not be mistaken for either, however new it is.
    #[test]
    fn the_longform_and_the_short_are_told_apart_by_orientation() {
        let root = temp("orientation");
        let session = session(&root);
        append(&session, &upload("aaaa", "long-1")).unwrap();
        append(&session, &short_row("bbbb", "short-1")).unwrap();
        assert_eq!(longform(&session).unwrap().video_id, "long-1");
        assert_eq!(short(&session).unwrap().video_id, "short-1");
        // A re-upload of the longform is the newest longform, Short or no Short.
        append(&session, &upload("cccc", "long-2")).unwrap();
        assert_eq!(longform(&session).unwrap().video_id, "long-2");
        assert_eq!(short(&session).unwrap().video_id, "short-1");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every row from before the Short existed was the longform.
    #[test]
    fn a_row_from_before_orientation_reads_as_the_longform() {
        let row: Upload = serde_json::from_str(
            r#"{"video_id":"abc","url":"https://y/abc","title":"A video",
                "source_hash":"h","uploaded_at":"2026-08-16T12:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(row.orientation, Orientation::Horizontal);
        let back = serde_json::to_string(&short_row("h", "s")).unwrap();
        assert!(back.contains(r#""orientation":"vertical""#), "{back}");
    }

    /// YouTube is told once, and never told twice.
    #[test]
    fn the_short_description_carries_the_tag_exactly_once() {
        assert_eq!(
            short_description("Four minutes on retries."),
            "Four minutes on retries.\n\n#Shorts"
        );
        assert_eq!(short_description(""), "#Shorts");
        assert_eq!(
            short_description("Already tagged #shorts here"),
            "Already tagged #shorts here"
        );
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

    /// Replace thumbnail writes the same kind of row a retry does — newest wins,
    /// the upload time is untouched, and the replacement time is its own field —
    /// and it targets the newest longform whatever the render on disk hashes to.
    #[test]
    fn a_replaced_poster_is_recorded_against_the_live_longform() {
        let root = temp("replace");
        let session = session(&root);
        append(&session, &upload("aaaa", "long-1")).unwrap();
        append(&session, &short_row("bbbb", "short-1")).unwrap();
        let live = longform(&session).unwrap();
        let row = record_poster(&session, live).unwrap();
        assert_eq!(
            row.video_id, "long-1",
            "the video on YouTube, not a hash lookup"
        );
        assert!(row.thumbnail_set);
        assert!(row.thumbnail_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(
            row.uploaded_at, "2026-08-16T12:00:00Z",
            "the upload time is history"
        );
        let back = longform(&session).unwrap();
        assert_eq!(back, row);
        assert_eq!(
            short(&session).unwrap().video_id,
            "short-1",
            "the Short is untouched"
        );
        // Rows from before the field existed still load, with no replacement time.
        let old: Upload = serde_json::from_str(
            r#"{"video_id":"abc","url":"https://y/abc","title":"A video",
                "source_hash":"h","uploaded_at":"2026-08-16T12:00:00Z","thumbnail_set":true}"#,
        )
        .unwrap();
        assert_eq!(old.thumbnail_at, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A visibility change from the picker is a row like a poster's: newest
    /// wins, the upload time is history, the change has its own time, and it
    /// lands on the video that is live rather than on a hash of the render.
    #[test]
    fn a_visibility_change_is_recorded_against_the_live_video() {
        let root = temp("visibility");
        let session = session(&root);
        append(&session, &upload("aaaa", "long-1")).unwrap();
        append(&session, &short_row("bbbb", "short-1")).unwrap();
        let live = longform(&session).unwrap();
        assert_eq!(live.privacy, youtube::Privacy::Public);
        let row = record_privacy(&session, live, youtube::Privacy::Unlisted).unwrap();
        assert_eq!(row.video_id, "long-1");
        assert_eq!(row.privacy, youtube::Privacy::Unlisted);
        assert!(row.privacy_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(row.uploaded_at, "2026-08-16T12:00:00Z");
        assert_eq!(
            longform(&session).unwrap(),
            row,
            "the newest row is the truth"
        );
        assert_eq!(
            short(&session).unwrap().privacy,
            youtube::Privacy::Public,
            "the Short has its own row and its own change"
        );
        // A row from before the field existed has no change time.
        let old: Upload = serde_json::from_str(
            r#"{"video_id":"abc","url":"https://y/abc","title":"A video",
                "source_hash":"h","uploaded_at":"2026-08-16T12:00:00Z","privacy":"private"}"#,
        )
        .unwrap();
        assert_eq!(old.privacy, youtube::Privacy::Private);
        assert_eq!(old.privacy_at, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_ledger_reads_as_nothing_uploaded() {
        let root = temp("empty");
        assert!(load(&session(&root)).is_empty());
        assert!(uploaded(&session(&root), "aaaa").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Three minutes is the line YouTube draws: at it a portrait upload is a
    /// Short, past it the same upload is an ordinary video under a `/shorts/`
    /// URL that lies. So the check is strictly over, and the reason names both
    /// lengths so the operator can see how far over it is.
    #[test]
    fn a_vertical_over_three_minutes_is_skipped_and_one_at_the_line_is_not() {
        assert_eq!(short_length_block(59.0), None);
        assert_eq!(short_length_block(180.0), None);
        let why = short_length_block(245.3).expect("over the limit");
        assert!(why.contains("4:05"), "{why}");
        assert!(why.contains("3:00"), "{why}");
        assert!(why.contains("not uploading"), "{why}");
        // Just over rounds to the limit on screen, and is still over.
        assert!(short_length_block(180.4).is_some());
    }

    #[test]
    fn lengths_read_as_minutes_and_seconds() {
        assert_eq!(mmss(0.0), "0:00");
        assert_eq!(mmss(59.6), "1:00");
        assert_eq!(mmss(180.0), "3:00");
        assert_eq!(mmss(605.0), "10:05");
        assert_eq!(capitalize("the vertical"), "The vertical");
        assert_eq!(capitalize(""), "");
    }

    /// "Make sure we skip it" has to fail closed: a file whose length cannot be
    /// read is not uploaded as a Short either, and the reason says why.
    #[test]
    fn an_unreadable_vertical_is_skipped_rather_than_guessed_at() {
        let root = temp("unreadable");
        let bogus = root.join("longform.mp4");
        std::fs::write(&bogus, b"not a video").unwrap();
        let why = short_block(&bogus).expect("skipped");
        assert!(why.contains("could not read"), "{why}");
        assert!(why.contains("not uploading"), "{why}");
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
