//! The YouTube Data API: uploading a video, and setting its thumbnail.
//!
//! The thumbnail is the one thing Buffer will not carry. It refuses
//! `thumbnailUrl` on a video asset outright — "social networks do not accept
//! custom video thumbnail images" — and its only alternative,
//! `metadata.thumbnailOffset`, picks a frame *out of the video* and is
//! Instagram, TikTok and Pinterest only. So a designed 16:9 thumbnail reaches
//! YouTube here or not at all, and once the upload had to happen here anyway,
//! the video came with it.
//!
//! Tokens are minted here, in this process: [`super::oauth`] runs the consent
//! and [`super::token_store`] keeps the result. That store is the same sqlite
//! file the Python side reads, so the connection is still shared even though no
//! part of this path shells out to it.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::oauth::{self, Credentials};
use super::token_store;

const SET_THUMBNAIL: &str = "https://www.googleapis.com/upload/youtube/v3/thumbnails/set";
const UPLOAD_VIDEO: &str = "https://www.googleapis.com/upload/youtube/v3/videos";

/// Who can see a video once it is up.
///
/// An enum rather than the `String` this was, because the value has to survive
/// three hops that a free-form string does not police: a saved config someone
/// may have hand-edited, a popup index, and YouTube's own enum. A typo reaches
/// the API as `privacyStatus: "publik"`, and the API's answer to that is a
/// rejected upload *after* the bytes have gone up — the same expensive failure
/// mode `VideoMeta::body`'s title truncation exists to avoid.
///
/// Serialized lowercase, which is what the Data API takes and also what a
/// config file should read like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    /// Listed, searchable, on the channel. The default, and what every upload
    /// before this was a choice did.
    #[default]
    Public,
    /// Anyone with the link, but not listed or searchable.
    Unlisted,
    /// Only the channel owner and anyone explicitly shared with.
    Private,
}

impl Privacy {
    /// In the order the picker offers them: most open first, so the riskiest
    /// choice is never the one adjacent to the default.
    pub const ALL: [Privacy; 3] = [Privacy::Public, Privacy::Unlisted, Privacy::Private];

    /// The API's own spelling, and what goes in `privacyStatus`.
    pub fn as_str(self) -> &'static str {
        match self {
            Privacy::Public => "public",
            Privacy::Unlisted => "unlisted",
            Privacy::Private => "private",
        }
    }

    /// What the picker shows.
    pub fn label(self) -> &'static str {
        match self {
            Privacy::Public => "Public",
            Privacy::Unlisted => "Unlisted",
            Privacy::Private => "Private",
        }
    }

    /// Its position in [`Privacy::ALL`], for the popup's selected index.
    pub fn index(self) -> usize {
        Privacy::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// What a video is called and how it is filed, at upload time.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoMeta {
    pub title: String,
    pub description: String,
    /// YouTube's numeric category id, from config.
    pub category_id: String,
    /// Who can see it. Set on the YouTube tab; see [`Privacy`].
    pub privacy: Privacy,
}

impl VideoMeta {
    /// The `snippet` and `status` parts, as the API wants them.
    ///
    /// A title over a hundred characters is rejected outright, and rejection
    /// after the bytes have gone up is the expensive way to find out — so it is
    /// cut here, on a character boundary rather than a byte one.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "snippet": {
                "title": truncate(&self.title, 100),
                "description": truncate(&self.description, 5000),
                "categoryId": self.category_id,
            },
            "status": {
                "privacyStatus": self.privacy.as_str(),
                "selfDeclaredMadeForKids": false,
            },
        })
    }
}

fn truncate(text: &str, chars: usize) -> String {
    match text.chars().count() > chars {
        true => text.chars().take(chars).collect(),
        false => text.to_string(),
    }
}

