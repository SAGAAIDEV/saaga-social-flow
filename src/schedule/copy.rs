//! The text a queued post carries, and its version-stable fingerprint.
//!
//! [`copy_hash`] is the dedupe key written into `schedule.jsonl`, so it must mean
//! the same thing next year as it does today. `DefaultHasher` cannot promise that
//! — std documents its algorithm as unspecified and free to change between
//! releases — so this is an explicit FNV-1a/64 over the bytes, pinned by
//! known-answer tests. A digest that drifts silently re-queues the whole back
//! catalogue, which is why the constants live here in the open.

/// FNV-1a 64-bit offset basis and prime.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// ASCII unit separator, so "ab" + "c" never collides with "a" + "bc".
const SEPARATOR: u8 = 0x1f;

/// Stable fingerprint of one post's copy: the body text plus its optional title.
/// Editing either produces a different key, which makes the item queueable again.
pub fn copy_hash(text: &str, title: Option<&str>) -> String {
    let mut hash = fnv1a(FNV_OFFSET, text.as_bytes());
    hash = fnv1a(hash, &[SEPARATOR]);
    // A present-but-empty title is not the same as no title, so tag the branch.
    hash = match title {
        Some(title) => fnv1a(fnv1a(hash, &[1]), title.as_bytes()),
        None => fnv1a(hash, &[0]),
    };
    format!("{hash:016x}")
}

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The hard caption limits a network enforces at publish time. Buffer accepts
/// the post and fails it later, in a status this app only learned to read in
/// September 2026 — so the plan says so up front instead.
///
/// Approximate on purpose: X counts a URL as 23 characters whatever its length
/// and some accounts may post longer, so this is a warning in the reason, never
/// a skip. Characters, not bytes, because that is what the networks count.
pub fn length_note(platform: &str, text: &str) -> Option<String> {
    let limit = match platform {
        "twitter" => 280,
        "bluesky" => 300,
        _ => return None,
    };
    let chars = text.chars().count();
    (chars > limit).then(|| {
        format!(
            "warning: {chars} characters, {} over the {platform} limit of {limit}",
            chars - limit
        )
    })
}

/// Mirrors the markdown body written by `posts::schema::save_manifest`:
/// content, blank line, then tags rendered as `#tag` unless already prefixed.
pub fn render_text(content: &str, tags: &[String]) -> String {
    let mut text = content.to_string();
    if !tags.is_empty() {
        text.push_str("\n\n");
        let rendered = tags
            .iter()
            .map(|t| {
                if t.starts_with('#') {
                    t.clone()
                } else {
                    format!("#{t}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        text.push_str(&rendered);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known answers, computed independently from the FNV-1a spec. These pin the
    /// on-disk dedupe key: if a change here breaks them, every ledger row in every
    /// project stops matching and the back catalogue re-queues.
    #[test]
    fn copy_hash_matches_its_pinned_digests() {
        assert_eq!(copy_hash("body copy", Some("A Title")), "30505c9acb242fe7");
        assert_eq!(copy_hash("body copy", None), "42ef82d0861f20a9");
        assert_eq!(copy_hash("", None), "0879e607b528124a");
    }

    #[test]
    fn copy_hash_follows_the_text_and_the_title() {
        let a = copy_hash("body copy", Some("A Title"));
        assert_eq!(a, copy_hash("body copy", Some("A Title")));
        assert_ne!(a, copy_hash("body copy edited", Some("A Title")));
        assert_ne!(a, copy_hash("body copy", Some("Another Title")));
        assert_ne!(a, copy_hash("body copy", None));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn an_empty_title_is_not_a_missing_title() {
        assert_ne!(
            copy_hash("body copy", Some("")),
            copy_hash("body copy", None)
        );
    }

    #[test]
    fn the_separator_keeps_the_text_and_title_apart() {
        // Without a separator byte these two would hash the same bytes in a row.
        assert_ne!(copy_hash("ab", Some("c")), copy_hash("a", Some("bc")));
    }

    #[test]
    fn over_length_captions_are_flagged_for_the_networks_that_reject_them() {
        let long = "x".repeat(281);
        assert_eq!(
            length_note("twitter", &long).as_deref(),
            Some("warning: 281 characters, 1 over the twitter limit of 280")
        );
        assert_eq!(length_note("twitter", &"x".repeat(280)), None);
        assert!(length_note("bluesky", &"y".repeat(301)).is_some());
        assert_eq!(length_note("bluesky", &"y".repeat(300)), None);
        // Counted in characters: an emoji is one, not four.
        assert_eq!(length_note("twitter", &"🚀".repeat(280)), None);
        // Networks with room to spare, or none we know, say nothing.
        for platform in [
            "linkedin",
            "instagram",
            "tiktok",
            "youtube_shorts",
            "facebook",
            "",
        ] {
            assert_eq!(length_note(platform, &long), None, "{platform}");
        }
    }

    #[test]
    fn text_appends_tags_after_a_blank_line() {
        let tags = vec!["rust".to_string(), "#agents".to_string()];
        assert_eq!(
            render_text("body copy", &tags),
            "body copy\n\n#rust #agents"
        );
    }

    #[test]
    fn text_without_tags_is_the_content_verbatim() {
        assert_eq!(render_text("body copy", &[]), "body copy");
    }
}
