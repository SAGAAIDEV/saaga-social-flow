//! Which Buffer channel a posts.json platform targets, and whether it can take a post.
//!
//! Selection is deterministic and never hardcodes an id: candidates come from the
//! live `channels()` query, are ranked (the named Twitter handle and the LinkedIn
//! brand page win), and ties break on the lowest channel id so two runs always agree.
//! The account has two Twitter profiles and two LinkedIn destinations, so a naive
//! one-to-one platform map would silently drop one of each.

use super::buffer::Channel;

/// Twitter profile this project posts from. The account has two, and alphabetical
/// order picks the *other* one, so the target has to be named rather than derived.
/// `BUFFER_TWITTER_HANDLE` moves it without a rebuild.
pub const DEFAULT_TWITTER_HANDLE: &str = "AndrewOsee59559";

/// Buffer service a posts.json platform string targets.
/// `youtube_shorts` is not its own channel — it rides the youtube channel.
pub fn service_for(platform: &str) -> &str {
    match platform {
        "youtube" | "youtube_shorts" => "youtube",
        other => other,
    }
}

/// The YouTube flavour a video actually is, from its shape rather than its label.
///
/// Both flavours go to the same channel, and `YoutubePostMetadataInput` has no
/// field for which one it is — so YouTube decides from the file: vertical and
/// three minutes or under is a Short, anything else is a regular upload. Nothing
/// we send can override that.
///
/// Which means the label has one job, to be *true*, and it was the one thing here
/// a language model chose: posts.json is written by one, and a longform tagged
/// `youtube_shorts` would have been queued, published as a regular video anyway,
/// and described wrongly everywhere in between. So the label is derived from the
/// orientation the video was distributed with, and the model's guess is only a
/// starting point.
pub fn youtube_flavour<'a>(platform: &'a str, orientation: Option<&str>) -> &'a str {
    debug_assert_eq!(service_for(platform), "youtube");
    match orientation {
        Some("portrait") => "youtube_shorts",
        Some(_) => "youtube",
        // No orientation recorded — an older links.json. There is nothing to
        // correct against, so the label stands rather than being overwritten with
        // a guess, which is the one way this could make a right label wrong.
        None => platform,
    }
}

/// The handle that wins when a service has more than one channel connected.
///
/// A leading `@` is dropped: the setting is documented as `@yourhandle`, Buffer
/// names channels without one, and an exact comparison would silently fall back
/// to the other profile.
pub fn preferred_handle(platform: &str) -> Option<String> {
    match service_for(platform) {
        "twitter" => Some(
            std::env::var("BUFFER_TWITTER_HANDLE")
                .ok()
                .map(|v| bare_handle(&v).to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_TWITTER_HANDLE.to_string()),
        ),
        _ => None,
    }
}

/// `@handle` and `handle` are the same handle.
fn bare_handle(handle: &str) -> &str {
    handle.trim().trim_start_matches('@')
}

/// Picks the channel for a platform, or the reason there is none.
pub fn resolve_channel<'a>(platform: &str, channels: &'a [Channel]) -> Result<&'a Channel, String> {
    resolve_preferring(
        service_for(platform),
        preferred_handle(platform).as_deref(),
        channels,
    )
}

/// The pure core of [`resolve_channel`], with the preference passed in rather than
/// read from the environment, so the ranking is testable without touching process env.
pub fn resolve_preferring<'a>(
    service: &str,
    preferred: Option<&str>,
    channels: &'a [Channel],
) -> Result<&'a Channel, String> {
    let mut candidates: Vec<&Channel> = channels.iter().filter(|c| c.service == service).collect();
    candidates.sort_by(|a, b| {
        candidate_rank(a, preferred)
            .cmp(&candidate_rank(b, preferred))
            .then(a.id.cmp(&b.id))
    });
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("no {service} channel connected to Buffer"))
}

