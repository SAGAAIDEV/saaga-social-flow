//! Per-service Buffer metadata: what each platform needs beside the video asset.
//!
//! Built at *plan* time rather than send time, so `schedule.json` shows the
//! irreversible half of the payload — YouTube privacy, subscriber notification,
//! the Instagram reel/feed choice — before anyone presses Queue. `createPost`
//! then sends this object verbatim; nothing is invented later.
//!
//! Shapes are introspected: `InstagramPostMetadataInput` requires both `type` and
//! `shouldShareToFeed`, `YoutubePostMetadataInput` has no `type` at all and its
//! privacy enum is lowercase, `FacebookPostMetadataInput` requires `type`, and
//! `TikTokPostMetadataInput` is `{ isAiGenerated, title }` with both nullable.

use serde_json::{json, Value};

use super::buffer::trimmed;

/// Shorts go up public. **Only** shorts — this no longer governs the longform.
///
/// It used to be shared with [`crate::publish`] on the reasoning that the two
/// paths reach the same channel and a video's privacy should not depend on
/// which one carried it. That reasoning has expired: `plan::build` now skips
/// the longform's `youtube` row unconditionally ("uploaded straight to
/// YouTube — see the YouTube tab"), so the longform never travels this path and
/// the only videos left here are `youtube_shorts`. Different videos, not two
/// routes for one, which is why the longform's visibility became a setting
/// (`config.youtube_privacy`) and this stayed a constant.
///
/// A constant on purpose: Buffer cannot flip privacy after the fact, so an
/// unlisted queued Short would need fixing by hand on the channel. Nobody has
/// asked for that, and a knob that can only be turned the wrong way is worse
/// than no knob.
pub const YOUTUBE_PRIVACY: &str = "public";

/// The metadata one platform takes. Services we send nothing for (twitter,
/// bluesky, and anything unrecognised) collapse into [`PlatformMeta::None`].
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformMeta {
    /// `type` and `shouldShareToFeed` are both non-null on the Buffer input.
    Instagram { should_share_to_feed: bool },
    Tiktok { title: Option<String> },
    Youtube {
        title: Option<String>,
        /// YouTube's numeric category id. Buffer refuses a post without one, and
        /// there is no default it will pick for us.
        category_id: String,
        privacy: String,
        made_for_kids: bool,
        notify_subscribers: bool,
    },
    /// `PostTypeFacebook` is required; there is no title field to carry.
    Facebook,
    None,
}

/// The `metadata` object for one planned item, or `None` when the service takes
/// nothing from us. This is exactly what lands in `schedule.json`.
pub fn metadata_for(platform: &str, title: Option<&str>, youtube_category: &str) -> Option<Value> {
    platform_metadata(&platform_meta(platform, title, youtube_category))
}

/// Maps a posts.json platform string onto the metadata that platform needs.
pub fn platform_meta(platform: &str, title: Option<&str>, youtube_category: &str) -> PlatformMeta {
    let title = title.map(str::to_string);
    match platform {
        "instagram" => PlatformMeta::Instagram { should_share_to_feed: true },
        "tiktok" => PlatformMeta::Tiktok { title },
        "youtube" | "youtube_shorts" => PlatformMeta::Youtube {
            title,
            category_id: youtube_category.to_string(),
            privacy: YOUTUBE_PRIVACY.to_string(),
            made_for_kids: false,
            notify_subscribers: true,
        },
        "facebook" => PlatformMeta::Facebook,
        _ => PlatformMeta::None,
    }
}

fn platform_metadata(meta: &PlatformMeta) -> Option<Value> {
    match meta {
        PlatformMeta::Instagram { should_share_to_feed } => Some(json!({
            "instagram": { "type": "reel", "shouldShareToFeed": should_share_to_feed }
        })),
        PlatformMeta::Tiktok { title } => {
            // Build the object, then drop it only when it is genuinely empty —
            // a missing title must never take the rest of the object with it.
            let mut object = json!({});
            insert_opt(&mut object, "title", title.as_deref());
            non_empty(object).map(|object| json!({ "tiktok": object }))
        }
        PlatformMeta::Youtube {
            title,
            category_id,
            privacy,
            made_for_kids,
            notify_subscribers,
        } => {
            let privacy = trimmed(Some(privacy.as_str())).unwrap_or("private").to_lowercase();
            let mut object = json!({
                "privacy": privacy,
                "madeForKids": made_for_kids,
                "notifySubscribers": notify_subscribers,
            });
            insert_opt(&mut object, "title", title.as_deref());
            // Omitted rather than sent blank when unset: an empty categoryId is
            // its own rejection, and the missing-category error names the cause.
            insert_opt(&mut object, "categoryId", Some(category_id.as_str()));
            Some(json!({ "youtube": object }))
        }
        PlatformMeta::Facebook => Some(json!({ "facebook": { "type": "post" } })),
        PlatformMeta::None => Option::None,
    }
}

