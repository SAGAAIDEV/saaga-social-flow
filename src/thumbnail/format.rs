//! Shared output geometry for local cards and generated thumbnails.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Horizontal,
    Vertical,
}

impl Format {
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Horizontal => (1280, 720),
            Self::Vertical => (720, 1280),
        }
    }
    pub fn aspect_ratio(self) -> &'static str {
        match self {
            Self::Horizontal => "16:9",
            Self::Vertical => "9:16",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_and_provider_agree_in_both_formats() {
        let brief = crate::thumbnail::Brief {
            title: "Hello".into(),
            description: String::new(),
        };
        for format in [Format::Horizontal, Format::Vertical] {
            let prompt = brief.render_for(format);
            let body = crate::thumbnail::image::request_body_for(
                "model",
                &prompt,
                b"jpeg",
                None,
                &[],
                format,
            );
            assert!(prompt.contains(format.aspect_ratio()));
            assert_eq!(body["image_config"]["aspect_ratio"], format.aspect_ratio());
        }
        assert_ne!(
            brief.render_for(Format::Horizontal),
            brief.render_for(Format::Vertical)
        );
    }
}
