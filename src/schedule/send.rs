//! Turning a reviewed plan item into the two things a send produces: the Buffer
//! mutation input, and the ledger row proving it went out.
//!
//! Nothing here decides anything. `mode`, `scheduling_type` and `metadata` ride
//! through untouched, so the payload on the wire is the payload the Schedule tab
//! showed and a human ticked — inventing a value at send time would make the
//! review step a lie.

use super::buffer::CreatePostInput;
use super::ledger;
use super::schema::{self, PlanItem, ScheduleRow};

/// One plan item as the Buffer mutation input.
pub fn post_input(item: &PlanItem) -> CreatePostInput {
    CreatePostInput {
        channel_id: item.channel_id.clone(),
        text: item.text.clone(),
        mode: item.mode.clone(),
        scheduling_type: item.scheduling_type.clone(),
        needs_approval: item.needs_approval,
        image: item.image,
        video_url: item.url.clone(),
        video_title: item.title.clone(),
        ai_assisted: true,
        metadata: item.metadata.clone(),
    }
}

/// The ledger row for an item that just reached Buffer, keyed so the next plan can
/// recognise the same copy on the same channel.
pub fn row_for(item: &PlanItem, post_id: &str, project: &str, version: Option<u32>) -> ScheduleRow {
    ScheduleRow {
        id: schema::row_id(&item.video_id, &item.platform, &item.copy_hash),
        buffer_post_id: post_id.to_string(),
        project: project.to_string(),
        version,
        video_id: item.video_id.clone(),
        platform: item.platform.clone(),
        channel_id: item.channel_id.clone(),
        url: item.url.clone(),
        prompt_id: item.prompt_id.clone(),
        prompt_version: item.prompt_version,
        copy_hash: item.copy_hash.clone(),
        queued_at: ledger::now_rfc3339(),
        // A row is only written when the post reached Buffer, so it starts live.
        deleted_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::buffer::create_post_variables;
    use super::super::meta;
    use super::*;

    fn item(platform: &str, title: Option<&str>) -> PlanItem {
        PlanItem {
            video_id: "chapter-01".into(),
            platform: platform.into(),
            channel_id: "6a3dbb795ab6d2f10671b945".into(),
            channel_name: "saagasocials".into(),
            url: "https://cdn.example.com/vertical/chapter-01.mp4".into(),
            text: "body copy\n\n#rust".into(),
            title: title.map(str::to_string),
            mode: "addToQueue".into(),
            scheduling_type: "notification".into(),
            needs_approval: false,
            image: false,
            metadata: meta::metadata_for(platform, title, "28"),
            reason: "vertical chapter".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(2),
            copy_hash: "0badc0de0badc0de".into(),
            skip: None,
            approved: true,
        }
    }

    #[test]
    fn post_input_carries_the_planned_channel_copy_and_metadata() {
        let planned = item("instagram", Some("Chapter One"));
        let input = post_input(&planned);
        assert_eq!(input.channel_id, "6a3dbb795ab6d2f10671b945");
        assert_eq!(input.mode, "addToQueue");
        assert_eq!(input.scheduling_type, "notification");
        assert_eq!(
            input.video_url,
            "https://cdn.example.com/vertical/chapter-01.mp4"
        );
        assert_eq!(input.video_title.as_deref(), Some("Chapter One"));
        assert!(input.ai_assisted, "every planned post is LLM-written");
        // The gate is the Schedule tab, so Buffer is never asked to hold anything.
        assert!(!input.needs_approval);
        // Nothing is invented at send time: the reviewed metadata is the payload.
        assert_eq!(input.metadata, planned.metadata);
    }

    #[test]
    fn a_planned_item_serializes_into_a_valid_create_post_payload() {
        let vars = create_post_variables(&post_input(&item("instagram", Some("Chapter One"))));
        let input = &vars["input"];
        assert_eq!(input["mode"], "addToQueue");
        assert_eq!(input["schedulingType"], "notification");
        assert_eq!(input["metadata"]["instagram"]["type"], "reel");
        assert_eq!(input["metadata"]["instagram"]["shouldShareToFeed"], true);
        assert_eq!(
            input["assets"][0]["video"]["url"],
            "https://cdn.example.com/vertical/chapter-01.mp4"
        );
    }

    /// The whole payload, pinned. Every key here was introspected against the live
    /// api; a stray `source`, an uppercase enum or a missing `schedulingType` is a
    /// rejected mutation, and this is the one place that shows all of them at once.
    #[test]
    fn the_wire_payload_is_exactly_what_buffer_accepts() {
        let vars = create_post_variables(&post_input(&item("instagram", Some("Chapter One"))));
        assert_eq!(
            vars,
            serde_json::json!({
                "input": {
                    "channelId": "6a3dbb795ab6d2f10671b945",
                    "text": "body copy\n\n#rust",
                    "mode": "addToQueue",
                    "schedulingType": "notification",
                    "needsApproval": false,
                    "aiAssisted": true,
                    "assets": [{
                        "video": {
                            "url": "https://cdn.example.com/vertical/chapter-01.mp4",
                            "metadata": { "title": "Chapter One" }
                        }
                    }],
                    "metadata": { "instagram": { "type": "reel", "shouldShareToFeed": true } }
                }
            })
        );
    }

    #[test]
    fn youtube_items_ship_the_public_notifying_metadata_the_plan_showed() {
        let planned = item("youtube", Some("The Long One"));
        let meta = planned.metadata.clone().expect("youtube metadata");
        assert_eq!(meta["youtube"]["privacy"], "public");
        assert_eq!(meta["youtube"]["notifySubscribers"], true);
        let vars = create_post_variables(&post_input(&planned));
        assert_eq!(vars["input"]["metadata"], meta);
    }

    #[test]
    fn row_for_keys_on_video_platform_and_copy() {
        let planned = item("tiktok", None);
        let row = row_for(&planned, "post-9", "vd-42-demo", Some(3));
        assert_eq!(row.id, "chapter-01:tiktok:0badc0de0badc0de");
        assert_eq!(row.buffer_post_id, "post-9");
        assert_eq!(row.project, "vd-42-demo");
        assert_eq!(row.version, Some(3));
        assert_eq!(row.url, planned.url);
        assert_eq!(row.prompt_id, "posts.social");
        assert!(row.queued_at.ends_with('Z'));
    }
}
