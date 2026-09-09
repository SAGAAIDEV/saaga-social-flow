//! The exact JSON the CMS receives, built without touching the network.
//!
//! Separated from [`super::strapi`] so the shape can be tested hard: every field
//! name here is validated on Strapi's side, and a typo in one of them is a 400
//! after the thumbnail has already been uploaded. `__component`, `textBodyHtml`,
//! `quoteTextHighlighted` and the rest are the CMS's names, and this is the only
//! file that should know them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::chapters::{Transcript, VideoChapter};
use super::schema::{Article, Block};

/// The body of the create request, written out beside `article.json`.
///
/// The article is the *intermediate* shape and deliberately CMS-free, so it can
/// never answer "what did we actually send". This file can: it is the exact
/// bytes of the POST, which is what `npm run content:validate` in the landing
/// repo wants and what a post that came out wrong has to be read back from.
pub const VIDEO_POST_JSON: &str = "video-post.json";

/// Writes the request body to `{blog_dir}/video-post.json`.
///
/// Called before the create rather than after it, and that ordering is the
/// point: a body Strapi rejects is the one worth having on disk, and a 400
/// after the thumbnail upload otherwise leaves nothing to look at.
pub fn save(dir: &Path, body: &Value) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(VIDEO_POST_JSON);
    let json = serde_json::to_string_pretty(body).context("serializing the video post")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Everything needed to create one `video-post`, with the parts nobody can
/// generate — the upload id, the probed duration, the live video URL — supplied
/// by the caller.
#[derive(Debug, Clone)]
pub struct NewVideoPost {
    pub article: Article,
    /// `YYYY-MM-DD`; the field is a Strapi `date`, not a datetime.
    pub date: String,
    pub video_url: String,
    /// YouTube's 11-character id, when the video is on YouTube.
    ///
    /// Carried beside the URL rather than parsed back out of it: the upload
    /// ledger already knows it, and a regex over a URL is exactly the fragile
    /// path this field exists to stop relying on.
    pub video_id: Option<String>,
    pub duration: u32,
    /// The uploaded poster. `None` only in a dry run, where nothing has been
    /// uploaded yet — the collection requires it for a real create.
    pub thumbnail_id: Option<i64>,
    pub thumbnail_vertical_id: Option<i64>,
    /// The portrait cut, when the project has one. `None` is the common case
    /// and publishes a page that renders one player at every width.
    pub vertical: Option<VerticalCut>,
    /// The reviewed 1200×630 OG artwork.
    pub og_image_id: Option<i64>,
    /// The standing call to action, when one is configured. Not per-article:
    /// see [`MagicLinkCta`].
    pub cta: Option<MagicLinkCta>,
    pub chapters: Vec<VideoChapter>,
    pub transcript: Option<Transcript>,
    pub author_id: Option<i64>,
    pub category_id: Option<i64>,
    /// The uploaded figures, by their number in `figures.jsonl`. A `Figure`
    /// block whose number is missing here is dropped rather than published: a
    /// `content.image` with no `src` renders as a broken image, and one empty
    /// box in a live article is worse than one missing figure.
    pub figures: std::collections::BTreeMap<u32, FigureMedia>,
    /// The built components, by step id, exactly as the component stage
    /// validated them — `__component`, `componentKey`, `componentVersion`,
    /// `heading` and `config` all included.
    ///
    /// Passed through untouched and never constructed here. The catalog that
    /// says which four `componentKey`s exist, and the schema each one's config
    /// has to satisfy, both live in the landing repo; a block assembled on this
    /// side would be one that never met either. An unresolved id is dropped for
    /// the reason a figure is: an unrecognised key does not error in the CMS, it
    /// silently blanks that one block on a permanent public page.
    pub embeds: std::collections::BTreeMap<String, Value>,
}

/// The inline call-to-action card.
///
/// Configured rather than generated, and standing rather than per-article. The
/// `url` has to be a real place, which rules out a model writing it; and what
/// the CTA points at is a marketing decision that is the same across a run of
/// videos, which is what [`crate::config`] is for. `url` is the only part the
/// component requires.
#[derive(Debug, Clone, PartialEq)]
pub struct MagicLinkCta {
    pub url: String,
    pub label: String,
    pub description: String,
    /// An uploaded image, when one is configured.
    pub image_id: Option<i64>,
}

impl MagicLinkCta {
    fn value(&self) -> Value {
        let mut cta = json!({ "url": self.url });
        let object = cta.as_object_mut().expect("a json object");
        for (field, text) in [("label", &self.label), ("description", &self.description)] {
            if !text.is_empty() {
                object.insert(field.into(), json!(text));
            }
        }
        if let Some(id) = self.image_id {
            object.insert("image".into(), json!(id));
        }
        cta
    }
}

