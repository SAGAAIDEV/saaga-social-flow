//! Optional portrait video; its designed poster is supplied by the artwork set.
use anyhow::{Context, Result};
use super::{payload, strapi};
use crate::session::Session;
pub(super) fn upload(client: &strapi::Strapi, session: &Session, status: impl Fn(String)) -> Result<Option<payload::VerticalCut>> {
    let video = session.render_dir().join("vertical/longform.mp4");
    if !video.is_file() { return Ok(None); }
    status("Uploading the vertical cut…".into());
    let uploaded = client.upload_video(&video).with_context(|| format!("uploading {}", video.display()))?;
    Ok(Some(payload::VerticalCut { url: uploaded.url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No portrait cut is the common case and it has to cost nothing: no
    /// upload, no artwork lookup, no error. The post then renders one player at
    /// every width, exactly as it did before this module existed.
    #[test]
    fn a_project_with_no_vertical_cut_uploads_nothing() {
        let root = std::env::temp_dir().join(format!("blog-vertical-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let session = Session { root: root.clone(), dir: root.join("drafts"), version: None };
        // Reaching the network at all is the failure here: a horizontal-only
        // project is a normal project, not a degraded one.
        let got = upload(&strapi::Strapi::for_test(), &session, |_| {}).unwrap();
        assert!(got.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
