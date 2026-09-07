//! Everything a reflection reads, reduced to what is worth reading.
//!
//! The naive version of this — hand the model `llm.jsonl` — does not fit and
//! would not help if it did. That file's bulk is *user prompts*, which are almost
//! entirely transcripts: the largest input and the least useful, since the outputs
//! already reflect them. So transcripts are summarised to a line and the budget is
//! spent on the things that actually carry signal:
//!
//! - the **preambles** in force, which are what a rewrite would change
//! - **deltas** — what the model wrote versus what a human kept. An unchanged
//!   caption says the prompt was fine; an edited one says exactly how it was not.
//! - **decisions** — plan items a human declined to approve
//! - **performance**, when any post has matured
//!
//! An unchanged item costs one line here, not two copies of itself.

use crate::analytics::schema::AnalyticsRow;
use crate::posts::schema::PostsManifest;
use crate::reflect::schema::Inputs;
use crate::schedule::schema::SchedulePlan;
use crate::titles::schema::TitlesManifest;

/// How much of a caption to show when it changed. Long enough to judge a hook.
const EXCERPT: usize = 240;

/// One generated-vs-final pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub label: String,
    pub generated: String,
    pub final_text: String,
}

impl Delta {
    pub fn changed(&self) -> bool {
        self.generated.trim() != self.final_text.trim()
    }
}

/// The reduced evidence a reflection runs on.
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    pub project: String,
    /// (prompt_id, version label, preamble)
    pub prompts: Vec<(String, String, String)>,
    pub titles: Vec<Delta>,
    pub captions: Vec<Delta>,
    /// Plan items a human left unapproved, with the reason they exist.
    pub declined: Vec<String>,
    /// Measured posts: label, window, and the metrics that came back.
    pub measured: Vec<(String, String, Vec<(String, f64)>)>,
    pub llm_steps: usize,
}

impl Corpus {
    pub fn inputs(&self) -> Inputs {
        Inputs {
            llm_steps: self.llm_steps,
            titles_edited: self.titles.iter().filter(|d| d.changed()).count(),
            captions_edited: self.captions.iter().filter(|d| d.changed()).count(),
            declined: self.declined.len(),
            measured_posts: self.measured.len(),
        }
    }

    /// The user prompt handed to the extractor.
    pub fn render(&self) -> String {
        let mut out = format!("Project: {}\n", self.project);

        out.push_str("\n## Prompts currently in force\n");
        for (id, version, text) in &self.prompts {
            out.push_str(&format!("\n### {id} ({version})\n{}\n", text.trim()));
        }

        push_deltas(&mut out, "Chapter titles — generated vs kept", &self.titles);
        push_deltas(&mut out, "Captions — generated vs sent", &self.captions);

        if !self.declined.is_empty() {
            out.push_str("\n## Planned but not approved\n");
            for item in &self.declined {
                out.push_str(&format!("- {item}\n"));
            }
        }

        if !self.measured.is_empty() {
            out.push_str("\n## Performance\n");
            for (label, window, metrics) in &self.measured {
                let numbers: Vec<String> = metrics
                    .iter()
                    .map(|(name, value)| format!("{name} {value:.0}"))
                    .collect();
                out.push_str(&format!("- {label} ({window}): {}\n", numbers.join(", ")));
            }
        }

        out
    }
}

fn push_deltas(out: &mut String, heading: &str, deltas: &[Delta]) {
    if deltas.is_empty() {
        return;
    }
    let changed = deltas.iter().filter(|d| d.changed()).count();
    out.push_str(&format!(
        "\n## {heading}\n{} of {} were edited by hand.\n\n",
        changed,
        deltas.len()
    ));
    for delta in deltas {
        if !delta.changed() {
            // An unchanged item is one line: the prompt got it right, and repeating
            // the text twice would spend budget saying so.
            out.push_str(&format!("- {} — kept as written\n", delta.label));
            continue;
        }
        out.push_str(&format!(
            "- {} — EDITED\n    generated: {}\n    final:     {}\n",
            delta.label,
            excerpt(&delta.generated),
            excerpt(&delta.final_text)
        ));
    }
}

fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT {
        return flat;
    }
    let cut: String = flat.chars().take(EXCERPT).collect();
    format!("{cut}…")
}

/// Pairs generated titles against what was kept.
pub fn title_deltas(generated: &serde_json::Value, final_titles: &TitlesManifest) -> Vec<Delta> {
    let from_model = generated
        .get("chapters")
        .and_then(|c| c.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let n = item.get("n")?.as_u64()? as u32;
                    let title = item.get("title")?.as_str()?.to_string();
                    Some((n, title))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    final_titles
        .chapters
        .iter()
        .filter_map(|chapter| {
            let generated = from_model
                .iter()
                .find(|(n, _)| *n == chapter.n)
                .map(|(_, title)| title.clone())?;
            Some(Delta {
                label: format!("chapter-{:02}", chapter.n),
                generated,
                final_text: chapter.title.clone(),
            })
        })
        .collect()
}

/// Pairs generated captions against what was saved, per (video, platform).
pub fn caption_deltas(generated: &serde_json::Value, final_posts: &PostsManifest) -> Vec<Delta> {
    let mut out = Vec::new();
    let Some(items) = generated.get("items").and_then(|i| i.as_array()) else {
        return out;
    };
    for video in &final_posts.items {
        for post in &video.posts {
            let from_model = items
                .iter()
                .find(|item| {
                    item.get("video_id").and_then(|v| v.as_str()) == Some(&video.video_id)
                })
                .and_then(|item| item.get("posts")?.as_array())
                .and_then(|posts| {
                    posts.iter().find(|entry| {
                        entry.get("platform").and_then(|p| p.as_str()) == Some(&post.platform)
                    })
                })
                .and_then(|entry| entry.get("content")?.as_str())
                .map(str::to_string);
            let Some(generated) = from_model else { continue };
            out.push(Delta {
                label: format!("{} · {}", video.video_id, post.platform),
                generated,
                final_text: post.content.clone(),
            });
        }
    }
    out
}

/// Items a human left unticked — a quieter signal than an edit, and a real one.
pub fn declined(plan: &SchedulePlan) -> Vec<String> {
    plan.queueable()
        .filter(|item| !item.approved)
        .map(|item| format!("{} · {} — {}", item.video_id, item.platform, item.reason))
        .collect()
}

/// Measured posts, flattened to (label, window, metrics).
pub fn measured(rows: &[AnalyticsRow]) -> Vec<(String, String, Vec<(String, f64)>)> {
    rows.iter()
        .filter(|row| !row.metrics.is_empty())
        .map(|row| {
            (
                format!("{} · {}", row.video_id, row.platform),
                row.window.clone(),
                row.metrics
                    .iter()
                    .map(|metric| (metric.name.clone(), metric.value))
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::posts::schema::{PlatformPost, VideoPosts};
    use crate::titles::schema::ChapterTitle;

    fn delta(label: &str, generated: &str, final_text: &str) -> Delta {
        Delta {
            label: label.into(),
            generated: generated.into(),
            final_text: final_text.into(),
        }
    }

    #[test]
    fn a_delta_knows_whether_a_human_touched_it() {
        assert!(!delta("a", "same", " same ").changed(), "whitespace is not an edit");
        assert!(delta("a", "before", "after").changed());
    }

    /// The budget rule: an unchanged item must not cost two copies of itself.
    #[test]
    fn unchanged_items_render_as_one_line() {
        let corpus = Corpus {
            project: "vd-42".into(),
            titles: vec![
                delta("chapter-01", "The Hook", "The Hook"),
                delta("chapter-02", "A Long Rambling Title", "The Fix"),
            ],
            ..Corpus::default()
        };
        let out = corpus.render();
        assert!(out.contains("chapter-01 — kept as written"));
        assert!(!out.contains("generated: The Hook"));
        assert!(out.contains("chapter-02 — EDITED"));
        assert!(out.contains("generated: A Long Rambling Title"));
        assert!(out.contains("final:     The Fix"));
        assert!(out.contains("1 of 2 were edited by hand"));
    }

    #[test]
    fn a_long_caption_is_excerpted_not_dumped() {
        let long = "word ".repeat(200);
        let out = excerpt(&long);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= EXCERPT + 1);
    }

    #[test]
    fn inputs_count_only_what_changed() {
        let corpus = Corpus {
            titles: vec![
                delta("a", "x", "x"),
                delta("b", "x", "y"),
            ],
            captions: vec![delta("c", "x", "y")],
            declined: vec!["chapter-02 · tiktok — vertical chapter".into()],
            llm_steps: 3,
            ..Corpus::default()
        };
        let inputs = corpus.inputs();
        assert_eq!(inputs.titles_edited, 1, "the unchanged one is not signal");
        assert_eq!(inputs.captions_edited, 1);
        assert_eq!(inputs.declined, 1);
        assert_eq!(inputs.llm_steps, 3);
        assert!(!inputs.is_empty());
    }

    #[test]
    fn title_deltas_pair_on_chapter_number() {
        let generated = serde_json::json!({
            "chapters": [{"n": 1, "title": "Generated One"}, {"n": 2, "title": "Generated Two"}]
        });
        let final_titles = TitlesManifest {
            longform: String::new(),
            version: Some(1),
            chapters: vec![
                ChapterTitle { n: 1, title: "Generated One".into(), approved: true },
                ChapterTitle { n: 2, title: "Human Two".into(), approved: true },
            ],
        };
        let deltas = title_deltas(&generated, &final_titles);
        assert_eq!(deltas.len(), 2);
        assert!(!deltas[0].changed());
        assert!(deltas[1].changed());
        assert_eq!(deltas[1].final_text, "Human Two");
    }

    #[test]
    fn caption_deltas_pair_on_video_and_platform() {
        let generated = serde_json::json!({
            "items": [{
                "video_id": "chapter-01",
                "posts": [
                    {"platform": "twitter", "content": "model hook"},
                    {"platform": "tiktok", "content": "model tiktok"}
                ]
            }]
        });
        let final_posts = PostsManifest {
            version: Some(1),
            prompt_version: Some(0),
            prompt_hash: String::new(),
            items: vec![VideoPosts {
                video_id: "chapter-01".into(),
                video_type: "vertical".into(),
                video_path: None,
                posts: vec![
                    PlatformPost {
                        platform: "twitter".into(),
                        title: None,
                        content: "human hook".into(),
                        tags: Vec::new(),
                    },
                    PlatformPost {
                        platform: "tiktok".into(),
                        title: None,
                        content: "model tiktok".into(),
                        tags: Vec::new(),
                    },
                ],
            }],
        };
        let deltas = caption_deltas(&generated, &final_posts);
        assert_eq!(deltas.len(), 2);
        let twitter = deltas.iter().find(|d| d.label.contains("twitter")).unwrap();
        assert!(twitter.changed());
        assert_eq!(twitter.generated, "model hook");
        let tiktok = deltas.iter().find(|d| d.label.contains("tiktok")).unwrap();
        assert!(!tiktok.changed());
    }

    /// A caption with no matching generated text cannot be a delta — reporting it
    /// as an edit from nothing would invent a signal.
    #[test]
    fn a_caption_with_no_generated_counterpart_is_skipped() {
        let generated = serde_json::json!({ "items": [] });
        let final_posts = PostsManifest {
            version: None,
            prompt_version: None,
            prompt_hash: String::new(),
            items: vec![VideoPosts {
                video_id: "chapter-01".into(),
                video_type: "vertical".into(),
                video_path: None,
                posts: vec![PlatformPost {
                    platform: "twitter".into(),
                    title: None,
                    content: "hand written".into(),
                    tags: Vec::new(),
                }],
            }],
        };
        assert!(caption_deltas(&generated, &final_posts).is_empty());
    }

    #[test]
    fn the_prompts_in_force_lead_the_corpus() {
        let corpus = Corpus {
            project: "vd-42".into(),
            prompts: vec![(
                "posts.social".into(),
                "v2".into(),
                "Write short hooks.".into(),
            )],
            ..Corpus::default()
        };
        let out = corpus.render();
        assert!(out.contains("### posts.social (v2)"));
        assert!(out.contains("Write short hooks."));
    }
}
