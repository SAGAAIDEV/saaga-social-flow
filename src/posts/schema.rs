use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformPost {
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoPosts {
    pub video_id: String,
    pub video_type: String, // "horizontal" or "vertical"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_path: Option<String>,
    pub posts: Vec<PlatformPost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostsManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Which `posts.social` preamble produced this copy — `Some(0)` builtin,
    /// `Some(n)` a recorded overlay, `None` an overlay edited outside the ledger.
    /// Carried into the schedule ledger so performance can be attributed to a
    /// prompt version rather than to a timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt_hash: String,
    pub items: Vec<VideoPosts>,
}

pub const POSTS_JSON: &str = "posts.json";

pub fn platform_label(platform: &str) -> &'static str {
    match platform {
        "twitter" => "Twitter / X",
        "bluesky" => "Bluesky",
        "instagram" => "Instagram Reels",
        "facebook" => "Facebook",
        "youtube_shorts" => "YouTube Shorts",
        "youtube" => "YouTube",
        "tiktok" => "TikTok",
        "linkedin" => "LinkedIn",
        _ => "Post",
    }
}

pub fn save_manifest(dir: &Path, manifest: &PostsManifest) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let json_path = dir.join(POSTS_JSON);
    let serialized = serde_json::to_string_pretty(manifest)
        .context("serializing posts manifest")?
        + "\n";
    std::fs::write(&json_path, serialized)
        .with_context(|| format!("writing {}", json_path.display()))?;

    // Also write individual markdown files for each video and platform
    for item in &manifest.items {
        let video_dir = dir.join(&item.video_id);
        std::fs::create_dir_all(&video_dir)
            .with_context(|| format!("creating {}", video_dir.display()))?;
        for post in &item.posts {
            let md_path = video_dir.join(format!("{}.md", post.platform));
            let mut content = String::new();
            if let Some(ref title) = post.title {
                content.push_str(&format!("# {}\n\n", title));
            }
            content.push_str(&post.content);
            if !post.tags.is_empty() {
                content.push_str("\n\n");
                let tags_str = post
                    .tags
                    .iter()
                    .map(|t| if t.starts_with('#') { t.clone() } else { format!("#{t}") })
                    .collect::<Vec<_>>()
                    .join(" ");
                content.push_str(&tags_str);
            }
            content.push('\n');
            let _ = std::fs::write(&md_path, content);
        }
    }
    Ok(json_path)
}

pub fn load_manifest(dir: &Path) -> Result<PostsManifest> {
    let json_path = dir.join(POSTS_JSON);
    let text = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading {}", json_path.display()))?;
    Ok(serde_json::from_str(&text).with_context(|| format!("parsing {}", json_path.display()))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip() {
        let dir = std::env::temp_dir().join(format!("posts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = PostsManifest {
            version: Some(1),
            prompt_version: Some(0),
            prompt_hash: "abc".into(),
            items: vec![VideoPosts {
                video_id: "longform".into(),
                video_type: "horizontal".into(),
                video_path: Some("horizontal/longform.mp4".into()),
                posts: vec![PlatformPost {
                    platform: "twitter".into(),
                    title: None,
                    content: "Exciting new release!".into(),
                    tags: vec!["coding".into(), "rust".into()],
                }],
            }],
        };
        let path = save_manifest(&dir, &manifest).expect("save");
        assert!(path.exists());
        assert!(dir.join("longform/twitter.md").exists());
        let back = load_manifest(&dir).expect("load");
        assert_eq!(back, manifest);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