/// The portrait cut, already up somewhere.
///
/// Carries no poster. `thumbnailVertical` is a post-level field on
/// [`NewVideoPost`] and comes out of the artwork set, which renders the
/// portrait poster as a designed 720×1280 rather than as a property of this
/// video — so holding a second id here only created two writers for one field,
/// and this one wrote last.
#[derive(Debug, Clone, PartialEq)]
pub struct VerticalCut {
    /// Absolute URL: the Short's page on YouTube, or the file the CMS hosts.
    pub url: String,
    /// YouTube's id when the cut went up as a Short — see
    /// [`crate::publish::short`] — which the page embeds the way it embeds the
    /// landscape video. `None` for a file uploaded into the media library.
    pub video_id: Option<String>,
}

/// One uploaded figure, as the CMS needs it.
///
/// Assembled by the caller because none of it can be derived here: the `src`
/// only exists after the upload, and the caption and dimensions come out of the
/// figure ledger rather than out of the article — see
/// [`Block::Figure`](super::schema::Block::Figure) for why the article holds
/// only a number.
#[derive(Debug, Clone, PartialEq)]
pub struct FigureMedia {
    /// Absolute URL of the uploaded file. Absolute, not the `/uploads/...` path
    /// the upload endpoint answers with: the mapper on the site only rewrites a
    /// relative URL when it came from a *media relation*, and this block carries
    /// a plain string, so a relative `src` would 404 on the live page.
    pub src: String,
    pub alt: String,
    pub caption: String,
    /// The file's real pixel size. Without it `next/image` lays every figure out
    /// at the featured image's 1504x612 and a 4:3 screenshot renders squashed.
    pub width: u32,
    pub height: u32,
}

/// The relations this asks Strapi to echo back after a create.
///
/// Without `populate` the response omits relations even when they were set,
/// which makes a silent drop invisible — see [`super::strapi`], which retries
/// each one that comes back null.
///
/// `educationCategory` was the third of these and is gone: the
/// `education-categories` collection was retired, `category` is the one
/// taxonomy a video post carries now, and `/education/{slug}` is a permanent
/// redirect to `/blog/{slug}`.
pub const RELATIONS: [&str; 2] = ["author", "category"];

impl NewVideoPost {
    pub fn body(&self) -> Value {
        let mut data = json!({
            "title": self.article.title,
            "slug": self.article.slug,
            "date": self.date,
            "shortAndMetaDescription": self.article.description,
            // The field is `min: 1`, so a video that probed as zero seconds would
            // be rejected outright rather than published as a stub.
            "duration": self.duration.max(1),
            "video": self.video(),
            "content": zone(&self.article.blocks, &self.figures, &self.embeds),
            // Pinning the featured slot is an editorial decision, so the
            // pipeline never claims it.
            "isFeatured": false,
        });

        let object = data.as_object_mut().expect("a json object");
        if let Some(id) = self.thumbnail_id {
            object.insert("thumbnail".into(), json!(id));
        }
        // Omitted rather than sent equal to the title: the site already falls
        // back, and an `h1` written into the CMS is a second string that has to
        // be re-edited every time the title is.
        if !self.article.h1.is_empty() {
            object.insert("h1".into(), json!(self.article.h1));
        }
        if !self.article.faq.is_empty() {
            object.insert("faq".into(), faq(&self.article.faq));
        }
        if let Some(id) = self.thumbnail_vertical_id {
            object.insert("thumbnailVertical".into(), json!(id));
        }
        if let Some(id) = self.og_image_id {
            object.insert("ogImage".into(), json!(id));
        }
        if !self.article.long_description.is_empty() {
            object.insert("description".into(), json!(self.article.long_description));
        }
        // Both default off, and both are omitted rather than sent as their
        // defaults: `noIndex: false` and `canonicalUrl: ""` are values, and
        // writing them would overwrite a decision someone made in the admin.
        if self.article.no_index {
            object.insert("noIndex".into(), json!(true));
        }
        if !self.article.canonical_url.is_empty() {
            object.insert("canonicalUrl".into(), json!(self.article.canonical_url));
        }
        if let Some(cta) = &self.cta {
            object.insert("magicLinkCta".into(), cta.value());
        }
        if let Some(cut) = &self.vertical {
            let mut video = json!({
                "url": cut.url,
                "caption": self.article.caption,
                // A file the CMS hosts, unless the cut is on YouTube — then
                // the same provider and id the landscape video is sent with,
                // so the page embeds it rather than streaming a file.
                "provider": "upload",
            });
            if let Some(id) = &cut.video_id {
                video["provider"] = json!("youtube");
                video["externalId"] = json!(id);
            }
            object.insert("videoVertical".into(), video);
        }
        // Written by the model on every draft and, until now, dropped on the
        // floor here. The field is real and the page reads it.
        if !self.article.keywords.is_empty() {
            object.insert("keywords".into(), json!(self.article.keywords));
        }
        // Both, deliberately. `keywordTargets` is the brief the site reads now
        // and `keywords` is what it falls back to — sending only the structured
        // one would leave anything still reading the flat list with nothing.
        if !self.article.keyword_targets.is_empty() {
            object.insert(
                "keywordTargets".into(),
                targets(&self.article.keyword_targets),
            );
        }
        if !self.chapters.is_empty() {
            object.insert(
                "videoChapters".into(),
                serde_json::to_value(&self.chapters).unwrap_or(Value::Null),
            );
        }
        if let Some(transcript) = &self.transcript {
            object.insert(
                "transcript".into(),
                serde_json::to_value(transcript).unwrap_or(Value::Null),
            );
            object.insert("transcriptProvider".into(), json!("assemblyai"));
        }
        for (field, id) in self.relations() {
            object.insert(field.into(), json!(id));
        }
        json!({ "data": data })
    }

