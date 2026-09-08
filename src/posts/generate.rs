//! Turn a project's transcripts and notes into per-platform social copy.
//!
//! The system prompt is the builtin below unless the project root carries a
//! `prompts/posts.social.txt` overlay — see [`crate::agent::prompt::resolve`].
//!
//! The output *shape* is deliberately not in that prompt. It is a `JsonSchema`
//! on [`PostsExtraction`], enforced by the extractor, so an overlay can change
//! the voice and the platform rules but cannot break parsing. Describing the
//! schema in prose — as this module used to — meant a rewritten preamble that
//! forgot to repeat it silently broke every future generation.

use std::path::Path;

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::schema::{PlatformPost, PostsManifest, VideoPosts};
use crate::notes::NotesData;

pub const SYSTEM_PROMPT: &str = r#"You are an expert social media copywriter and growth strategist for technical video content.
You turn recorded video projects and chapter transcripts into high-performing, platform-native social media posts.

You must generate tailored posts. Use only the platforms that match the video type:
- horizontal / longform: linkedin, facebook
- vertical / chapter: twitter, bluesky, instagram, facebook, youtube_shorts, tiktok

Platform rules:
1. "twitter": Max 280 characters. High-impact hook, 1-2 hashtags.
2. "bluesky": Max 300 characters. Direct, authentic thought.
3. "instagram": Engaging hook, line breaks, CTA, 3-5 hashtags.
4. "facebook": Conversational narrative, end with a question.
5. "youtube_shorts": Catchy title (<= 90 chars) and brief description including #Shorts.
6. The full video already has its own YouTube upload; do not generate another YouTube longform post.
7. "tiktok": Hook in the first line, caption <= 150 chars, 3-5 hashtags.
8. "linkedin": Professional insight, opening line, takeaway, 3-5 tags.

Use provided published video/article URLs exactly when relevant. Never invent a URL or promote a private video or draft article.
Write one entry per video, and only the platforms listed above for its type."#;