/// True when this channel answers to the wanted handle, by either name.
pub fn handle_matches(channel: &Channel, handle: &str) -> bool {
    let wanted = bare_handle(handle);
    bare_handle(&channel.name).eq_ignore_ascii_case(wanted)
        || bare_handle(&channel.display_name).eq_ignore_ascii_case(wanted)
}

/// `Some(note)` when a handle was named but the pick is a fallback. A renamed or
/// removed handle has to surface in the plan — silently retargeting to the other
/// profile posts to the wrong audience with no warning anywhere.
pub fn handle_note(platform: &str, picked: &Channel) -> Option<String> {
    let wanted = preferred_handle(platform)?;
    (!handle_matches(picked, &wanted))
        .then(|| format!("wanted @{wanted}, fell back to @{}", channel_label(picked)))
}

/// `Some(note)` when the pick only won because another channel of the same
/// service is disconnected or paused. The live one is used — a dead channel
/// blocking every post while a working one sits idle helps nobody — but the
/// plan has to say so, because the post is now going somewhere else.
pub fn passed_over_note(picked: &Channel, channels: &[Channel]) -> Option<String> {
    let blocked: Vec<String> = channels
        .iter()
        .filter(|other| other.service == picked.service && other.id != picked.id)
        .filter_map(channel_block)
        .collect();
    (!blocked.is_empty() && channel_block(picked).is_none()).then(|| {
        format!(
            "fell back to @{}: {}",
            channel_label(picked),
            blocked.join("; ")
        )
    })
}

/// Lower sorts first. A channel that cannot take a post sorts after every one
/// that can, whatever its name — see [`passed_over_note`]. Among the live ones:
/// the named handle, then the LinkedIn brand page.
fn candidate_rank(channel: &Channel, preferred: Option<&str>) -> (u8, u8) {
    let blocked = u8::from(channel_block(channel).is_some());
    if let Some(handle) = preferred {
        return (blocked, u8::from(!handle_matches(channel, handle)));
    }
    let preference = match (channel.service.as_str(), channel.kind.as_str()) {
        ("linkedin", "page") => 0,
        ("linkedin", _) => 1,
        _ => 0,
    };
    (blocked, preference)
}

pub fn channel_label(channel: &Channel) -> &str {
    if channel.display_name.trim().is_empty() {
        &channel.name
    } else {
        &channel.display_name
    }
}

/// A resolved channel that still cannot take a post.
pub fn channel_block(channel: &Channel) -> Option<String> {
    let name = channel_label(channel);
    if channel.is_disconnected {
        return Some(format!("channel \"{name}\" is disconnected in Buffer"));
    }
    if channel.is_queue_paused {
        return Some(format!("channel \"{name}\" has a paused queue in Buffer"));
    }
    None
}

