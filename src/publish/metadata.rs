//! YouTube copy belongs to the upload, independently of downstream social posts.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use crate::session::Session;

const FILE: &str = "youtube-metadata.json";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

pub fn load(session: &Session) -> Metadata {
    std::fs::read_to_string(session.root.join(FILE)).ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .or_else(|| legacy(session))
        .unwrap_or_else(|| Metadata { title: session.title(), description: String::new() })
}

// Existing projects retain their previously prepared YouTube copy.
fn legacy(session: &Session) -> Option<Metadata> {
    let manifest = crate::posts::load_manifest(&session.posts_dir()).ok()?;
    let post = manifest.items.iter().find(|item| item.video_id == "longform")?
        .posts.iter().find(|post| post.platform == "youtube" || post.platform == "youtube_shorts")?;
    Some(Metadata {
        title: post.title.clone().filter(|title| !title.trim().is_empty()).unwrap_or_else(|| session.title()),
        description: crate::schedule::copy::render_text(&post.content, &post.tags),
    })
}

impl Metadata {
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() || self.title.chars().count() > 100 {
            bail!("YouTube title must contain 1–100 characters");
        }
        if self.description.chars().count() > 5000 {
            bail!("YouTube description must be at most 5,000 characters");
        }
        Ok(())
    }
}

pub fn save(session: &Session, metadata: &Metadata) -> Result<()> {
    metadata.validate()?;
    std::fs::create_dir_all(&session.root)?;
    std::fs::write(session.root.join(FILE), serde_json::to_string_pretty(metadata)?)
        .context("saving YouTube title and description")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn youtube_copy_can_be_saved_without_social_posts() {
        let root = std::env::temp_dir().join(format!("youtube-metadata-{}", std::process::id()));
        let session = Session { dir: root.join("drafts"), root: root.clone(), version: None };
        let metadata = Metadata { title: "A video".into(), description: "About the video".into() };
        save(&session, &metadata).unwrap();
        assert_eq!(load(&session), metadata);
        assert!(!session.posts_dir().exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn invalid_metadata_is_refused_before_upload() {
        assert!(Metadata { title: " ".into(), description: String::new() }.validate().is_err());
        assert!(Metadata { title: "x".repeat(101), description: String::new() }.validate().is_err());
        assert!(Metadata { title: "Fine".into(), description: "x".repeat(5001) }.validate().is_err());
    }
}
