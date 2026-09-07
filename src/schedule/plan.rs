//! Deterministic Buffer schedule planner.
//!
//! Produces one [`PlanItem`] per (video, platform) from the posts manifest, the
//! distributed S3 links and the live Buffer channel list. Nothing here touches
//! the network or an LLM — the plan is a pure function of posts.json, links.json,
//! the channels and the schedule ledger, so the UI can preview exactly what would
//! queue, down to the `metadata` object each post will carry.

use crate::distribute::schema::DistributeLinks;
use crate::posts::schema::{PlatformPost, PostsManifest};

use super::buffer::Channel;
use super::channels::{
    channel_block, channel_label, handle_note, preferred_handle, resolve_channel,
    scheduling_type_for,
};
use super::copy::{copy_hash, render_text};
use super::ledger;
use super::meta::metadata_for;
use super::schema::{PlanItem, SchedulePlan, ScheduleRow};

/// Prompt that produced the copy carried by every planned item.
const PROMPT_ID: &str = "posts.social";
/// Buffer share mode — everything goes to the channel queue, never shareNow.
const MODE: &str = "addToQueue";

#[tracing::instrument(
    skip(posts, links, channels, queued),
    fields(project = %project, videos = posts.items.len(), channels = channels.len())
)]
pub fn build_plan(
    posts: &PostsManifest,
    links: &DistributeLinks,
    channels: &[Channel],
    queued: &[ScheduleRow],
    project: &str,
    version: Option<u32>,
    // YouTube's numeric category id, from config. Passed in rather than read here
    // so the planner stays a pure function of its inputs.
    youtube_category: &str,
) -> SchedulePlan {
    let mut items = Vec::new();

    for video in &posts.items {
        let url = links.url_for(&video.video_id).unwrap_or_default().to_string();
        let orientation = orientation_of(links, &video.video_id);
        for post in &video.posts {
            // The one platform where the label can disagree with the file. Both
            // YouTube flavours ride the same channel and YouTube decides which it
            // is from the video, so the label is corrected to the video rather
            // than trusted from posts.json — which a language model writes.
            let platform = match resolve_channel(&post.platform, channels).is_ok()
                && crate::schedule::channels::service_for(&post.platform) == "youtube"
            {
                true => crate::schedule::channels::youtube_flavour(&post.platform, orientation)
                    .to_string(),
                false => post.platform.clone(),
            };
            let post = &PlatformPost { platform, ..post.clone() };

            let text = render_text(&post.content, &post.tags);
            let title = post.title.clone();
            let image = video.video_id == "longform" && matches!(post.platform.as_str(), "linkedin" | "facebook") && links.url_for("og-image").is_some();
            let url = if image { links.url_for("og-image").unwrap().to_string() } else { url.clone() };
            // Changing the artwork resets approval and dedupe along with the copy.
            let hash = if image { copy_hash(&format!("{text}\nimage:{url}"), title.as_deref()) } else { copy_hash(&text, title.as_deref()) };
            let resolved = resolve_channel(&post.platform, channels);
            let already = ledger::queued_row(queued, &video.video_id, &post.platform, &hash);
            // The channel is asked about first because the flavour above is only
            // trustworthy once it resolves: an unresolved YouTube channel leaves
            // the label as posts.json wrote it, and a Short mislabelled "youtube"
            // must report the channel problem rather than claim it was uploaded.
            let skip = match &resolved {
                Err(why) => Some(why.clone()),
                // Not Buffer's any more. A direct upload sets the title,
                // description, category, privacy *and* thumbnail in one call,
                // where Buffer cannot carry a thumbnail at all and publishes on
                // the channel's schedule rather than when the video is ready.
                // Kept in the plan as a skip rather than omitted, so the longform
                // still has a visible fate here. Shorts are unaffected — they are
                // `youtube_shorts` by now, and Buffer suits them.
                Ok(_) if post.platform == "youtube" => {
                    Some("uploaded straight to YouTube — see the YouTube tab".to_string())
                }
                Ok(_) if url.is_empty() => Some("no distributed url — run Distribute".to_string()),
                Ok(channel) => channel_block(channel)
                    .or_else(|| already.map(|row| format!("already queued {}", row.queued_at))),
            };

            let mut item = PlanItem {
                video_id: video.video_id.clone(),
                platform: post.platform.clone(),
                channel_id: String::new(),
                channel_name: String::new(),
                url: url.clone(),
                text,
                title: title.clone(),
                mode: MODE.to_string(),
                scheduling_type: "automatic".to_string(),
                // The gate lives in the Schedule tab, not in Buffer — one gate, and
                // it is the one showing the copy. See `PlanItem::approved`.
                needs_approval: false,
                image,
                metadata: if image {
                    (post.platform == "facebook").then(|| serde_json::json!({"facebook": {"type": "post"}}))
                } else { metadata_for(&post.platform, title.as_deref(), youtube_category) },
                reason: reason_for(queued, &video.video_id, &post.platform, already),
                prompt_id: PROMPT_ID.to_string(),
                prompt_version: posts.prompt_version,
                copy_hash: hash,
                skip,
                approved: false,
            };

            if image { item.reason = "Designed OG image with social copy".into(); }
            if let Ok(channel) = &resolved {
                item.channel_id = channel.id.clone();
                item.channel_name = channel_label(channel).to_string();
                item.scheduling_type = scheduling_type_for(channel).to_string();
                if preferred_handle(&post.platform).is_some() {
                    item.reason = format!("{} — @{}", item.reason, channel_label(channel));
                }
                if let Some(note) = handle_note(&post.platform, channel) {
                    item.reason = format!("{} — {note}", item.reason);
                }
            }

            items.push(item);
        }
    }

    items.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
    SchedulePlan { project: project.to_string(), version, items }
}

