//! Running a proposed preamble for real before it becomes the program's behaviour.
//!
//! Every stage's output shape is enforced by a `JsonSchema`, so a rewrite can no
//! longer break parsing. What it can still do is produce copy nobody would send:
//! captions over a platform's character limit, empty bodies, or posts for
//! platforms that stage does not target. Structure is not usefulness, so this
//! generates against the project's own content and checks the result.
//!
//! One model call, on a deliberately small slice — validation should cost about
//! what one chapter costs, not a whole project.

use crate::posts::generate::{generate_posts, VideoContext};
use crate::reflect::schema::{Rewrite, Validation};
use crate::schedule::ledger::now_rfc3339;
use crate::session::Session;

/// Platform limits worth failing on. Buffer rejects some of these outright, and
/// the rest simply read as broken.
const LIMITS: [(&str, usize); 3] = [("twitter", 280), ("bluesky", 300), ("tiktok", 150)];

/// How many videos to generate against. Enough to exercise the prompt, small
/// enough that validating is cheap.
const SAMPLE: usize = 2;

pub fn run(
    session: &Session,
    rewrite: &Rewrite,
    model: &str,
    provider: Option<&str>,
) -> Validation {
    let checked_at = now_rfc3339();
    if rewrite.prompt_id != crate::agent::prompt::POSTS {
        // Only the posts prompt has a cheap, checkable output. A titles or notes
        // rewrite is applied on the strength of the review alone, and saying so
        // is better than implying it was tested.
        return Validation {
            ok: true,
            checked_at,
            detail: format!(
                "{} is not generation-checked — review the diff on its merits",
                rewrite.prompt_id
            ),
        };
    }

    let contexts = sample_contexts(session);
    if contexts.is_empty() {
        return Validation {
            ok: false,
            checked_at,
            detail: "no transcripts in this project to generate against".to_string(),
        };
    }

    match generate_with(&contexts, session, rewrite, model, provider) {
        Ok(report) => Validation {
            ok: report.ok,
            checked_at,
            detail: report.detail,
        },
        Err(err) => Validation {
            ok: false,
            checked_at,
            detail: format!("generation failed: {err:#}"),
        },
    }
}

struct Report {
    ok: bool,
    detail: String,
}

fn generate_with(
    contexts: &[VideoContext],
    session: &Session,
    rewrite: &Rewrite,
    model: &str,
    provider: Option<&str>,
) -> anyhow::Result<Report> {
    // The proposed preamble is passed directly rather than written to disk: a
    // validation must never change what the next real generation would use.
    let (manifest, _step) = generate_posts(
        contexts,
        &session.title(),
        session.version,
        model,
        provider,
        None,
        None,
        Some(&rewrite.preamble),
    )?;
    Ok(check(&manifest))
}

/// Judges a generated manifest. Returns the first reasons it is unusable.
fn check(manifest: &crate::posts::schema::PostsManifest) -> Report {
    let mut problems = Vec::new();
    let mut posts = 0;

    for video in &manifest.items {
        for post in &video.posts {
            posts += 1;
            let length = post.content.chars().count();
            if let Some((_, limit)) = LIMITS
                .iter()
                .find(|(platform, _)| *platform == post.platform)
            {
                if length > *limit {
                    problems.push(format!(
                        "{} · {} is {length} chars, over the {limit} limit",
                        video.video_id, post.platform
                    ));
                }
            }
            if post.content.trim().is_empty() {
                problems.push(format!("{} · {} has no body", video.video_id, post.platform));
            }
        }
    }

    if posts == 0 {
        return Report {
            ok: false,
            detail: "produced no posts at all".to_string(),
        };
    }
    if problems.is_empty() {
        return Report {
            ok: true,
            detail: format!("{posts} post(s), all within platform limits"),
        };
    }
    Report {
        ok: false,
        detail: format!(
            "{} problem(s) in {posts} post(s): {}",
            problems.len(),
            problems.join("; ")
        ),
    }
}

/// A couple of this project's real videos — the prompt should be judged on the
/// content it will actually face.
fn sample_contexts(session: &Session) -> Vec<VideoContext> {
    let notes = session
        .notes_dir()
        .ok()
        .and_then(|dir| crate::notes::load_notes(&dir).ok());
    let mut contexts =
        crate::posts::generate::collect_video_contexts(&session.dir, notes.as_ref());
    contexts.truncate(SAMPLE);
    contexts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::posts::schema::{PlatformPost, PostsManifest, VideoPosts};

    fn manifest(posts: Vec<PlatformPost>) -> PostsManifest {
        PostsManifest {
            version: Some(1),
            prompt_version: Some(1),
            prompt_hash: String::new(),
            items: vec![VideoPosts {
                video_id: "chapter-01".into(),
                video_type: "vertical".into(),
                video_path: None,
                posts,
            }],
        }
    }

    fn post(platform: &str, content: &str) -> PlatformPost {
        PlatformPost {
            platform: platform.into(),
            title: None,
            content: content.into(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn copy_within_every_limit_passes() {
        let report = check(&manifest(vec![
            post("twitter", "a tight hook"),
            post("linkedin", &"word ".repeat(200)),
        ]));
        assert!(report.ok, "linkedin has no limit here");
        assert!(report.detail.contains("2 post(s)"));
    }

    /// The case structural validation cannot catch: it parses, and it is unusable.
    #[test]
    fn copy_over_a_platform_limit_fails_with_the_number() {
        let report = check(&manifest(vec![post("twitter", &"x".repeat(400))]));
        assert!(!report.ok);
        assert!(report.detail.contains("400 chars"));
        assert!(report.detail.contains("280"));
    }

    #[test]
    fn tiktok_and_bluesky_have_their_own_limits() {
        assert!(!check(&manifest(vec![post("tiktok", &"x".repeat(200))])).ok);
        assert!(check(&manifest(vec![post("bluesky", &"x".repeat(280))])).ok);
        assert!(!check(&manifest(vec![post("bluesky", &"x".repeat(400))])).ok);
    }

    #[test]
    fn producing_nothing_is_a_failure_not_a_pass() {
        let report = check(&manifest(Vec::new()));
        assert!(!report.ok);
        assert!(report.detail.contains("no posts at all"));
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let report = check(&manifest(vec![
            post("twitter", &"x".repeat(400)),
            post("tiktok", &"y".repeat(400)),
        ]));
        assert!(!report.ok);
        assert!(report.detail.contains("2 problem(s)"));
        assert!(report.detail.contains("twitter"));
        assert!(report.detail.contains("tiktok"));
    }
}
