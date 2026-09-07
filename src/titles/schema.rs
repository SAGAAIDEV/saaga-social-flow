use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterTitle {
    pub n: u32,
    pub title: String,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitlesManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// The whole video's title, shown on the opening card and used as the
    /// YouTube and blog headline. `default` rather than required so a manifest
    /// written before this existed still loads.
    #[serde(default)]
    pub longform: String,
    pub chapters: Vec<ChapterTitle>,
}

pub const TITLES_JSON: &str = "titles.json";

pub fn save(dir: &Path, manifest: &TitlesManifest) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(TITLES_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(manifest).context("serializing titles")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load(dir: &Path) -> Result<TitlesManifest> {
    let path = dir.join(TITLES_JSON);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

impl TitlesManifest {
    /// The video's own title, if one has been written. `None` rather than an
    /// empty string so a caller has to say what it wants instead — the opening
    /// card falls back to the project folder's name.
    pub fn longform_title(&self) -> Option<&str> {
        let title = self.longform.trim();
        (!title.is_empty()).then_some(title)
    }

    pub fn title_for(&self, n: u32) -> Option<&str> {
        let chapter = self.chapters.iter().find(|c| c.n == n)?;
        let title = chapter.title.trim();
        (!title.is_empty()).then_some(title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_round_trip() {
        let dir = std::env::temp_dir().join(format!("titles-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = TitlesManifest {
            version: Some(1),
            longform: "Why watermarking fails".into(),
            chapters: vec![ChapterTitle {
                n: 1,
                title: "The Hook".into(),
                approved: true,
            }],
        };
        save(&dir, &manifest).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.title_for(1), Some("The Hook"));
        assert_eq!(back.longform_title(), Some("Why watermarking fails"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A manifest written before the longform title existed still loads, and
    /// simply has no title of its own.
    #[test]
    fn a_manifest_from_before_the_longform_title_still_loads() {
        let older: TitlesManifest =
            serde_json::from_str(r#"{"chapters":[{"n":1,"title":"The Hook"}]}"#).unwrap();
        assert_eq!(older.longform_title(), None);
        assert_eq!(older.title_for(1), Some("The Hook"));
    }

    /// Whitespace is not a title. The opening card would render a blank.
    #[test]
    fn a_blank_longform_title_reads_as_none() {
        let blank = TitlesManifest {
            version: None,
            longform: "   ".into(),
            chapters: Vec::new(),
        };
        assert_eq!(blank.longform_title(), None);
    }
}
