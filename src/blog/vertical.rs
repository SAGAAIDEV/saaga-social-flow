//! Optional portrait video: the vertical longform, as the page's mobile player.
//!
//! Two sources, in order. The YouTube Short, when the vertical cut went up with
//! the longform — see [`crate::publish::short`] — which the page embeds the way
//! it embeds the landscape video and which costs the CMS nothing to hold.
//! Failing that, the file itself, uploaded into the media library: what this did
//! before the Short existed, and what a project that skipped YouTube still gets.
//! Its designed poster is supplied by the artwork set either way.
use super::{payload, strapi};
use crate::session::Session;
use anyhow::{Context, Result};

/// The cut as a Short, when one has gone up. No network: a ledger read.
pub(super) fn from_youtube(session: &Session) -> Option<payload::VerticalCut> {
    crate::publish::short(session).map(|short| payload::VerticalCut {
        url: short.url,
        video_id: Some(short.video_id),
    })
}

pub(super) fn upload(
    client: &strapi::Strapi,
    session: &Session,
    status: impl Fn(String),
) -> Result<Option<payload::VerticalCut>> {
    if let Some(short) = from_youtube(session) {
        status("Embedding the vertical cut from YouTube…".into());
        return Ok(Some(short));
    }
    let video = session.render_dir().join("vertical/longform.mp4");
    if !video.is_file() {
        return Ok(None);
    }
    status("Uploading the vertical cut…".into());
    let uploaded = client
        .upload_video(&video)
        .with_context(|| format!("uploading {}", video.display()))?;
    Ok(Some(payload::VerticalCut {
        url: uploaded.url,
        video_id: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(tag: &str) -> Session {
        let root = std::env::temp_dir().join(format!("blog-vertical-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    /// No portrait cut is the common case and it has to cost nothing: no
    /// upload, no artwork lookup, no error. The post then renders one player at
    /// every width, exactly as it did before this module existed.
    #[test]
    fn a_project_with_no_vertical_cut_uploads_nothing() {
        let session = session("none");
        // Reaching the network at all is the failure here: a horizontal-only
        // project is a normal project, not a degraded one.
        let got = upload(&strapi::Strapi::for_test(), &session, |_| {}).unwrap();
        assert!(got.is_none());
        assert!(from_youtube(&session).is_none());
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// A Short in the ledger is the vertical video, and the CMS is never sent
    /// the file: the client here points nowhere, so an upload would fail.
    #[test]
    fn a_short_on_youtube_is_embedded_rather_than_uploaded() {
        let session = session("short");
        std::fs::write(
            session.root.join(crate::publish::UPLOADS_JSONL),
            concat!(
                r#"{"video_id":"long1","url":"https://www.youtube.com/watch?v=long1","title":"T","#,
                r#""source_hash":"a","uploaded_at":"2026-09-08T12:00:00Z","orientation":"horizontal"}"#,
                "\n",
                r#"{"video_id":"short1","url":"https://www.youtube.com/shorts/short1","title":"T","#,
                r#""source_hash":"b","uploaded_at":"2026-09-08T12:05:00Z","orientation":"vertical"}"#,
                "\n",
            ),
        )
        .unwrap();
        let got = upload(&strapi::Strapi::for_test(), &session, |_| {})
            .unwrap()
            .expect("the short is the vertical video");
        assert_eq!(got.video_id.as_deref(), Some("short1"));
        assert_eq!(got.url, "https://www.youtube.com/shorts/short1");
        let _ = std::fs::remove_dir_all(&session.root);
    }
}