fn insert_opt(object: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = trimmed(value) {
        object[key] = json!(value);
    }
}

fn non_empty(object: Value) -> Option<Value> {
    let empty = object.as_object().map(serde_json::Map::is_empty).unwrap_or(true);
    if empty {
        Option::None
    } else {
        Some(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instagram_sends_both_required_fields() {
        let meta = metadata_for("instagram", Some("Chapter One"), "28").expect("instagram metadata");
        assert_eq!(meta["instagram"]["type"], "reel");
        assert_eq!(meta["instagram"]["shouldShareToFeed"], true);
        // The title rides the video asset, not the instagram metadata.
        assert!(meta["instagram"].get("title").is_none());
    }

    #[test]
    fn youtube_and_shorts_share_one_public_notifying_channel() {
        for platform in ["youtube", "youtube_shorts"] {
            let meta = metadata_for(platform, Some("The Long One"), "28").expect("youtube metadata");
            let yt = &meta["youtube"];
            assert_eq!(yt["privacy"], "public", "{platform} privacy");
            assert_eq!(yt["title"], "The Long One");
            assert_eq!(yt["madeForKids"], false);
            assert_eq!(yt["notifySubscribers"], true);
            assert!(yt.get("type").is_none(), "youtube metadata has no type key");
        }
    }

    /// Buffer rejects a YouTube post that carries no category — "YouTube posts
    /// require a category." — and it rejected five before this was sent. Shorts
    /// are YouTube posts too, which is why both platforms are checked here.
    #[test]
    fn every_youtube_post_carries_a_category() {
        for platform in ["youtube", "youtube_shorts"] {
            let meta = metadata_for(platform, Some("The Long One"), "27").expect("metadata");
            assert_eq!(meta["youtube"]["categoryId"], "27", "{platform}");
        }
    }

    #[test]
    fn youtube_privacy_is_forced_lowercase() {
        let meta = platform_metadata(&PlatformMeta::Youtube {
            title: None,
            category_id: "28".into(),
            privacy: "PUBLIC".into(),
            made_for_kids: false,
            notify_subscribers: false,
        })
        .expect("youtube metadata");
        assert_eq!(meta["youtube"]["privacy"], "public");
        assert!(meta["youtube"].get("title").is_none());
    }

    #[test]
    fn a_blank_privacy_degrades_to_private_rather_than_an_invalid_enum() {
        let meta = platform_metadata(&PlatformMeta::Youtube {
            title: None,
            category_id: "28".into(),
            privacy: "   ".into(),
            made_for_kids: true,
            notify_subscribers: false,
        })
        .expect("youtube metadata");
        assert_eq!(meta["youtube"]["privacy"], "private");
    }

    #[test]
    fn tiktok_keeps_its_title_and_omits_the_object_only_when_it_is_empty() {
        let titled = metadata_for("tiktok", Some(" Chapter One "), "28").expect("tiktok metadata");
        assert_eq!(titled["tiktok"]["title"], "Chapter One");
        // posts.json usually leaves the tiktok title null; that must not become
        // a half-built object, it becomes no metadata at all.
        assert!(metadata_for("tiktok", None, "28").is_none());
        assert!(metadata_for("tiktok", Some("   "), "28").is_none());
    }

    #[test]
    fn facebook_sends_the_required_type_and_nothing_else() {
        let meta = metadata_for("facebook", Some("ignored"), "28").expect("facebook metadata");
        assert_eq!(meta["facebook"], json!({ "type": "post" }));
    }

    #[test]
    fn platforms_that_take_no_metadata_send_none() {
        for platform in ["twitter", "bluesky", "linkedin", "mastodon", ""] {
            assert!(
                metadata_for(platform, Some("A Title"), "28").is_none(),
                "{platform} should send no metadata"
            );
            assert_eq!(platform_meta(platform, None, "28"), PlatformMeta::None);
        }
    }
}
