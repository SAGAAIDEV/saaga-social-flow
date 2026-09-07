//! Carrying approval ticks across a re-plan.
//!
//! Approval is the only thing standing between generated copy and a live post, so
//! it is bound to the copy itself rather than to a row position. Re-planning keeps
//! a tick when the caption is byte-identical and drops it when anything changed —
//! otherwise you could approve caption A in the preview and ship caption B.

use super::schema::SchedulePlan;

/// Carries approval ticks from the previous plan onto a freshly built one.
///
/// Matched on (video_id, platform, copy_hash), so a tick survives a re-plan only
/// while the copy is byte-identical. Regenerate the posts or edit a caption and the
/// hash moves, the tick does not come with it, and the item goes back for review —
/// which is the property that makes the preview trustworthy.
pub fn carry_approvals(plan: &mut SchedulePlan, prior: &SchedulePlan) {
    for item in &mut plan.items {
        item.approved = prior.items.iter().any(|old| {
            old.approved
                && old.video_id == item.video_id
                && old.platform == item.platform
                && old.copy_hash == item.copy_hash
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::schema::PlanItem;

    fn item(video_id: &str, platform: &str, hash: &str) -> PlanItem {
        PlanItem {
            video_id: video_id.into(),
            platform: platform.into(),
            channel_id: "id".into(),
            channel_name: "channel".into(),
            url: "https://example.com/v.mp4".into(),
            text: "copy".into(),
            title: None,
            mode: "addToQueue".into(),
            scheduling_type: "automatic".into(),
            needs_approval: false,
            image: false,
            metadata: None,
            reason: "hub video".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: hash.into(),
            skip: None,
            approved: false,
        }
    }

    fn plan(items: Vec<PlanItem>) -> SchedulePlan {
        SchedulePlan { project: "vd-42".into(), version: Some(3), items }
    }

    fn approved(items: Vec<PlanItem>) -> SchedulePlan {
        let mut plan = plan(items);
        plan.items.iter_mut().for_each(|item| item.approved = true);
        plan
    }

    #[test]
    fn an_identical_caption_keeps_its_tick() {
        let prior = approved(vec![
            item("longform", "youtube", "aaaa"),
            item("longform", "twitter", "bbbb"),
        ]);
        let mut replanned = plan(vec![
            item("longform", "youtube", "aaaa"),
            item("longform", "twitter", "bbbb"),
        ]);
        carry_approvals(&mut replanned, &prior);
        assert_eq!(replanned.sendable().count(), 2);
    }

    #[test]
    fn changed_copy_drops_the_tick_and_leaves_the_rest_alone() {
        let prior = approved(vec![
            item("longform", "youtube", "aaaa"),
            item("longform", "twitter", "bbbb"),
        ]);
        // The YouTube caption was regenerated; the Twitter one was not.
        let mut replanned = plan(vec![
            item("longform", "youtube", "cccc"),
            item("longform", "twitter", "bbbb"),
        ]);
        carry_approvals(&mut replanned, &prior);
        assert!(!replanned.items[0].approved, "changed copy comes back for review");
        assert!(replanned.items[1].approved, "untouched copy keeps its tick");
        assert_eq!(replanned.sendable().count(), 1);
    }

    /// A tick approves one caption on one channel — not the video everywhere.
    #[test]
    fn approval_does_not_leak_across_platforms_or_videos() {
        let mut prior = plan(vec![
            item("longform", "youtube", "aaaa"),
            item("longform", "twitter", "aaaa"),
            item("chapter-01", "youtube", "aaaa"),
        ]);
        prior.items[0].approved = true;

        let mut replanned = plan(vec![
            item("longform", "youtube", "aaaa"),
            item("longform", "twitter", "aaaa"),
            item("chapter-01", "youtube", "aaaa"),
        ]);
        carry_approvals(&mut replanned, &prior);
        assert!(replanned.items[0].approved);
        assert!(!replanned.items[1].approved, "same copy, different platform");
        assert!(!replanned.items[2].approved, "same copy, different video");
    }

    #[test]
    fn an_unapproved_prior_plan_approves_nothing() {
        let prior = plan(vec![item("longform", "youtube", "aaaa")]);
        let mut replanned = approved(vec![item("longform", "youtube", "aaaa")]);
        carry_approvals(&mut replanned, &prior);
        assert_eq!(
            replanned.sendable().count(),
            0,
            "the prior plan is the authority, so a stale tick is cleared"
        );
    }

    #[test]
    fn an_item_with_no_prior_match_is_left_unapproved() {
        let prior = approved(vec![item("longform", "youtube", "aaaa")]);
        let mut replanned = plan(vec![item("chapter-02", "tiktok", "dddd")]);
        carry_approvals(&mut replanned, &prior);
        assert!(!replanned.items[0].approved);
    }
}
