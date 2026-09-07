//! The reflection extractor: read the corpus, propose better preambles.
//!
//! A typed extractor like [`super::titles`], so the proposal comes back
//! schema-validated. The preamble it writes is guidance only — the output shape
//! of every stage is enforced by its own `JsonSchema`, so a rewrite here cannot
//! break parsing downstream. That is what makes letting a model edit the
//! program's prompts survivable.

use anyhow::{bail, Result};

use crate::reflect::schema::Reflection;

use super::prompt;

const SYSTEM: &str = r#"You improve the system prompts of a video-to-social-media pipeline.

You are shown the prompts currently in force, what those prompts generated, and
what a human did next: kept it, edited it, or declined to post it. Where posts
have been published long enough to measure, you are shown how they performed.

An edit is the strongest signal you have. A human rewriting a caption is telling
you precisely how the prompt was wrong; a human keeping one is telling you it was
right. Read the direction of the edits, not just their presence.

Rules:
1. Recommend only what the evidence supports. If you are inferring from general
   knowledge rather than from something in this project, say so in `why` and
   leave `post_ids` empty rather than inventing them.
2. A rewritten preamble replaces the whole prompt, so it must stand alone: keep
   everything still working, change only what the evidence says to change, and do
   not shorten it into something vaguer.
3. Never describe an output format, JSON shape, or schema. That is enforced
   elsewhere. Write guidance about voice, structure, length and hooks.
4. Prefer no rewrite over a speculative one. An empty `rewrite` list is a valid
   and useful answer when the evidence is thin.
5. Quote the platform in `keep`/`drop` when the finding is platform-specific —
   what works on TikTok is not what works on LinkedIn."#;

/// Runs a reflection over `corpus`.
#[tracing::instrument(skip(corpus, prompt_root), fields(model, provider))]
pub fn extract_reflection(
    corpus: String,
    model: &str,
    provider: Option<&str>,
    prompt_root: Option<&std::path::Path>,
) -> Result<(Reflection, super::trace::LlmStep)> {
    if corpus.trim().is_empty() {
        bail!("nothing to reflect on");
    }
    let preamble = prompt::resolve(prompt::REFLECT, SYSTEM, prompt_root);
    let (reflection, step) = super::extract::extract::<Reflection>(
        prompt::REFLECT,
        "reflect",
        &preamble,
        corpus,
        model,
        provider,
    )?;
    Ok((sanitised(reflection), step))
}

/// Drops proposals that cannot be acted on.
///
/// A rewrite naming a prompt this program does not have is not a recommendation,
/// it is a typo that would write a file nothing reads. An empty preamble would
/// blank the prompt entirely.
fn sanitised(mut reflection: Reflection) -> Reflection {
    reflection.rewrite.retain(|rewrite| {
        let known = KNOWN_PROMPTS.contains(&rewrite.prompt_id.as_str());
        if !known {
            eprintln!(
                "stream-recorder: dropping rewrite for unknown prompt {:?}",
                rewrite.prompt_id
            );
        }
        known && !rewrite.preamble.trim().is_empty()
    });
    reflection
}

/// The prompts a reflection is allowed to touch.
///
/// `substack.notes` and `blog.article` are here even though `reflect::validate`
/// cannot generation-check them — that path already says so out loud for titles
/// and notes rather than implying a test that did not run, and a prompt nobody
/// may rewrite is a prompt the loop cannot improve.
const KNOWN_PROMPTS: [&str; 5] = [
    prompt::NOTES,
    prompt::TITLES,
    prompt::POSTS,
    prompt::SUBSTACK,
    prompt::BLOG,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflect::schema::Rewrite;

    fn rewrite(prompt_id: &str, preamble: &str) -> Rewrite {
        Rewrite {
            prompt_id: prompt_id.into(),
            preamble: preamble.into(),
            why: "because".into(),
            approved: false,
            validation: None,
        }
    }

    fn reflection(rewrites: Vec<Rewrite>) -> Reflection {
        Reflection {
            keep: Vec::new(),
            drop: Vec::new(),
            evidence: Vec::new(),
            rewrite: rewrites,
        }
    }

    #[test]
    fn a_rewrite_for_an_unknown_prompt_is_dropped() {
        let got = sanitised(reflection(vec![
            rewrite("posts.social", "guidance"),
            rewrite("posts.socail", "typo"),
            rewrite("schedule.plan", "not a thing we run"),
        ]));
        assert_eq!(got.rewrite.len(), 1);
        assert_eq!(got.rewrite[0].prompt_id, "posts.social");
    }

    #[test]
    fn an_empty_preamble_is_dropped_rather_than_blanking_the_prompt() {
        let got = sanitised(reflection(vec![
            rewrite("posts.social", "   "),
            rewrite("titles.chapter_cards", "real guidance"),
        ]));
        assert_eq!(got.rewrite.len(), 1);
        assert_eq!(got.rewrite[0].prompt_id, "titles.chapter_cards");
    }

    /// Named rather than left to the loop below, which reads the same constant
    /// it is checking: dropping either of these from that list would keep every
    /// other test green while quietly making that prompt un-improvable.
    #[test]
    fn the_substack_prompt_can_be_rewritten() {
        let got = sanitised(reflection(vec![rewrite("substack.notes", "beats, not prose")]));
        assert_eq!(got.rewrite.len(), 1);
        assert_eq!(got.rewrite[0].prompt_id, "substack.notes");
    }

    #[test]
    fn the_blog_prompt_can_be_rewritten() {
        let got = sanitised(reflection(vec![rewrite("blog.article", "prose, not beats")]));
        assert_eq!(got.rewrite.len(), 1);
        assert_eq!(got.rewrite[0].prompt_id, "blog.article");
    }

    #[test]
    fn every_known_prompt_is_accepted() {
        let got = sanitised(reflection(
            KNOWN_PROMPTS
                .iter()
                .map(|id| rewrite(id, "guidance"))
                .collect(),
        ));
        assert_eq!(got.rewrite.len(), KNOWN_PROMPTS.len());
    }

    /// The prompt must not teach the model to describe a schema — the whole point
    /// of moving the contract into the extractor.
    #[test]
    fn the_system_prompt_forbids_writing_a_schema() {
        assert!(SYSTEM.contains("Never describe an output format"));
        assert!(SYSTEM.contains("Prefer no rewrite over a speculative one"));
    }

    #[test]
    fn an_empty_corpus_is_refused_before_spending_a_call() {
        let err = extract_reflection("   ".into(), "m", None, None).unwrap_err();
        assert!(err.to_string().contains("nothing to reflect on"));
    }
}
