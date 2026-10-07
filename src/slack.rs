//! Tells the team on Slack when a video goes up on YouTube.
//!
//! One message per fresh upload, from the upload worker the moment the video
//! lands — see `publish::upload_one`. Never fatal: the video is live whether or
//! not the message goes, so a failure here is a status line, not an error.
//!
//! Off until `SLACK_WEBHOOK_URL` is set. It is an incoming webhook, the same
//! kind the landing site's signup notifications use, and the channel is the one
//! picked when the webhook was made — there is no channel to set here.

use anyhow::{bail, Result};

use crate::publish::Upload;

pub const WEBHOOK: &str = "SLACK_WEBHOOK_URL";
const PREFIX: &str = "https://hooks.slack.com/";

/// The webhook, trimmed, when one is set.
fn webhook() -> Option<String> {
    let value = std::env::var(WEBHOOK).unwrap_or_default();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// What the channel reads. The URL goes in bare so Slack unfurls it into the
/// player card with the thumbnail.
pub fn text(upload: &Upload) -> String {
    let what = match upload.orientation {
        crate::publish::Orientation::Horizontal => "New video",
        crate::publish::Orientation::Vertical => "New Short",
    };
    format!("{what}: *{}*\n{}", escape(&upload.title), upload.url)
}

/// Slack reads `&`, `<` and `>` as markup in message text.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Posts the upload's link, and says how that went in a sentence for the
/// status line. `None` when Slack is not set up, so an unconfigured app says
/// nothing about it.
pub fn announce(upload: &Upload) -> Option<String> {
    let url = webhook()?;
    // A private video's link opens for nobody in the channel.
    if upload.privacy == crate::publish::youtube::Privacy::Private {
        return Some("Not posted to Slack: the video is private.".into());
    }
    match post(&url, &text(upload)) {
        Ok(()) => {
            eprintln!("stream-recorder: posted {} to slack", upload.url);
            Some("Posted the link to Slack.".into())
        }
        Err(err) => {
            eprintln!("stream-recorder: slack post failed: {err:#}");
            Some(format!("The link did not reach Slack: {err:#}"))
        }
    }
}

/// For the Settings pane's Test button. Shape only: a webhook has no call that
/// proves it works without posting a message to the channel.
pub fn check() -> Result<String> {
    let Some(url) = webhook() else {
        bail!("{WEBHOOK} is not set");
    };
    if !url.starts_with(PREFIX) {
        bail!("{WEBHOOK} should start with {PREFIX}");
    }
    Ok("webhook looks right — the first upload is the real test".into())
}

/// Slack answers a webhook with a plain-text body: `ok`, or an error code.
fn post(url: &str, text: &str) -> Result<()> {
    if !url.starts_with(PREFIX) {
        bail!("{WEBHOOK} should start with {PREFIX}");
    }
    send(url, url, text)
}

/// `target` is where the request goes; `_webhook` is only there so a test can
/// point at a dead port while checking the real URL never reaches the error.
fn send(_webhook: &str, target: &str, text: &str) -> Result<()> {
    let response = ureq::post(target)
        .timeout(std::time::Duration::from_secs(15))
        .send_json(serde_json::json!({ "text": text, "unfurl_links": true }));
    match response {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            bail!("{}", explain(code, body.trim()))
        }
        // Never the error itself: ureq puts the request URL in a transport
        // error's message, and the webhook URL is the secret.
        Err(ureq::Error::Transport(transport)) => {
            bail!("could not reach Slack ({})", transport.kind())
        }
    }
}

/// The codes that have a fix someone can act on.
fn explain(status: u16, code: &str) -> String {
    match code {
        "no_service" | "no_team" | "team_disabled" | "invalid_token" => {
            format!("the webhook was revoked ({code}) — make a new one and update {WEBHOOK}")
        }
        "channel_not_found" | "channel_is_archived" => {
            format!("the webhook's channel is gone ({code}) — make a new one")
        }
        "" => format!("Slack http {status}"),
        other => format!("Slack http {status}: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::{youtube::Privacy, Orientation};

    fn upload(title: &str, orientation: Orientation) -> Upload {
        Upload {
            video_id: "abc".into(),
            url: "https://youtu.be/abc".into(),
            title: title.into(),
            source_hash: "h".into(),
            uploaded_at: "2026-10-05T00:00:00Z".into(),
            thumbnail_set: true,
            thumbnail_at: None,
            privacy: Privacy::Public,
            privacy_at: None,
            orientation,
            slack: None,
        }
    }

    #[test]
    fn the_message_names_the_video_and_leaves_the_link_bare_to_unfurl() {
        let message = text(&upload("Ship it <fast> & safe", Orientation::Horizontal));
        assert_eq!(
            message,
            "New video: *Ship it &lt;fast&gt; &amp; safe*\nhttps://youtu.be/abc"
        );
        assert!(text(&upload("x", Orientation::Vertical)).starts_with("New Short"));
    }

    #[test]
    fn the_fixable_errors_say_what_to_fix() {
        assert!(explain(404, "no_service").contains(WEBHOOK));
        assert!(explain(404, "channel_not_found").contains("make a new one"));
        assert_eq!(explain(500, ""), "Slack http 500");
    }

    #[test]
    fn a_transport_failure_never_repeats_the_webhook_url() {
        // Nothing listens on port 9, so this fails at connect — the same path a
        // DNS failure or a timeout takes.
        let secret = "https://hooks.slack.com/services/T000/B000/SECRET";
        let err =
            super::send(secret, "http://127.0.0.1:9/services/T000/B000/SECRET", "hi").unwrap_err();
        assert!(!format!("{err:#}").contains("SECRET"), "{err:#}");
    }

    #[test]
    fn a_url_that_is_not_a_slack_webhook_is_never_sent_to() {
        let err = post("https://example.com/hook", "hi").unwrap_err();
        assert!(format!("{err}").contains(PREFIX));
    }
}