/// What the model returns. Separate from [`PostsManifest`] so the model is never
/// asked to fill in bookkeeping it knows nothing about — version, prompt hash,
/// local video paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct PostsExtraction {
    items: Vec<ExtractedVideo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedVideo {
    /// "longform" or "chapter-01".
    video_id: String,
    /// "horizontal" or "vertical".
    video_type: String,
    posts: Vec<ExtractedPost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct ExtractedPost {
    platform: String,
    title: Option<String>,
    content: String,
    tags: Vec<String>,
}

impl PostsExtraction {
    /// Drops posts with no body.
    ///
    /// The old hand-rolled parser defaulted a missing `content` to an empty
    /// string, so a truncated response produced captions that looked valid all
    /// the way through to the Schedule tab. An empty caption is a failed
    /// generation, not a post.
    fn into_manifest(
        self,
        version: Option<u32>,
        prompt: &crate::agent::prompt::Resolved,
    ) -> PostsManifest {
        let items = self
            .items
            .into_iter()
            .map(|video| VideoPosts {
                video_id: video.video_id,
                video_type: video.video_type,
                video_path: None,
                posts: video
                    .posts
                    .into_iter()
                    .filter(|post| !post.content.trim().is_empty())
                    .map(|post| PlatformPost {
                        platform: post.platform,
                        title: post.title.filter(|t| !t.trim().is_empty()),
                        content: post.content.trim().to_string(),
                        tags: post
                            .tags
                            .into_iter()
                            .map(|tag| tag.trim().trim_start_matches('#').to_string())
                            .filter(|tag| !tag.is_empty())
                            .collect(),
                    })
                    .collect(),
            })
            .filter(|video: &VideoPosts| !video.posts.is_empty())
            .collect();
        PostsManifest {
            version,
            prompt_version: prompt.version,
            prompt_hash: prompt.hash.clone(),
            items,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoContext {
    pub id: String,
    pub video_type: String, // "horizontal" or "vertical"
    pub title: String,
    pub points: Vec<String>,
    pub transcript_text: String,
}

pub fn generate_posts(
    videos: &[VideoContext],
    project_title: &str,
    version: Option<u32>,
    model: &str,
    provider: Option<&str>,
    custom_prompt: Option<&str>,
    prompt_root: Option<&std::path::Path>,
    // `preamble_override` runs a preamble *instead of* the resolved one without
    // writing it anywhere: validating a proposed rewrite must never change what
    // the next real generation would use.
    preamble_override: Option<&str>,
) -> Result<(PostsManifest, crate::agent::trace::LlmStep)> {
    if videos.is_empty() {
        bail!("no videos provided to generate posts for");
    }

    let system = match preamble_override {
        // An untried preamble has no version — recording one would claim a
        // provenance it does not have.
        Some(text) => crate::agent::prompt::Resolved {
            text: text.to_string(),
            version: None,
            hash: crate::agent::prompt::hash_of(text),
        },
        None => {
            crate::agent::prompt::resolve(crate::agent::prompt::POSTS, SYSTEM_PROMPT, prompt_root)
        }
    };
    let prompt = build_user_prompt(videos, project_title, custom_prompt);

    // Through the extractor, like notes and titles: the shape is enforced by the
    // schema rather than asked for in prose, and the LlmStep comes back with it —
    // which is how this step got traced at all.
    let (extracted, step) = crate::agent::extract::extract::<PostsExtraction>(
        crate::agent::prompt::POSTS,
        "posts",
        &system,
        prompt,
        model,
        provider,
    )?;

    let manifest = extracted.into_manifest(version, &system);
    if manifest.items.is_empty() {
        bail!("the model returned no usable posts");
    }
    Ok((manifest, step))
}

fn build_user_prompt(
    videos: &[VideoContext],
    project_title: &str,
    custom_prompt: Option<&str>,
) -> String {
    let mut out = format!("Project: {project_title}\n\n");
    if let Some(extra) = custom_prompt.filter(|p| !p.trim().is_empty()) {
        out.push_str(&format!("Author guidance/tone instructions:\n{extra}\n\n"));
    }

    out.push_str("Generate posts for the following videos:\n\n");
    for v in videos {
        out.push_str(&format!("--- Video ID: {} ({}) ---\n", v.id, v.video_type));
        out.push_str(&format!("Topic/Title: {}\n", v.title));
        if !v.points.is_empty() {
            out.push_str("Key points:\n");
            for p in &v.points {
                out.push_str(&format!("- {}\n", p));
            }
        }
        if !v.transcript_text.trim().is_empty() {
            out.push_str(&format!("Transcript:\n{}\n", v.transcript_text.trim()));
        }
        out.push('\n');
    }
    out
}

/// Every video the copywriter should be given: the longform, then one per closed
/// chapter.
///
/// Deliberately not gated on the rendered files. Copy is written from the notes
/// and the transcript, and both exist the moment a chapter closes — the rendered
/// file only matters later, when there has to be something to upload. Gating
/// here is what made generating posts before a render yield chapters and *no
/// longform at all*, which left the Schedule tab with no longform row to show
/// however far the render got afterwards. A video that has no public URL yet is
/// already handled where it belongs, by the planner's "no distributed url — run
/// Distribute" skip.
pub fn collect_video_contexts(session_dir: &Path, notes: Option<&NotesData>) -> Vec<VideoContext> {
    let chapters = chapter_contexts(session_dir, notes);
    if chapters.is_empty() {
        return chapters;
    }
    // The longform is those chapters end to end, so it exists exactly when they
    // do — and it is first because it is the video the others are cut from.
    let mut out = Vec::with_capacity(chapters.len() + 1);
    out.push(longform_context(session_dir, notes));
    out.extend(chapters);
    out
}

fn longform_context(session_dir: &Path, notes: Option<&NotesData>) -> VideoContext {
    VideoContext {
        id: "longform".into(),
        video_type: "horizontal".into(),
        title: notes
            .map(|n| n.title.clone())
            .unwrap_or_else(|| "Full Video".into()),
        points: notes
            .map(|n| n.chapters.iter().flat_map(|c| c.points.clone()).collect())
            .unwrap_or_default(),
        transcript_text: crate::notes::collect_completed(session_dir)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn chapter_contexts(session_dir: &Path, notes: Option<&NotesData>) -> Vec<VideoContext> {
    crate::notes::closed_chapter_numbers(session_dir)
        .into_iter()
        .map(|n| {
            let ch_notes = notes.and_then(|d| d.chapters.get(n.saturating_sub(1) as usize));
            let transcript_path = session_dir.join(format!("chapter-{n:02}.transcript.json"));
            VideoContext {
                id: format!("chapter-{n:02}"),
                video_type: "vertical".into(),
                title: ch_notes
                    .map(|c| c.title.clone())
                    .unwrap_or_else(|| format!("Chapter {n}")),
                points: ch_notes.map(|c| c.points.clone()).unwrap_or_default(),
                transcript_text: std::fs::read_to_string(transcript_path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<crate::notes::ChapterTranscript>(&t).ok())
                    .map(|t| t.text)
                    .unwrap_or_default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extracted(platform: &str, content: &str) -> ExtractedPost {
        ExtractedPost {
            platform: platform.into(),
            title: None,
            content: content.into(),
            tags: vec!["#rust".into(), " agents ".into(), "  ".into()],
        }
    }

    fn extraction(posts: Vec<ExtractedPost>) -> PostsExtraction {
        PostsExtraction {
            items: vec![ExtractedVideo {
                video_id: "chapter-01".into(),
                video_type: "vertical".into(),
                posts,
            }],
        }
    }

    fn resolved() -> crate::agent::prompt::Resolved {
        crate::agent::prompt::Resolved {
            text: "guidance".into(),
            version: Some(2),
            hash: "feedfacefeedface".into(),
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-contexts-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes what a closed chapter leaves behind: the recording, and the
    /// transcript that lands beside it.
    fn closed_chapter(dir: &Path, n: u32, text: &str) {
        std::fs::write(dir.join(format!("chapter-{n:02}.mp4")), b"vid").unwrap();
        std::fs::write(
            dir.join(format!("chapter-{n:02}.transcript.json")),
            format!(r#"{{"status":"completed","text":"{text}","words":[]}}"#),
        )
        .unwrap();
    }

    /// The regression: posts generated before the render produced chapters and no
    /// longform, so the Schedule tab had no longform row to build — and running
    /// the render afterwards could not add one, because the copy was already
    /// written without it.
    #[test]
    fn the_longform_is_collected_before_anything_is_rendered() {
        let dir = temp("unrendered");
        closed_chapter(&dir, 1, "first chapter");
        closed_chapter(&dir, 2, "second chapter");
        let contexts = collect_video_contexts(&dir, None);
        let ids: Vec<&str> = contexts.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["longform", "chapter-01", "chapter-02"]);
        assert_eq!(contexts[0].video_type, "horizontal");
        assert!(contexts[1..].iter().all(|c| c.video_type == "vertical"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The longform's copy is written from every chapter's words at once, which
    /// is what makes it a different post rather than the first chapter's again.
    #[test]
    fn the_longform_carries_every_chapters_transcript() {
        let dir = temp("transcript");
        closed_chapter(&dir, 1, "alpha");
        closed_chapter(&dir, 2, "beta");
        let contexts = collect_video_contexts(&dir, None);
        assert!(contexts[0].transcript_text.contains("alpha"));
        assert!(contexts[0].transcript_text.contains("beta"));
        assert_eq!(contexts[1].transcript_text, "alpha");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing recorded is nothing to write about — and no phantom longform for a
    /// project with no chapters under it.
    #[test]
    fn an_empty_project_yields_no_contexts() {
        let dir = temp("empty");
        assert!(collect_video_contexts(&dir, None).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The schema is the extractor's job now, so the prompt must not also try to
    /// specify it — that duplication is what an overlay could break.
    #[test]
    fn the_builtin_prompt_no_longer_carries_the_output_schema() {
        assert!(!SYSTEM_PROMPT.contains("Return JSON only"));
        assert!(!SYSTEM_PROMPT.contains("\"items\""));
        // The guidance it exists for is still there.
        assert!(SYSTEM_PROMPT.contains("Max 280 characters"));
    }

    /// A truncated response used to yield captions with empty bodies that looked
    /// valid all the way to the Schedule tab.
    #[test]
    fn a_post_with_no_body_is_dropped() {
        let manifest = extraction(vec![
            extracted("twitter", "a real hook"),
            extracted("bluesky", "   "),
        ])
        .into_manifest(Some(3), &resolved());
        assert_eq!(manifest.items.len(), 1);
        assert_eq!(manifest.items[0].posts.len(), 1);
        assert_eq!(manifest.items[0].posts[0].platform, "twitter");
    }

    #[test]
    fn a_video_left_with_no_posts_is_dropped_entirely() {
        let manifest =
            extraction(vec![extracted("twitter", "")]).into_manifest(Some(3), &resolved());
        assert!(manifest.items.is_empty());
    }

    #[test]
    fn the_manifest_records_which_prompt_version_wrote_it() {
        let manifest =
            extraction(vec![extracted("twitter", "hook")]).into_manifest(Some(3), &resolved());
        assert_eq!(manifest.version, Some(3));
        assert_eq!(manifest.prompt_version, Some(2));
        assert_eq!(manifest.prompt_hash, "feedfacefeedface");
    }

    #[test]
    fn tags_are_normalised_and_blanks_dropped() {
        let manifest =
            extraction(vec![extracted("twitter", "hook")]).into_manifest(Some(3), &resolved());
        assert_eq!(manifest.items[0].posts[0].tags, vec!["rust", "agents"]);
    }

    #[test]
    fn prompt_overlay_replaces_the_builtin_system_prompt() {
        use crate::agent::prompt;

        let dir =
            std::env::temp_dir().join(format!("stream-recorder-posts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("prompts")).unwrap();
        std::fs::write(
            dir.join("prompts").join("posts.social.txt"),
            " write like a pirate \n",
        )
        .unwrap();
        // Through `resolve_in` with no library, so a real house prompt in
        // `~/.stream-recorder/prompts/` cannot decide what this asserts.
        assert_eq!(
            prompt::resolve_in(prompt::POSTS, SYSTEM_PROMPT, Some(&dir), None).text,
            "write like a pirate"
        );
        assert_eq!(
            prompt::resolve_in(prompt::POSTS, SYSTEM_PROMPT, None, None).text,
            SYSTEM_PROMPT
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_prompt_lands_in_the_user_prompt_not_the_system_one() {
        let videos = vec![VideoContext {
            id: "chapter-01".into(),
            video_type: "vertical".into(),
            title: "Rust agents".into(),
            points: vec!["a point".into()],
            transcript_text: "spoken words".into(),
        }];
        let user = build_user_prompt(&videos, "vd-42-demo", Some("keep it dry"));
        assert!(user.contains("Project: vd-42-demo"));
        assert!(user.contains("keep it dry"));
        assert!(user.contains("--- Video ID: chapter-01 (vertical) ---"));
        assert!(user.contains("- a point"));
        assert!(user.contains("spoken words"));
        assert!(!build_user_prompt(&videos, "p", None).contains("Author guidance"));
    }
}