/// Buffer `SchedulingType`: reminder-only channels cannot be published for you.
pub fn scheduling_type_for(channel: &Channel) -> &'static str {
    if channel.default_to_reminders == Some(true) {
        "notification"
    } else {
        "automatic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn both_linkedin() -> Vec<Channel> {
        vec![
            channel("69267c5429ea336fd631f864", "linkedin", "profile", "amovfx"),
            channel("69267c5429ea336fd631f865", "linkedin", "page", "saagasolve"),
        ]
    }

    #[test]
    fn youtube_shorts_rides_the_youtube_channel() {
        assert_eq!(service_for("youtube_shorts"), "youtube");
        assert_eq!(service_for("youtube"), "youtube");
        assert_eq!(service_for("tiktok"), "tiktok");
    }

    #[test]
    fn linkedin_prefers_the_page_even_when_the_profile_sorts_first() {
        let channels = both_linkedin();
        let picked = resolve_channel("linkedin", &channels).expect("a linkedin channel");
        assert_eq!(picked.id, "69267c5429ea336fd631f865");
        assert_eq!(channel_label(picked), "saagasolve");
    }

    fn both_twitter() -> Vec<Channel> {
        vec![
            channel(
                "6a4a9bbf40483446287252ef",
                "twitter",
                "profile",
                "AndrewOsee59559",
            ),
            channel(
                "69266cd829ea336fd631d03a",
                "twitter",
                "profile",
                "amelnychukoseen",
            ),
        ]
    }

    /// The named handle has to beat id order — `amelnychukoseen` sorts first, so
    /// an alphabetical tie-break silently posts to the wrong profile.
    #[test]
    fn twitter_picks_the_named_handle_over_the_lower_id() {
        let channels = both_twitter();
        let picked = resolve_preferring("twitter", Some(DEFAULT_TWITTER_HANDLE), &channels)
            .expect("a twitter channel");
        assert_eq!(picked.id, "6a4a9bbf40483446287252ef");
        assert_eq!(channel_label(picked), "AndrewOsee59559");
        assert!(
            channels[1].id < channels[0].id,
            "the other handle sorts first"
        );
    }

    #[test]
    fn the_handle_match_ignores_case() {
        let channels = both_twitter();
        let picked = resolve_preferring("twitter", Some("andrewosee59559"), &channels)
            .expect("a twitter channel");
        assert_eq!(picked.id, "6a4a9bbf40483446287252ef");
    }

    /// The setting is documented as `@yourhandle`; the `@` must not turn the
    /// named profile into a silent fallback to the other one.
    #[test]
    fn the_handle_match_tolerates_a_leading_at() {
        let channels = both_twitter();
        let picked = resolve_preferring("twitter", Some("@AndrewOsee59559"), &channels)
            .expect("a twitter channel");
        assert_eq!(picked.id, "6a4a9bbf40483446287252ef");
        assert!(handle_matches(&channels[0], "@andrewosee59559"));
        assert_eq!(bare_handle("  @Name "), "Name");
        assert_eq!(bare_handle("Name"), "Name");
    }

    /// The live account: a disconnected Instagram business profile with the
    /// lower id, and a connected personal profile. Id order picked the dead one
    /// and every Instagram post was skipped while a working channel sat idle.
    #[test]
    fn a_disconnected_channel_loses_to_a_live_one_and_the_plan_says_so() {
        let mut channels = vec![
            channel(
                "6a3dbb795ab6d2f10671b945",
                "instagram",
                "business",
                "saagasocials",
            ),
            channel(
                "6a825edcccaf649a67bd8289",
                "instagram",
                "profile",
                "amelnychukoseen",
            ),
        ];
        assert_eq!(
            resolve_preferring("instagram", None, &channels).unwrap().id,
            "6a3dbb795ab6d2f10671b945",
            "both live: lowest id wins as before"
        );
        channels[0].is_disconnected = true;
        let picked = resolve_preferring("instagram", None, &channels).expect("the live one");
        assert_eq!(picked.id, "6a825edcccaf649a67bd8289");
        assert_eq!(
            passed_over_note(picked, &channels).as_deref(),
            Some(
                "fell back to @amelnychukoseen: channel \"saagasocials\" is disconnected in Buffer"
            )
        );
        // A paused queue is a block too, and the note names it.
        channels[0].is_disconnected = false;
        channels[0].is_queue_paused = true;
        let picked = resolve_preferring("instagram", None, &channels).expect("the live one");
        assert_eq!(picked.id, "6a825edcccaf649a67bd8289");
        assert!(passed_over_note(picked, &channels)
            .unwrap()
            .contains("has a paused queue"));
        // With nothing blocked there is nothing to note.
        channels[0].is_queue_paused = false;
        let picked = resolve_preferring("instagram", None, &channels).unwrap();
        assert_eq!(passed_over_note(picked, &channels), None);
    }

    /// When every channel of a service is blocked the best of them is still
    /// returned, so the plan reports the block rather than "no channel".
    #[test]
    fn an_all_blocked_service_still_resolves_so_the_block_is_reported() {
        let mut channels = both_twitter();
        for channel in &mut channels {
            channel.is_disconnected = true;
        }
        let picked = resolve_preferring("twitter", Some(DEFAULT_TWITTER_HANDLE), &channels)
            .expect("still a channel");
        assert_eq!(
            picked.id, "6a4a9bbf40483446287252ef",
            "the named one, still"
        );
        assert!(channel_block(picked).is_some());
        assert_eq!(
            passed_over_note(picked, &channels),
            None,
            "nothing fell back"
        );
    }

    /// A named handle that is disconnected gives way to the live profile, and
    /// both notes say what happened: which was wanted, and why it lost.
    #[test]
    fn a_disconnected_named_handle_gives_way_to_the_live_profile() {
        let mut channels = both_twitter();
        channels[0].is_disconnected = true;
        let picked = resolve_preferring("twitter", Some(DEFAULT_TWITTER_HANDLE), &channels)
            .expect("the live profile");
        assert_eq!(picked.id, "69266cd829ea336fd631d03a");
        assert!(passed_over_note(picked, &channels)
            .unwrap()
            .contains("\"AndrewOsee59559\" is disconnected"));
    }

    /// A renamed handle must not silently retarget the other profile.
    #[test]
    fn an_unmatched_handle_falls_back_by_id_and_says_so() {
        let channels = both_twitter();
        let picked =
            resolve_preferring("twitter", Some("gone-handle"), &channels).expect("a fallback");
        assert_eq!(picked.id, "69266cd829ea336fd631d03a");
        assert_eq!(
            handle_note("twitter", picked).as_deref(),
            Some("wanted @AndrewOsee59559, fell back to @amelnychukoseen"),
            "the note reports the configured handle, not the probe"
        );
        let wanted = resolve_preferring("twitter", Some(DEFAULT_TWITTER_HANDLE), &channels)
            .expect("a twitter channel");
        assert_eq!(handle_note("twitter", wanted), None);
    }

    #[test]
    fn only_twitter_names_a_preferred_handle() {
        assert_eq!(
            preferred_handle("twitter").as_deref(),
            Some(DEFAULT_TWITTER_HANDLE)
        );
        assert_eq!(preferred_handle("linkedin"), None);
        assert_eq!(preferred_handle("youtube_shorts"), None);
    }

    #[test]
    fn a_service_with_no_channel_explains_itself() {
        let channels = both_linkedin();
        assert_eq!(
            resolve_channel("facebook", &channels).unwrap_err(),
            "no facebook channel connected to Buffer"
        );
    }

    #[test]
    fn a_label_falls_back_to_the_name_when_the_display_name_is_blank() {
        let mut bare = channel("id", "bluesky", "profile", "saaga-dev.bsky.social");
        bare.display_name = "  ".into();
        assert_eq!(channel_label(&bare), "saaga-dev.bsky.social");
    }

    #[test]
    fn disconnected_and_paused_channels_block_with_a_named_reason() {
        let mut live = channel("id", "tiktok", "account", "andrewmelnychukos");
        assert_eq!(channel_block(&live), None);
        live.is_queue_paused = true;
        assert_eq!(
            channel_block(&live).as_deref(),
            Some("channel \"andrewmelnychukos\" has a paused queue in Buffer")
        );
        live.is_disconnected = true;
        // Disconnected wins: it is the reason to fix first.
        assert_eq!(
            channel_block(&live).as_deref(),
            Some("channel \"andrewmelnychukos\" is disconnected in Buffer")
        );
    }

    #[test]
    fn scheduling_type_follows_default_to_reminders() {
        let mut ig = channel("id", "instagram", "business", "saagasocials");
        assert_eq!(scheduling_type_for(&ig), "automatic");
        ig.default_to_reminders = Some(false);
        assert_eq!(scheduling_type_for(&ig), "automatic");
        ig.default_to_reminders = Some(true);
        assert_eq!(scheduling_type_for(&ig), "notification");
    }
}
