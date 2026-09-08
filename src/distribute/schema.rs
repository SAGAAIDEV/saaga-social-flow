use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const LINKS_JSON: &str = "links.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributedAsset {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributeLinks {
    pub project: String,
    pub version: u32,
    pub items: Vec<DistributedAsset>,
}

impl DistributeLinks {
    pub fn url_for(&self, id: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.url.as_str())
    }
}

pub fn save(dir: &Path, links: &DistributeLinks) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(LINKS_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(links).context("serializing distribute links")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load(dir: &Path) -> Result<DistributeLinks> {
    let path = dir.join(LINKS_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-dist-{}-schema",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let links = DistributeLinks {
            project: "vd-1".into(),
            version: 2,
            items: vec![DistributedAsset {
                id: "longform".into(),
                kind: "video".into(),
                orientation: Some("landscape".into()),
                chapter: None,
                url: "https://example.com/long.mp4".into(),
                file: Some("longform.mp4".into()),
            }],
        };
        save(&dir, &links).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(
            back.url_for("longform"),
            Some("https://example.com/long.mp4")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