    /// The `video` component, told explicitly where the video lives.
    ///
    /// `provider` and `externalId` rather than a bare watch URL. The site's
    /// `getVideoUrl` hands back `video.url` untouched until `provider` is set,
    /// and YouTube then plays only because `getEmbedUrl`'s regex happens to
    /// catch a watch URL further downstream — a documented fallback, and the
    /// only thing that has been standing between this pipeline and a page with
    /// no player on it.
    fn video(&self) -> Value {
        let mut video = json!({
            "url": self.video_url,
            "caption": self.article.caption,
            "provider": "upload",
        });
        if let Some(id) = &self.video_id {
            video["provider"] = json!("youtube");
            video["externalId"] = json!(id);
        }
        video
    }

    /// The relations that were actually resolved, in `RELATIONS` order.
    ///
    /// A lookup that missed is left off the payload entirely rather than sent as
    /// null: null is a value Strapi will happily write, and it would clear a
    /// relation someone had set by hand.
    pub fn relations(&self) -> Vec<(&'static str, i64)> {
        [("author", self.author_id), ("category", self.category_id)]
            .into_iter()
            .filter_map(|(field, id)| id.map(|id| (field, id)))
            .collect()
    }
}

/// One article block as a dynamic-zone component.
pub fn zone(
    blocks: &[Block],
    figures: &std::collections::BTreeMap<u32, FigureMedia>,
    embeds: &std::collections::BTreeMap<String, Value>,
) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text { html } => Some(json!({
                "__component": "content.text",
                "textBodyHtml": html,
            })),
            Block::Quote { text, highlight } => Some(json!({
                "__component": "content.quote",
                "quoteText": text,
                // Sent as "" rather than omitted: the field is optional but the
                // reader treats an absent value and an empty one the same, and
                // "" is what the legacy publisher wrote.
                "quoteTextHighlighted": highlight,
            })),
            Block::Table { headers, rows } => Some(json!({
                "__component": "content.table",
                "tableHeaders": headers.join(", "),
                "tableContent": rows
                    .iter()
                    .map(|row| row.join(", "))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            })),
            // Verbatim: the component stage built this against the landing
            // repo's catalog and ran its validator over it, and anything this
            // side rewrote would be a change that never met either.
            Block::Embed { id } => embeds.get(id).cloned(),
            // The blocks that can be dropped at this stage, and the only ones
            // that have to be: everything else is self-contained, while a
            // figure needs an upload and an embed needs a build.
            Block::Figure { n } => figures.get(n).map(|media| {
                json!({
                    "__component": "content.image",
                    "src": media.src,
                    "alt": media.alt,
                    "caption": media.caption,
                    "width": media.width,
                    "height": media.height,
                })
            }),
        })
        .collect()
}

/// The keyword targets, with the empty optionals left off.
///
/// `intent` and `notes` are omitted when blank rather than sent as `""`. For
/// `intent` that is not tidiness: the field is a closed enum, and `""` is not
/// one of the four values it accepts.
fn targets(entries: &[super::schema::KeywordTarget]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|target| {
                let mut value = json!({ "term": target.term, "priority": target.priority });
                let object = value.as_object_mut().expect("a json object");
                for (field, text) in [("intent", &target.intent), ("notes", &target.notes)] {
                    if !text.is_empty() {
                        object.insert(field.into(), json!(text));
                    }
                }
                value
            })
            .collect(),
    )
}