/// Runs the OAuth flow for YouTube, which opens a browser.
///
/// Blocks until the flow finishes: the consent screen is only half of it, and
/// returning at the point the browser opens would report success before there
/// was anything to succeed at.
///
/// Needed because a refresh token dies — Google expires them after seven days
/// while the OAuth app is still in "Testing" — and the only cure is for a human
/// to grant again.
pub fn connect() -> Result<()> {
    let creds = Credentials::from_env()?;
    let token = oauth::consent(&creds)?;
    expected_channel(&token)?;
    let scope = token
        .extra
        .get("scope")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    token_store::save(&token, scope.as_deref())?;
    Ok(())
}

/// Refuse a token bound to the wrong channel, before it is ever stored.
///
/// Google's chooser decides which channel a consent grants, and a Google
/// account with a brand channel offers both. Getting it wrong is not visible
/// until a finished render is public on the wrong channel, so the check happens
/// here and nothing is saved on a mismatch.
///
/// With `YOUTUBE_CHANNEL_ID` unset there is nothing to check against and
/// whatever was granted is accepted — the id is recorded either way.
fn expected_channel(token: &token_store::Token) -> Result<()> {
    let expected = std::env::var("YOUTUBE_CHANNEL_ID").unwrap_or_default();
    let expected = expected.trim();
    if expected.is_empty() {
        return Ok(());
    }
    let got = token.channel_id.as_deref().unwrap_or("(none)");
    if got == expected {
        return Ok(());
    }
    let title = token.channel_title.as_deref().unwrap_or("untitled");
    bail!(
        "connected to the wrong YouTube channel: the grant is bound to {got} \
         ({title}), but YOUTUBE_CHANNEL_ID expects {expected}. Nothing was \
         saved.\n\nGoogle caches which channel a client is bound to, so \
         re-consenting silently reuses {got} and never offers the chooser — \
         `prompt=consent` re-asks for permission, not for a channel. To break \
         it: remove this app at https://myaccount.google.com/permissions, \
         switch to the right channel on youtube.com, then Connect again and \
         pick it when Google asks."
    )
}

/// A valid access token for the stored YouTube account.
///
/// Refreshes on read and persists what it refreshed, so callers never deal with
/// expiry. Sixty seconds of slack because the token is about to be used for an
/// upload that takes longer than the round trip that fetched it.
pub fn access_token() -> Result<String> {
    let Some(mut token) = token_store::load()? else {
        bail!("YouTube is not connected — run Connect first");
    };
    if !token.is_stale(60) {
        return Ok(token.access_token);
    }
    let Some(refresh_token) = token.refresh_token.clone() else {
        bail!("the stored YouTube token has no refresh token — connect again");
    };

    let creds = Credentials::from_env()?;
    // A refresh token belongs to the client that minted it. When the stored one
    // came from somewhere else, say so plainly rather than letting Google's
    // `invalid_grant` stand in for it.
    if let Some(minted_by) = token.client_id.as_deref() {
        if minted_by != creds.id {
            bail!(
                "the stored YouTube token was minted by a different OAuth client \
                 ({minted_by}) and cannot be refreshed here — connect again"
            );
        }
    }

    let (access, expires_at) = oauth::refresh(&creds, &refresh_token)
        .map_err(|err| explain_dead_refresh(err, &creds.id))?;
    token.access_token = access;
    token.expires_at = Some(expires_at);
    // Carry the minter forward for a token first stored by the Python side,
    // which does not record one.
    token.client_id = Some(creds.id.clone());
    let scope = token
        .extra
        .get("scope")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    token_store::save(&token, scope.as_deref())?;
    Ok(token.access_token)
}

/// A refresh Google refused, said in terms of what to do about it.
///
/// `invalid_grant` on a refresh means the refresh token itself is dead, and by
/// far the commonest reason is the OAuth app still being in "Testing", where
/// Google expires every refresh token after seven days. The store is fine and
/// the code is fine — connecting again only buys another week — so the message
/// names the setting that ends it, rather than leaving "connect again" to read
/// as the whole answer.
fn explain_dead_refresh(err: anyhow::Error, client_id: &str) -> anyhow::Error {
    let text = format!("{err:#}");
    if !text.contains("invalid_grant") {
        return err;
    }
    anyhow::anyhow!(
        "{text}\n\nGoogle has expired the YouTube connection. It does this every seven days \
         while the OAuth app (client {client_id}) is in \"Testing\". To stay connected: Google \
         Cloud Console → APIs & Services → OAuth consent screen → Publish app (or make the \
         app Internal), then press Connect once more."
    )
}