/// How this video was distributed — "landscape" or "portrait" — from links.json.
fn orientation_of<'a>(links: &'a DistributeLinks, video_id: &str) -> Option<&'a str> {
    links
        .items
        .iter()
        .find(|item| item.id == video_id)
        .and_then(|item| item.orientation.as_deref())
}


/// Why this item exists — plus the one thing the ledger knows that the plan
/// otherwise hides: this video is already live on this platform under *different*
/// copy. Regenerating posts makes every item queueable again by design, so without
/// this line a second Queue silently double-posts the same video with new words.
fn reason_for(
    rows: &[ScheduleRow],
    video_id: &str,
    platform: &str,
    already: Option<&ScheduleRow>,
) -> String {
    let base = if video_id == "longform" { "hub video" } else { "vertical chapter" };
    if already.is_some() {
        return base.to_string();
    }
    match ledger::prior_row(rows, video_id, platform) {
        Some(prior) => format!(
            "{base} — warning: already queued as {} on {} with different copy",
            prior.buffer_post_id, prior.queued_at
        ),
        None => base.to_string(),
    }
}

/// Preview order: longform, then chapters ascending, then platform name.
fn order_key(item: &PlanItem) -> (u8, u32, &str, &str) {
    let (rank, chapter) = match item.video_id.as_str() {
        "longform" => (0u8, 0u32),
        other => match other
            .strip_prefix("chapter-")
            .and_then(|n| n.parse::<u32>().ok())
        {
            Some(n) => (1, n),
            None => (2, 0),
        },
    };
    (rank, chapter, item.video_id.as_str(), item.platform.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distribute::schema::DistributedAsset;
    use crate::posts::schema::{PlatformPost, VideoPosts};

    fn channel(id: &str, service: &str, kind: &str, name: &str) -> Channel {
        Channel {
            id: id.into(),
            name: name.into(),
            display_name: name.into(),
            service: service.into(),
            kind: kind.into(),
            timezone: "America/Vancouver".into(),
            is_disconnected: false,
            is_queue_paused: false,
            default_to_reminders: None,
            posting_schedule: Vec::new(),
        }
    }

    fn all_channels() -> Vec<Channel> {
        vec![
            channel("6a3dbb795ab6d2f10671b945", "instagram", "business", "saagasocials"),
            channel("69267c5429ea336fd631f864", "linkedin", "profile", "amovfx"),
            channel("69267c5429ea336fd631f865", "linkedin", "page", "saagasolve"),
            channel("6a3dbb555ab6d2f10671b8cf", "tiktok", "account", "andrewmelnychukos"),
            channel("6a3dbba45ab6d2f10671b9db", "youtube", "channel", "SAAGA Solve"),
            channel("6a4a9bbf40483446287252ef", "twitter", "profile", "AndrewOsee59559"),
            channel("69266cd829ea336fd631d03a", "twitter", "profile", "amelnychukoseen"),
            channel("6a4959825ab6d2f106a52bb2", "bluesky", "profile", "saaga-dev.bsky.social"),
        ]
    }

    fn post(platform: &str) -> PlatformPost {
        PlatformPost {
            platform: platform.into(),
            title: Some("A Title".into()),
            content: "body copy".into(),
            tags: vec!["rust".into(), "#agents".into()],
        }
    }

    fn vp(video_id: &str, platforms: &[&str]) -> VideoPosts {
        let video_type = if video_id == "longform" { "horizontal" } else { "vertical" };
        VideoPosts {
            video_id: video_id.into(),
            video_type: video_type.into(),
            video_path: None,
            posts: platforms.iter().map(|p| post(p)).collect(),
        }
    }

    fn manifest(video_id: &str, platforms: &[&str]) -> PostsManifest {
        PostsManifest {
            version: Some(3),
            prompt_version: Some(2),
            prompt_hash: "feedfacefeedface".into(),
            items: vec![vp(video_id, platforms)],
        }
    }

    fn links(ids: &[&str]) -> DistributeLinks {
        DistributeLinks {
            project: "vd-42".into(),
            version: 3,
            items: ids
                .iter()
                .map(|id| DistributedAsset {
                    id: (*id).into(),
                    kind: "video".into(),
                    orientation: None,
                    chapter: None,
                    url: format!("https://cdn.example.com/{id}.mp4"),
                    file: None,
                })
                .collect(),
        }
    }

    fn plan_for(
        posts: &PostsManifest,
        links: &DistributeLinks,
        rows: &[ScheduleRow],
    ) -> SchedulePlan {
        build_plan(posts, links, &all_channels(), rows, "vd-42", Some(3), "28")
    }

    fn find<'a>(plan: &'a SchedulePlan, platform: &str) -> &'a PlanItem {
        plan.items
            .iter()
            .find(|item| item.platform == platform)
            .expect("planned item")
    }

    fn row_from(item: &PlanItem, post_id: &str, at: &str) -> ScheduleRow {
        ScheduleRow {
            id: super::super::schema::row_id(&item.video_id, &item.platform, &item.copy_hash),
            buffer_post_id: post_id.into(),
            project: "vd-42".into(),
            version: Some(3),
            video_id: item.video_id.clone(),
            platform: item.platform.clone(),
            channel_id: item.channel_id.clone(),
            url: item.url.clone(),
            prompt_id: PROMPT_ID.into(),
            prompt_version: item.prompt_version,
            copy_hash: item.copy_hash.clone(),
            queued_at: at.into(),
            deleted_at: None,
        }
    }

    /// Links as Distribute really writes them: the longform tagged landscape, the
    /// chapters portrait, and the chosen thumbnail beside them as an image.
    fn links_with_thumbnail() -> DistributeLinks {
        let mut links = links(&["longform", "chapter-01"]);
        for item in &mut links.items {
            item.orientation = Some(match item.id.as_str() {
                "longform" => "landscape".into(),
                _ => "portrait".to_string(),
            });
        }
        links.items.push(DistributedAsset {
            id: "thumbnail".into(),
            kind: "image".into(),
            orientation: Some("landscape".into()),
            chapter: None,
            url: "https://cdn.example.com/thumb-abc123.jpg".into(),
            file: Some("thumb-abc123.jpg".into()),
        });
        links
    }

    /// The ask this exists for: whatever posts.json says, a landscape video is a
    /// regular YouTube upload. Both flavours ride the same channel and
    /// `YoutubePostMetadataInput` has no field for which one it is — so if the
    /// label were wrong, nothing downstream could put it right.
    #[test]
    fn the_longform_goes_to_regular_youtube_even_if_posts_json_says_shorts() {
        let plan = plan_for(
            &manifest("longform", &["youtube_shorts"]),
            &links_with_thumbnail(),
            &[],
        );
        let item = find(&plan, "youtube");
        assert_eq!(item.video_id, "longform");
        assert!(plan.items.iter().all(|i| i.platform != "youtube_shorts"));
    }

    /// And the other way, so the label is always true rather than merely usually.
    #[test]
    fn a_vertical_chapter_goes_to_shorts_even_if_posts_json_says_youtube() {
        let plan = plan_for(
            &manifest("chapter-01", &["youtube"]),
            &links_with_thumbnail(),
            &[],
        );
        let item = find(&plan, "youtube_shorts");
        assert_eq!(item.video_id, "chapter-01");
        assert_eq!(item.channel_name, "SAAGA Solve", "the same one channel");
    }

    /// A links.json from before orientations were recorded has nothing to correct
    /// against, and a guess there could make a right label wrong.
    #[test]
    fn an_unknown_orientation_leaves_the_label_alone() {
        let plan = plan_for(
            &manifest("chapter-01", &["youtube_shorts"]),
            &links(&["chapter-01"]),
            &[],
        );
        assert_eq!(find(&plan, "youtube_shorts").video_id, "chapter-01");
    }

    /// Every other platform is left exactly as written — this correction is about
    /// one service whose two names share a channel, not about second-guessing.
    #[test]
    fn no_other_platform_is_rewritten() {
        let plan = plan_for(
            &manifest("chapter-01", &["tiktok", "bluesky", "instagram"]),
            &links_with_thumbnail(),
            &[],
        );
        let mut platforms: Vec<&str> = plan.items.iter().map(|i| i.platform.as_str()).collect();
        platforms.sort_unstable();
        assert_eq!(platforms, ["bluesky", "instagram", "tiktok"]);
    }

    /// An uploaded thumbnail is not a post asset. Buffer rejects a custom video
    /// cover outright, so the planner must not carry one anywhere near a payload —
    /// the image is published for use outside Buffer, and that is all.
    #[test]
    fn an_uploaded_thumbnail_never_reaches_a_planned_post() {
        let plan = plan_for(
            &manifest("longform", &["youtube"]),
            &links_with_thumbnail(),
            &[],
        );
        let item = find(&plan, "youtube");
        assert_eq!(item.url, "https://cdn.example.com/longform.mp4");
        let payload = serde_json::to_value(item).unwrap();
        assert!(
            !payload.to_string().contains("thumb-abc123"),
            "the cover must not appear in the plan item at all"
        );
    }

    /// The correction above only runs once the channel resolves, so a Short
    /// planned against a disconnected YouTube channel still carries the raw
    /// "youtube" label. It must report the channel, not claim it was uploaded.
    #[test]
    fn a_short_with_no_youtube_channel_names_the_channel_not_the_upload() {
        let channels: Vec<Channel> = all_channels()
            .into_iter()
            .filter(|c| c.service != "youtube")
            .collect();
        let plan = build_plan(
            &manifest("chapter-01", &["youtube"]),
            &links_with_thumbnail(),
            &channels,
            &[],
            "vd-42",
            Some(3),
            "28",
        );
        let skip = find(&plan, "youtube").skip.clone().expect("skipped");
        assert!(!skip.contains("YouTube tab"), "not an upload: {skip}");
        assert!(skip.to_lowercase().contains("channel"), "names the channel: {skip}");
    }

    #[test]
    fn missing_url_skips_the_item() {
        let plan = plan_for(&manifest("chapter-01", &["tiktok"]), &links(&[]), &[]);
        let item = find(&plan, "tiktok");
        assert_eq!(item.url, "");
        assert_eq!(item.skip.as_deref(), Some("no distributed url — run Distribute"));
        assert_eq!(plan.queueable().count(), 0);
    }

    /// The longform's YouTube upload does not go through Buffer, and the plan
    /// says so rather than leaving a gap where a row used to be.
    #[test]
    fn regular_youtube_is_skipped_and_names_where_it_went() {
        let plan = plan_for(
            &manifest("longform", &["youtube", "linkedin"]),
            &links_with_thumbnail(),
            &[],
        );
        assert_eq!(
            find(&plan, "youtube").skip.as_deref(),
            Some("uploaded straight to YouTube — see the YouTube tab")
        );
        // And it is only that one: everything else the longform posts is unchanged.
        assert!(find(&plan, "linkedin").skip.is_none());
    }

    /// A Short is still Buffer's — only the landscape upload moved.
    #[test]
    fn shorts_still_go_through_buffer() {
        let plan = plan_for(
            &manifest("chapter-01", &["youtube_shorts"]),
            &links_with_thumbnail(),
            &[],
        );
        assert!(find(&plan, "youtube_shorts").skip.is_none());
    }

    #[test]
    fn facebook_skips_because_no_channel_is_connected() {
        let plan = plan_for(
            &manifest("longform", &["facebook"]),
            &links(&["longform"]),
            &[],
        );
        let item = find(&plan, "facebook");
        assert_eq!(
            item.skip.as_deref(),
            Some("no facebook channel connected to Buffer")
        );
        assert_eq!(item.channel_id, "");
    }

    #[test]
    fn linkedin_prefers_the_page_over_the_profile() {
        let plan = plan_for(
            &manifest("longform", &["linkedin"]),
            &links(&["longform"]),
            &[],
        );
        let item = find(&plan, "linkedin");
        assert_eq!(item.channel_id, "69267c5429ea336fd631f865");
        assert_eq!(item.channel_name, "saagasolve");
        assert!(item.skip.is_none());
    }

    #[test]
    fn youtube_shorts_resolves_to_the_youtube_channel() {
        let plan = plan_for(
            &manifest("chapter-01", &["youtube_shorts"]),
            &links(&["chapter-01"]),
            &[],
        );
        let item = find(&plan, "youtube_shorts");
        assert_eq!(item.channel_id, "6a3dbba45ab6d2f10671b9db");
        assert_eq!(item.reason, "vertical chapter");
        assert!(item.skip.is_none());
    }

    #[test]
    fn twitter_picks_the_named_handle_and_says_which() {
        let plan = plan_for(
            &manifest("longform", &["twitter"]),
            &links(&["longform"]),
            &[],
        );
        let item = find(&plan, "twitter");
        assert_eq!(item.channel_id, "6a4a9bbf40483446287252ef");
        assert_eq!(item.reason, "hub video — @AndrewOsee59559");
    }

    #[test]
    fn disconnected_channel_skips_and_names_the_channel() {
        let mut channels = all_channels();
        channels.iter_mut().find(|c| c.service == "tiktok").unwrap().is_disconnected = true;
        let plan = build_plan(
            &manifest("chapter-02", &["tiktok"]),
            &links(&["chapter-02"]),
            &channels,
            &[],
            "vd-42",
            Some(3),
            "28",
        );
        assert_eq!(
            find(&plan, "tiktok").skip.as_deref(),
            Some("channel \"andrewmelnychukos\" is disconnected in Buffer")
        );
    }

    #[test]
    fn matching_ledger_row_skips_as_already_queued() {
        let posts = manifest("longform", &["bluesky"]);
        let links = links(&["longform"]);
        let planned = plan_for(&posts, &links, &[]);
        let item = find(&planned, "bluesky");
        assert!(item.skip.is_none());

        let row = row_from(item, "post-1", "2026-08-14T23:12:04Z");
        let again = plan_for(&posts, &links, std::slice::from_ref(&row));
        assert_eq!(
            find(&again, "bluesky").skip.as_deref(),
            Some("already queued 2026-08-14T23:12:04Z")
        );

        // A different platform with the same hash is untouched.
        let other = plan_for(&manifest("longform", &["instagram"]), &links, &[row]);
        assert!(find(&other, "instagram").skip.is_none());
    }

    /// Regenerated copy is queueable again on purpose — but the plan has to say
    /// out loud that the same video is already live, or Queue duplicates it.
    #[test]
    fn regenerated_copy_is_queueable_but_warns_about_the_live_post() {
        let links = links(&["longform"]);
        let first = plan_for(&manifest("longform", &["bluesky"]), &links, &[]);
        let row = row_from(find(&first, "bluesky"), "post-7", "2026-08-14T23:12:04Z");

        let mut rewritten = manifest("longform", &["bluesky"]);
        rewritten.items[0].posts[0].content = "a completely different hook".into();
        let again = plan_for(&rewritten, &links, &[row]);
        let item = find(&again, "bluesky");
        assert!(item.skip.is_none(), "new copy stays queueable");
        assert_eq!(
            item.reason,
            "hub video — warning: already queued as post-7 on 2026-08-14T23:12:04Z with different copy"
        );
    }

    /// The plan is the review surface for the irreversible half of the payload.
    #[test]
    fn the_plan_carries_the_metadata_queue_will_send() {
        let plan = plan_for(
            &manifest("longform", &["youtube", "instagram", "twitter"]),
            &links(&["longform"]),
            &[],
        );
        let youtube = find(&plan, "youtube").metadata.as_ref().expect("youtube metadata");
        assert_eq!(youtube["youtube"]["privacy"], "public");
        assert_eq!(youtube["youtube"]["notifySubscribers"], true);
        assert_eq!(youtube["youtube"]["title"], "A Title");
        let instagram = find(&plan, "instagram").metadata.as_ref().expect("instagram metadata");
        assert_eq!(instagram["instagram"]["type"], "reel");
        assert_eq!(instagram["instagram"]["shouldShareToFeed"], true);
        assert!(find(&plan, "twitter").metadata.is_none());
    }

    #[test]
    fn planned_text_carries_the_rendered_tags_and_its_hash() {
        let plan = plan_for(
            &manifest("longform", &["instagram"]),
            &links(&["longform"]),
            &[],
        );
        let item = find(&plan, "instagram");
        assert_eq!(item.text, "body copy\n\n#rust #agents");
        assert_eq!(item.copy_hash, copy_hash(&item.text, item.title.as_deref()));
    }

    #[test]
    fn ordering_puts_longform_first_then_chapters_ascending() {
        let posts = PostsManifest {
            version: Some(3),
            prompt_version: Some(2),
            prompt_hash: "feedfacefeedface".into(),
            items: vec![
                vp("chapter-02", &["tiktok"]),
                vp("chapter-01", &["tiktok", "instagram"]),
                vp("longform", &["youtube"]),
            ],
        };
        let plan = plan_for(&posts, &links(&["longform", "chapter-01", "chapter-02"]), &[]);
        let order: Vec<(&str, &str)> = plan
            .items
            .iter()
            .map(|i| (i.video_id.as_str(), i.platform.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("longform", "youtube"),
                ("chapter-01", "instagram"),
                ("chapter-01", "tiktok"),
                ("chapter-02", "tiktok"),
            ]
        );
    }

    /// Nothing the planner builds is pre-approved, and nothing asks Buffer to
    /// hold it either — the tab is the only gate.
    #[test]
    fn a_fresh_plan_is_never_approved_and_never_gates_at_buffer() {
        let plan = plan_for(
            &manifest("longform", &["twitter", "linkedin"]),
            &links(&["longform"]),
            &[],
        );
        assert!(plan.items.iter().all(|item| !item.approved));
        assert!(plan.items.iter().all(|item| !item.needs_approval));
        assert_eq!(plan.sendable().count(), 0);
        assert_eq!(plan.queueable().count(), 2);
    }

    #[test]
    fn og_artwork_is_an_image_post_and_a_new_image_changes_approval_identity() {
        let mut links = links_with_thumbnail();
        links.items.push(DistributedAsset { id: "og-image".into(), kind: "image".into(), orientation: Some("landscape".into()), chapter: None, url: "https://cdn.example.com/og-1.jpg".into(), file: None });
        let posts = manifest("longform", &["facebook", "linkedin"]);
        let plan = plan_for(&posts, &links, &[]);
        let item = find(&plan, "facebook");
        assert!(item.image);
        assert_eq!(item.metadata.as_ref().unwrap()["facebook"]["type"], "post");
        let body = super::super::buffer::create_post_variables(&super::super::send::post_input(item));
        assert_eq!(body["input"]["assets"][0]["image"]["url"], "https://cdn.example.com/og-1.jpg");
        assert!(body["input"]["assets"][0].get("video").is_none());
        links.items.last_mut().unwrap().url = "https://cdn.example.com/og-2.jpg".into();
        let changed = plan_for(&posts, &links, &[]);
        assert_ne!(item.copy_hash, find(&changed, "facebook").copy_hash);
    }

}