/// The FAQ, under the field names the component actually declares.
///
/// `title` and `content`, **not** `question` and `answer`. The obvious pair is
/// the wrong one — the landing repo's schema doc warns about it in bold — and
/// getting it wrong does not fail: Strapi drops unknown keys inside a component
/// and the accordion publishes with empty rows.
fn faq(entries: &[super::schema::Faq]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|entry| json!({ "title": entry.question, "content": entry.answer }))
            .collect(),
    )
}

/// Today, as the `date` field wants it.
pub fn today() -> String {
    crate::schedule::ledger::now_rfc3339()
        .chars()
        .take(10)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blog::chapters::{Transcript, VideoChapter};
    use crate::notes::TranscriptWord;

    fn article() -> Article {
        Article {
            title: "Why watermarking fails".into(),
            slug: "why-watermarking-fails".into(),
            description: "A complete promise of the argument.".into(),
            caption: "Four minutes on enforcement economics".into(),
            keywords: vec!["ai".into()],
            blocks: vec![
                Block::Text {
                    html: "<h2>Where it broke</h2><p>Body.</p>".into(),
                },
                Block::Quote {
                    text: "It could not.".into(),
                    highlight: "could not".into(),
                },
            ],
            ..Article::default()
        }
    }

    fn post() -> NewVideoPost {
        NewVideoPost {
            figures: Default::default(),
            embeds: Default::default(),
            article: article(),
            date: "2026-08-18".into(),
            video_url: "https://www.youtube.com/watch?v=2-nJ8yH_L_8".into(),
            video_id: Some("2-nJ8yH_L_8".into()),
            duration: 238,
            thumbnail_id: Some(42),
            thumbnail_vertical_id: None,
            vertical: None,
            og_image_id: None,
            cta: None,
            chapters: vec![VideoChapter {
                name: "Where it broke".into(),
                start_offset: 0,
                end_offset: 60,
            }],
            transcript: Some(Transcript {
                status: "completed",
                language_code: "en",
                text: "we said this".into(),
                words: vec![TranscriptWord {
                    text: "we".into(),
                    start: 0,
                    end: 10,
                    confidence: 0.9,
                }],
                chapters: Vec::new(),
                audio_duration: 60.0,
            }),
            author_id: Some(7),
            category_id: None,
        }
    }

    /// Every required field on the collection type, named exactly as Strapi
    /// validates it. A typo here is a 400 after the thumbnail is already up.
    #[test]
    fn the_payload_carries_every_required_field() {
        let body = post().body();
        let data = &body["data"];
        assert_eq!(data["title"], "Why watermarking fails");
        assert_eq!(data["slug"], "why-watermarking-fails");
        assert_eq!(data["date"], "2026-08-18");
        assert_eq!(
            data["shortAndMetaDescription"],
            "A complete promise of the argument."
        );
        assert_eq!(data["duration"], 238);
        assert_eq!(
            data["video"]["url"],
            "https://www.youtube.com/watch?v=2-nJ8yH_L_8"
        );
        assert_eq!(
            data["video"]["caption"],
            "Four minutes on enforcement economics"
        );
        assert_eq!(data["thumbnail"], 42);
        assert!(data["content"].is_array());
    }

    /// `duration` is `min: 1`, so a video that probed as zero must not be sent as
    /// zero — Strapi refuses the whole entry.
    #[test]
    fn a_zero_duration_is_raised_to_the_minimum_strapi_accepts() {
        let mut zero = post();
        zero.duration = 0;
        assert_eq!(zero.body()["data"]["duration"], 1);
    }

    /// A missed lookup is omitted, never sent as null — null is a value Strapi
    /// writes, and it would clear a relation someone set by hand.
    #[test]
    fn an_unresolved_relation_is_left_off_rather_than_nulled() {
        let body = post().body();
        let data = body["data"].as_object().unwrap();
        assert_eq!(data["author"], 7);
        assert!(
            !data.contains_key("category"),
            "an unresolved lookup is absent"
        );
    }

    #[test]
    fn the_resolved_relations_are_reported_for_the_echo_check() {
        assert_eq!(post().relations(), vec![("author", 7)]);
    }

    /// `category` is the only taxonomy a video post carries. The
    /// `education-categories` collection it used to be filed under was retired,
    /// and a post that still named it was uncategorised everywhere that counts.
    #[test]
    fn category_is_the_one_taxonomy_and_it_is_verified() {
        let mut filed = post();
        filed.category_id = Some(9);
        let body = filed.body();
        assert_eq!(body["data"]["category"], 9);
        assert!(
            !body["data"]
                .as_object()
                .unwrap()
                .contains_key("educationCategory"),
            "the retired collection is not written to"
        );
        assert_eq!(filed.relations(), vec![("author", 7), ("category", 9)]);
        // Every one of them is asked to be echoed back, so a silent drop shows.
        for (field, _) in filed.relations() {
            assert!(
                RELATIONS.contains(&field),
                "{field} is verified after create"
            );
        }
        assert!(!RELATIONS.contains(&"educationCategory"));
    }

    /// The video component says where the video lives rather than leaving the
    /// site to sniff a watch URL with a regex, which is the documented fallback
    /// and renders no player at all when it misses.
    #[test]
    fn a_youtube_video_names_its_provider_and_id() {
        let video = &post().body()["data"]["video"];
        assert_eq!(video["provider"], "youtube");
        assert_eq!(video["externalId"], "2-nJ8yH_L_8");
        assert_eq!(video["url"], "https://www.youtube.com/watch?v=2-nJ8yH_L_8");
        assert_eq!(video["caption"], "Four minutes on enforcement economics");
    }

    /// No id means an uploaded file, which is the component's own default —
    /// sent explicitly all the same, because "unset" and "upload" look
    /// identical in the admin and only one of them was decided.
    #[test]
    fn a_video_with_no_external_id_is_declared_an_upload() {
        let mut hosted = post();
        hosted.video_id = None;
        let video = &hosted.body()["data"]["video"];
        assert_eq!(video["provider"], "upload");
        assert!(!video.as_object().unwrap().contains_key("externalId"));
    }

    /// Written on every draft and, until this existed, dropped between the
    /// generator and the CMS.
    #[test]
    fn the_keywords_the_model_wrote_are_sent() {
        assert_eq!(post().body()["data"]["keywords"][0], "ai");
        let mut bare = post();
        bare.article.keywords.clear();
        assert!(!bare.body()["data"]
            .as_object()
            .unwrap()
            .contains_key("keywords"));
    }

    /// The brief and the flat list both go, because the site reads the first
    /// and falls back to the second — sending only the structured one leaves
    /// anything still on the old field with nothing.
    #[test]
    fn the_keyword_brief_is_sent_alongside_the_flat_list() {
        let mut briefed = post();
        briefed.article.keyword_targets = vec![crate::blog::schema::KeywordTarget {
            term: "ai watermarking".into(),
            priority: "primary".into(),
            intent: "informational".into(),
            notes: String::new(),
        }];
        let data = briefed.body();
        let target = &data["data"]["keywordTargets"][0];
        assert_eq!(target["term"], "ai watermarking");
        assert_eq!(target["priority"], "primary");
        assert_eq!(target["intent"], "informational");
        // `intent` is a closed enum and "" is not one of its values, so an
        // unclassified target omits the field rather than emptying it.
        assert!(!target.as_object().unwrap().contains_key("notes"));
        assert_eq!(data["data"]["keywords"][0], "ai", "the fallback still goes");
    }

    /// The two field names that are a trap. `question`/`answer` is what anyone
    /// writing this by hand reaches for, and Strapi drops unknown keys inside a
    /// component rather than refusing them — so the wrong pair publishes an
    /// accordion of empty rows and says nothing about it.
    #[test]
    fn the_faq_uses_the_field_names_the_component_declares() {
        let mut asked = post();
        asked.article.faq = vec![crate::blog::schema::Faq {
            question: "Does it scale?".into(),
            answer: "Not past the first retry.".into(),
        }];
        let entry = &asked.body()["data"]["faq"][0];
        assert_eq!(entry["title"], "Does it scale?");
        assert_eq!(entry["content"], "Not past the first retry.");
        let object = entry.as_object().unwrap();
        assert!(
            !object.contains_key("question"),
            "the obvious pair is the wrong one"
        );
        assert!(!object.contains_key("answer"));
    }

    /// Both are optional on the collection and both fall back on the site, so
    /// an article that has neither sends neither rather than sending empties.
    #[test]
    fn an_article_with_no_heading_and_no_faq_sends_neither() {
        let data = post().body();
        let object = data["data"].as_object().unwrap();
        assert!(!object.contains_key("h1"));
        assert!(!object.contains_key("faq"));
    }

    #[test]
    fn a_heading_that_differs_from_the_title_is_sent() {
        let mut headed = post();
        headed.article.h1 = "Why watermarking fails, and what enforcement costs".into();
        assert_eq!(
            headed.body()["data"]["h1"],
            "Why watermarking fails, and what enforcement costs"
        );
    }

    /// Every field on the collection this pipeline can fill, filled at once.
    /// The completeness check: a field added to the schema and wired nowhere is
    /// exactly the gap this stage keeps discovering, so the whole set is pinned
    /// in one place.
    #[test]
    fn a_fully_specified_post_carries_every_field_it_can() {
        let mut full = post();
        full.article.h1 = "A heading that reads differently".into();
        full.article.long_description = "The same promise, with room to keep it.".into();
        full.article.no_index = true;
        full.article.canonical_url = "https://example.com/original".into();
        full.article.keyword_targets = vec![crate::blog::schema::KeywordTarget {
            term: "ai watermarking".into(),
            priority: "primary".into(),
            intent: "informational".into(),
            notes: "Has to explain why enforcement fails.".into(),
        }];
        full.article.faq = vec![crate::blog::schema::Faq {
            question: "Does it scale?".into(),
            answer: "Not past the first retry.".into(),
        }];
        full.category_id = Some(9);
        full.og_image_id = Some(91);
        full.vertical = Some(VerticalCut {
            url: "https://cms.saagasolve.com/uploads/vertical.mp4".into(),
            video_id: None,
        });
        full.thumbnail_vertical_id = Some(88);
        full.cta = Some(MagicLinkCta {
            url: "https://saagasolve.com/start".into(),
            label: "Book a call".into(),
            description: "Thirty minutes, no deck.".into(),
            image_id: Some(77),
        });

        let body = full.body();
        let data = body["data"].as_object().unwrap();
        for field in [
            "title",
            "slug",
            "date",
            "shortAndMetaDescription",
            "description",
            "duration",
            "video",
            "thumbnail",
            "content",
            "isFeatured",
            "videoChapters",
            "transcript",
            "transcriptProvider",
            "author",
            "category",
            "keywords",
            "h1",
            "faq",
            "ogImage",
            "videoVertical",
            "thumbnailVertical",
            "noIndex",
            "canonicalUrl",
            "magicLinkCta",
            "keywordTargets",
        ] {
            assert!(
                data.contains_key(field),
                "{field} is not sent by any code path"
            );
        }
        // And nothing that was retired.
        assert!(!data.contains_key("educationCategory"));
    }

    /// `url` is the only field the component requires, and the rest are omitted
    /// rather than sent empty — an empty label renders an unlabelled button.
    #[test]
    fn a_bare_cta_sends_only_its_url() {
        let mut bare = post();
        bare.cta = Some(MagicLinkCta {
            url: "https://saagasolve.com/start".into(),
            label: String::new(),
            description: String::new(),
            image_id: None,
        });
        let cta = &bare.body()["data"]["magicLinkCta"];
        assert_eq!(cta["url"], "https://saagasolve.com/start");
        let object = cta.as_object().unwrap();
        assert_eq!(object.len(), 1, "an empty field was sent: {cta}");
    }

    /// Both controls default off, and both are omitted at their defaults rather
    /// than written — sending `noIndex: false` would overwrite someone's choice
    /// in the admin, and that choice is the whole reason the field exists.
    #[test]
    fn the_search_controls_are_absent_until_they_are_set() {
        let data = post().body();
        let object = data["data"].as_object().unwrap();
        assert!(!object.contains_key("noIndex"));
        assert!(!object.contains_key("canonicalUrl"));
        assert!(
            !object.contains_key("description"),
            "no long description written"
        );
        assert!(!object.contains_key("magicLinkCta"));
    }

    /// Absent falls back to `thumbnail`, so it is omitted rather than sent
    /// pointing at the same media id — which would say "this is deliberately
    /// different" about an image that is not.
    #[test]
    fn a_post_with_no_social_card_omits_the_field() {
        assert!(!post().body()["data"]
            .as_object()
            .unwrap()
            .contains_key("ogImage"));
    }

    #[test]
    fn a_refitted_social_card_is_sent_as_its_own_media() {
        let mut carded = post();
        carded.og_image_id = Some(91);
        let body = carded.body();
        assert_eq!(body["data"]["ogImage"], 91);
        // Its own id, never the thumbnail's: the point of the field is that the
        // two images are different shapes.
        assert_ne!(body["data"]["ogImage"], body["data"]["thumbnail"]);
    }

    /// Absent is the common case and it has to publish the page that existed
    /// before this field did: one player at every width, no empty portrait box.
    #[test]
    fn a_post_with_no_portrait_cut_sends_neither_vertical_field() {
        let data = post().body();
        let object = data["data"].as_object().unwrap();
        assert!(!object.contains_key("videoVertical"));
        assert!(!object.contains_key("thumbnailVertical"));
    }

    #[test]
    fn a_portrait_cut_is_sent_as_a_cms_hosted_upload() {
        let mut tall = post();
        tall.vertical = Some(VerticalCut {
            url: "https://cms.saagasolve.com/uploads/longform_vertical.mp4".into(),
            video_id: None,
        });
        let body = tall.body();
        let video = &body["data"]["videoVertical"];
        assert_eq!(
            video["url"],
            "https://cms.saagasolve.com/uploads/longform_vertical.mp4"
        );
        assert_eq!(
            video["provider"], "upload",
            "hosted by the CMS, not YouTube"
        );
        assert!(!video.as_object().unwrap().contains_key("externalId"));
        assert_eq!(video["caption"], "Four minutes on enforcement economics");
    }

    /// The cut that went up as a Short is embedded like the landscape video —
    /// provider and id — so the page plays YouTube's copy and the CMS holds no
    /// file. The same shape `video` is sent with, for the same reason.
    #[test]
    fn a_portrait_cut_on_youtube_is_sent_as_a_short_embed() {
        let mut tall = post();
        tall.vertical = Some(VerticalCut {
            url: "https://www.youtube.com/shorts/sh0rt1d".into(),
            video_id: Some("sh0rt1d".into()),
        });
        let body = tall.body();
        let video = &body["data"]["videoVertical"];
        assert_eq!(video["provider"], "youtube");
        assert_eq!(video["externalId"], "sh0rt1d");
        assert_eq!(video["url"], "https://www.youtube.com/shorts/sh0rt1d");
        assert_eq!(video["caption"], "Four minutes on enforcement economics");
    }

    /// The poster is a post-level field out of the artwork set, so exactly one
    /// thing writes it. Two writers is what this replaced — and the loser wrote
    /// last, which is the version that would have published.
    #[test]
    fn the_portrait_poster_has_exactly_one_writer() {
        let mut tall = post();
        tall.vertical = Some(VerticalCut {
            url: "https://cms.saagasolve.com/uploads/longform_vertical.mp4".into(),
            video_id: None,
        });
        assert!(
            !tall.body()["data"]
                .as_object()
                .unwrap()
                .contains_key("thumbnailVertical"),
            "a cut must not conjure a poster of its own"
        );
        tall.thumbnail_vertical_id = Some(88);
        assert_eq!(tall.body()["data"]["thumbnailVertical"], 88);
    }

    /// A dry run has uploaded nothing, so it has no media id to name. Omitted
    /// rather than sent as 0 or null: both are values, and both are wrong.
    #[test]
    fn a_body_with_no_upload_omits_the_thumbnail() {
        let mut dry = post();
        dry.thumbnail_id = None;
        assert!(!dry.body()["data"]
            .as_object()
            .unwrap()
            .contains_key("thumbnail"));
    }

    /// The pipeline never claims the featured slot.
    #[test]
    fn the_pipeline_does_not_feature_its_own_post() {
        assert_eq!(post().body()["data"]["isFeatured"], false);
    }

    #[test]
    fn a_transcript_brings_its_provider_with_it() {
        let body = post().body();
        assert_eq!(body["data"]["transcriptProvider"], "assemblyai");
        assert_eq!(body["data"]["transcript"]["text"], "we said this");
    }

    /// No transcript means no `transcriptProvider` either — claiming an
    /// AssemblyAI transcription of nothing is worse than leaving both empty.
    #[test]
    fn no_transcript_claims_no_provider() {
        let mut bare = post();
        bare.transcript = None;
        let data = bare.body();
        let object = data["data"].as_object().unwrap();
        assert!(!object.contains_key("transcript"));
        assert!(!object.contains_key("transcriptProvider"));
    }

    #[test]
    fn no_chapters_means_no_empty_chapter_list() {
        let mut bare = post();
        bare.chapters.clear();
        assert!(!bare.body()["data"]
            .as_object()
            .unwrap()
            .contains_key("videoChapters"));
    }

    #[test]
    fn chapters_keep_the_camel_case_offsets_the_component_declares() {
        let body = post().body();
        let chapter = &body["data"]["videoChapters"][0];
        assert_eq!(chapter["name"], "Where it broke");
        assert_eq!(chapter["startOffset"], 0);
        assert_eq!(chapter["endOffset"], 60);
    }

    #[test]
    fn blocks_become_the_components_the_dynamic_zone_accepts() {
        let got = zone(&article().blocks, &Default::default(), &Default::default());
        assert_eq!(got[0]["__component"], "content.text");
        assert_eq!(
            got[0]["textBodyHtml"],
            "<h2>Where it broke</h2><p>Body.</p>"
        );
        assert_eq!(got[1]["__component"], "content.quote");
        assert_eq!(got[1]["quoteText"], "It could not.");
        assert_eq!(got[1]["quoteTextHighlighted"], "could not");
    }

    /// The flattening the prompt's no-commas rule exists for: headers join on
    /// commas and rows join on blank lines, so a comma inside a cell would
    /// silently become a new column downstream.
    #[test]
    fn a_table_is_flattened_the_way_the_reader_unflattens_it() {
        let got = zone(
            &[Block::Table {
                headers: vec!["Option".into(), "Cost".into()],
                rows: vec![
                    vec!["Watermark".into(), "High".into()],
                    vec!["Restraint".into(), "Low".into()],
                ],
            }],
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(got[0]["__component"], "content.table");
        assert_eq!(got[0]["tableHeaders"], "Option, Cost");
        assert_eq!(got[0]["tableContent"], "Watermark, High\n\nRestraint, Low");
    }

    /// The block the CMS renders as a figure, with the two fields that were
    /// missing from the site's own component until figures existed.
    #[test]
    fn a_resolved_figure_becomes_a_content_image() {
        let mut figures = std::collections::BTreeMap::new();
        figures.insert(
            2,
            FigureMedia {
                src: "https://cms.saagasolve.com/uploads/figure_02.jpg".into(),
                alt: "A log of 429s".into(),
                caption: "The retry storm.".into(),
                width: 1280,
                height: 960,
            },
        );
        let got = zone(&[Block::Figure { n: 2 }], &figures, &Default::default());
        assert_eq!(got[0]["__component"], "content.image");
        assert_eq!(
            got[0]["src"],
            "https://cms.saagasolve.com/uploads/figure_02.jpg"
        );
        assert_eq!(got[0]["alt"], "A log of 429s");
        assert_eq!(got[0]["caption"], "The retry storm.");
        // Without these the site lays every figure out at the featured image's
        // 1504x960 ratio and a 4:3 screenshot renders squashed.
        assert_eq!(got[0]["width"], 1280);
        assert_eq!(got[0]["height"], 960);
    }

    /// An unresolved figure is the one block this stage drops. A `content.image`
    /// with no `src` is a visibly broken image on a permanent public page.
    #[test]
    fn a_figure_with_no_upload_is_left_out_of_the_zone() {
        let got = zone(
            &[
                Block::Text {
                    html: "<p>Body.</p>".into(),
                },
                Block::Figure { n: 4 },
            ],
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(got.len(), 1, "the figure published anyway: {got:?}");
        assert_eq!(got[0]["__component"], "content.text");
    }

    /// Passed through byte for byte. The catalog of valid `componentKey`s and
    /// each one's config schema live in the landing repo, so a block this side
    /// rewrote would be one that never met the validator that knows them.
    #[test]
    fn a_built_component_is_published_exactly_as_it_was_validated() {
        let mut embeds = std::collections::BTreeMap::new();
        embeds.insert(
            "retry_steps".to_string(),
            json!({
                "__component": "content.embed",
                "componentKey": "step_cards",
                "componentVersion": 1,
                "heading": "How the retry works",
                "config": { "steps": [{ "title": "Back off", "body": "Wait, then halve." }] },
            }),
        );
        let got = zone(
            &[Block::Embed {
                id: "retry_steps".into(),
            }],
            &Default::default(),
            &embeds,
        );
        assert_eq!(got[0]["__component"], "content.embed");
        assert_eq!(got[0]["componentKey"], "step_cards");
        assert_eq!(got[0]["config"]["steps"][0]["title"], "Back off");
    }

    /// An unrecognised key does not error in the CMS — it silently blanks that
    /// one block on a permanent public page. So an id with nothing built for it
    /// never gets there.
    #[test]
    fn an_embed_with_nothing_built_for_it_is_left_out_of_the_zone() {
        let got = zone(
            &[
                Block::Text {
                    html: "<p>Body.</p>".into(),
                },
                Block::Embed {
                    id: "never_built".into(),
                },
            ],
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(got.len(), 1, "the empty embed published anyway: {got:?}");
    }

    #[test]
    fn a_quote_with_no_highlight_still_sends_the_field() {
        let got = zone(
            &[Block::Quote {
                text: "Said it.".into(),
                highlight: String::new(),
            }],
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(got[0]["quoteTextHighlighted"], "");
    }

    #[test]
    fn today_is_a_bare_date_not_a_timestamp() {
        let got = today();
        assert_eq!(got.len(), 10, "{got}");
        assert!(!got.contains('T'), "{got}");
        assert_eq!(got.matches('-').count(), 2, "{got}");
    }
    #[test]
    fn all_artwork_fields_travel_even_without_a_portrait_video() {
        let mut value = post();
        value.thumbnail_id = Some(10);
        value.thumbnail_vertical_id = Some(11);
        value.og_image_id = Some(12);
        value.vertical = None;
        let body = value.body();
        assert_eq!(body["data"]["thumbnail"], 10);
        assert_eq!(body["data"]["thumbnailVertical"], 11);
        assert_eq!(body["data"]["ogImage"], 12);
        assert!(body["data"].get("videoVertical").is_none());
    }
}
