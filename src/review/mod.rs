//! Watching the renders before they are uploaded.
//!
//! The stage between making a video and publishing it, which had nothing to look
//! at: Render reported paths and byte counts, Distribute uploaded them, and the
//! first time anyone saw the result was on a social network. Every rendering bug
//! this project has had — a vertical that went black at ten seconds, a longform
//! that opened on three seconds of nothing — was visible in the first frames and
//! invisible in the summary.
//!
//! A `<video controls>` per clip, which is scrubbing, playback and a duration
//! readout for free, and reads the same files Distribute is about to upload.

use std::path::Path;

use serde::Serialize;

/// Percent-encoded, and shared with every other pane that shows a file: a real path
/// can hold a space, and a space ends a URL.
use crate::ui::file_url;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pane {
    pub clips: Vec<Clip>,
    /// Why there is nothing to watch, when there is nothing.
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Clip {
    /// "longform" | "chapter-01" — the id Distribute uploads it under.
    pub id: String,
    pub label: String,
    pub url: String,
    /// "landscape" | "portrait", so the player is shaped like its video.
    pub orientation: String,
    pub megabytes: f64,
}

/// Everything a render produced, in the order the longform plays.
///
/// The same files, found the same way, as [`crate::distribute`] collects — so
/// what is watched here is what goes out, rather than a parallel idea of it.
pub fn build(render_dir: &Path) -> Pane {
    let mut clips = Vec::new();
    let longform = render_dir.join("horizontal/longform.mp4");
    if longform.is_file() {
        clips.push(clip("longform", "Longform", &longform, "landscape"));
    }
    for n in 1..=99 {
        let path = render_dir.join(format!("vertical/chapter-{n:02}.mp4"));
        if path.is_file() {
            clips.push(clip(
                &format!("chapter-{n:02}"),
                &format!("Chapter {n:02}"),
                &path,
                "portrait",
            ));
        }
    }
    let blocked = clips
        .is_empty()
        .then(|| format!("Nothing rendered yet — run Render. ({})", render_dir.display()));
    Pane { clips, blocked }
}

fn clip(id: &str, label: &str, path: &Path, orientation: &str) -> Clip {
    Clip {
        id: id.to_string(),
        label: label.to_string(),
        url: file_url(path),
        orientation: orientation.to_string(),
        megabytes: path
            .metadata()
            .map(|meta| meta.len() as f64 / (1024.0 * 1024.0))
            .unwrap_or(0.0),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-review-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(path: PathBuf, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn nothing_rendered_says_so_and_names_the_folder() {
        let render = temp("empty");
        let pane = build(&render);
        assert!(pane.clips.is_empty());
        assert!(pane.blocked.unwrap().contains("run Render"));
    }

    /// Longform first, then chapters ascending — the order the video plays in.
    #[test]
    fn clips_are_listed_in_playing_order() {
        let render = temp("order");
        write(render.join("vertical/chapter-02.mp4"), b"two");
        write(render.join("vertical/chapter-01.mp4"), b"one");
        write(render.join("horizontal/longform.mp4"), b"long");

        let pane = build(&render);
        let ids: Vec<&str> = pane.clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["longform", "chapter-01", "chapter-02"]);
        assert!(pane.blocked.is_none());
        let _ = std::fs::remove_dir_all(&render);
    }

    /// The player is shaped like its video, so a vertical is not letterboxed into
    /// a widescreen frame that hides what the edges are doing.
    #[test]
    fn each_clip_carries_its_orientation_and_size() {
        let render = temp("shape");
        write(render.join("horizontal/longform.mp4"), &vec![0u8; 2 * 1024 * 1024]);
        write(render.join("vertical/chapter-01.mp4"), b"x");

        let pane = build(&render);
        assert_eq!(pane.clips[0].orientation, "landscape");
        assert!((pane.clips[0].megabytes - 2.0).abs() < 0.01);
        assert_eq!(pane.clips[1].orientation, "portrait");
        assert!(pane.clips[0].url.starts_with("file:///"));
        let _ = std::fs::remove_dir_all(&render);
    }

    /// The ids are what Distribute uploads under, so the tab and the upload agree
    /// on which clip is which.
    #[test]
    fn the_ids_match_what_distribute_calls_them() {
        let render = temp("ids");
        write(render.join("horizontal/longform.mp4"), b"long");
        write(render.join("vertical/chapter-04.mp4"), b"four");
        let pane = build(&render);
        assert_eq!(pane.clips[0].id, "longform");
        assert_eq!(pane.clips[1].id, "chapter-04");
        let _ = std::fs::remove_dir_all(&render);
    }
}