/// Uploads a video file and returns its YouTube id.
///
/// Resumable in two steps, which is what the API wants for anything but a tiny
/// file: the metadata goes up as JSON and comes back with a session URL, then the
/// bytes are PUT to that URL. The file is streamed from disk rather than read
/// into memory — a longform render is tens of megabytes and there is no reason
/// for all of it to be resident at once.
pub fn upload_video(token: &str, path: &Path, meta: &VideoMeta) -> Result<String> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len();
    let started = ureq::post(UPLOAD_VIDEO)
        .query("uploadType", "resumable")
        .query("part", "snippet,status")
        // The API's default, stated rather than assumed — it matches what the
        // Buffer path sends for the shorts, and it is the whole point of
        // publishing a longform to a channel people subscribed to.
        .query("notifySubscribers", "true")
        .set("Authorization", &format!("Bearer {token}"))
        .set("X-Upload-Content-Type", "video/mp4")
        .set("X-Upload-Content-Length", &size.to_string())
        .timeout(std::time::Duration::from_secs(120))
        .send_json(meta.body());
    let session = match started {
        Ok(response) => response
            .header("location")
            .map(str::to_string)
            .context("youtube started an upload without a session url")?,
        Err(ureq::Error::Status(code, response)) => {
            let detail = response.into_string().unwrap_or_default();
            bail!("youtube refused the upload ({code}): {}", detail.trim());
        }
        Err(err) => return Err(err).context("starting the youtube upload"),
    };

    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let finished = ureq::put(&session)
        .set("Content-Type", "video/mp4")
        .set("Content-Length", &size.to_string())
        // Minutes, not seconds: this is the whole video going over the wire.
        .timeout(std::time::Duration::from_secs(3600))
        .send(file);
    let body: serde_json::Value = match finished {
        Ok(response) => response
            .into_json()
            .context("parsing the upload response")?,
        Err(ureq::Error::Status(code, response)) => {
            let detail = response.into_string().unwrap_or_default();
            bail!("youtube rejected the video ({code}): {}", detail.trim());
        }
        Err(err) => return Err(err).context("sending the video to youtube"),
    };
    body.get("id")
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .context("youtube accepted the video but named no id")
}

