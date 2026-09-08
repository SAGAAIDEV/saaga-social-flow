//! Upload the reviewed 1200×630 OG artwork; failure must stop the CMS write.
use super::strapi;
use anyhow::Result;
use std::path::Path;

pub(super) fn upload(
    client: &strapi::Strapi,
    path: &Path,
    alt: &str,
    status: impl Fn(String),
) -> Result<i64> {
    status("Uploading OG artwork…".into());
    client.upload_media(path, Some(alt))
}