/// Uploads `jpeg` as the thumbnail of `video_id`.
///
/// Idempotent at YouTube's end — setting the same image twice replaces it with
/// itself — so this needs no ledger of its own to be safe to press again.
pub fn set_thumbnail(token: &str, video_id: &str, jpeg: &[u8]) -> Result<()> {
    let response = ureq::post(SET_THUMBNAIL)
        .query("videoId", video_id)
        .query("uploadType", "media")
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "image/jpeg")
        .timeout(std::time::Duration::from_secs(120))
        .send_bytes(jpeg);
    match response {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, response)) => {
            let detail = response.into_string().unwrap_or_default();
            bail!("youtube refused the thumbnail ({code}): {}", detail.trim());
        }
        Err(err) => Err(err).context("calling youtube thumbnails.set"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upload body is the one thing here that can be checked without a
    /// network: everything else is a round trip to Google.
    #[test]
    fn the_upload_body_carries_the_snippet_and_the_status() {
        let meta = VideoMeta {
            title: "A talk".into(),
            description: "What it is about".into(),
            category_id: "28".into(),
            privacy: Privacy::Unlisted,
        };
        let body = meta.body();
        assert_eq!(body["snippet"]["title"], "A talk");
        assert_eq!(body["snippet"]["description"], "What it is about");
        assert_eq!(body["status"]["privacyStatus"], "unlisted");
        // Declared explicitly rather than left to the channel default, which
        // would disable comments and end screens on every upload.
        assert_eq!(body["status"]["selfDeclaredMadeForKids"], false);
    }

    /// Cut on a character boundary, not a byte one — an em dash near the limit
    /// would otherwise split into invalid UTF-8.
    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate("a—b", 2), "a—");
        assert_eq!(truncate("short", 100), "short");
    }

    /// The three spellings YouTube accepts, exactly. Nothing here is
    /// case-insensitive at the API and a wrong one fails the upload after the
    /// bytes have gone, so this is worth pinning literally rather than by
    /// round trip.
    #[test]
    fn the_api_spellings_are_youtubes_own() {
        assert_eq!(Privacy::Public.as_str(), "public");
        assert_eq!(Privacy::Unlisted.as_str(), "unlisted");
        assert_eq!(Privacy::Private.as_str(), "private");
    }

    /// The popup hands back an index into `ALL`, so an `ALL` reordered without
    /// `index` following it would silently upload under the wrong visibility —
    /// a picker showing "Private" while the video goes up public.
    #[test]
    fn every_privacy_survives_the_round_trip_through_its_popup_index() {
        for privacy in Privacy::ALL {
            assert_eq!(
                Privacy::ALL[privacy.index()],
                privacy,
                "{privacy:?} does not round-trip through its own index"
            );
        }
        assert_eq!(
            Privacy::ALL.len(),
            3,
            "a new option needs a label and an index"
        );
    }

    /// Public leads the list. Not cosmetic: the popup's first entry is what a
    /// mis-click lands on, and it should be the choice that loses nothing
    /// rather than the one that hides a video nobody then notices is hidden.
    #[test]
    fn the_most_open_choice_leads_and_is_the_default() {
        assert_eq!(Privacy::ALL[0], Privacy::Public);
        assert_eq!(Privacy::default(), Privacy::Public);
    }

    /// A config written before this setting existed must upload exactly as it
    /// always did. `#[serde(default)]` on the field plus `Default` here is what
    /// makes that true, and neither is much use without the other.
    #[test]
    fn a_config_with_no_visibility_uploads_public_as_it_always_did() {
        let cfg: crate::config::Config =
            serde_json::from_str("{}").expect("an empty config is a valid one");
        assert_eq!(cfg.youtube_privacy, Privacy::Public);
    }

    /// Lowercase in the file, because that is both what the API takes and what
    /// someone hand-editing the config would write.
    #[test]
    fn the_config_spelling_is_lowercase_both_ways() {
        assert_eq!(
            serde_json::to_string(&Privacy::Unlisted).unwrap(),
            "\"unlisted\""
        );
        assert_eq!(
            serde_json::from_str::<Privacy>("\"private\"").unwrap(),
            Privacy::Private
        );
    }

    /// The picker's choice is what reaches the wire — the whole point of the
    /// setting, and the one link in the chain that spans two modules.
    #[test]
    fn the_chosen_visibility_is_what_reaches_the_api_body() {
        for privacy in Privacy::ALL {
            let meta = VideoMeta {
                title: "A talk".into(),
                description: String::new(),
                category_id: "28".into(),
                privacy,
            };
            assert_eq!(
                meta.body()["status"]["privacyStatus"],
                privacy.as_str(),
                "{privacy:?} did not survive into the upload body"
            );
        }
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::explain_dead_refresh;

    /// The failure that sends people back to the browser every week has to say
    /// why, and name the client, or the fix is never found.
    #[test]
    fn a_dead_refresh_token_names_the_testing_mode_cause() {
        let err = anyhow::Error::msg(
            r#"Google returned 400: {"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        );
        let explained = format!("{:#}", explain_dead_refresh(err, "532651497555"));
        assert!(explained.contains("invalid_grant"), "{explained}");
        assert!(explained.contains("Testing"), "{explained}");
        assert!(explained.contains("532651497555"), "{explained}");
        assert!(explained.contains("Publish app"), "{explained}");
    }

    /// Any other refresh failure is not this one and must not be dressed as it.
    #[test]
    fn other_refresh_failures_pass_through_unchanged() {
        let err = anyhow::Error::msg("Google returned 503: try again later");
        let explained = format!("{:#}", explain_dead_refresh(err, "x"));
        assert_eq!(explained, "Google returned 503: try again later");
    }
}
