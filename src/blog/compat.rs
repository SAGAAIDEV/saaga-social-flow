//! What an older CMS cannot accept, and what to do about it.
//!
//! The schema this uploader writes against lives in `strapi-cms`, and the
//! instance receiving the post is whatever was last deployed from it. Those two
//! drift: `keywordTargets`, `keywords`, `ogImage`, `h1`, `faq`, `noIndex`,
//! `canonicalUrl`, `videoVertical` and `thumbnailVertical` are all in the local
//! schema and none of them are on `cms.saagasolve.com` today, because the
//! commits that added them have not been deployed.
//!
//! Strapi refuses such a body outright — `400 ValidationError`, `Invalid key
//! keywordTargets` — and names **one** key per response, so a post carrying
//! five unknown fields fails five times before it lands. That is what this
//! module is for: read the key out of the refusal, drop it, and try again,
//! keeping a list of everything that had to go so the caller can say so.
//!
//! Two things it deliberately does not do. It never drops a field on a refusal
//! it does not recognise — an enum value the CMS rejects is a bug in the post,
//! not a schema gap, and quietly deleting the field would publish the wrong
//! page. And it never treats the degraded post as the record: the article on
//! disk keeps every field, so a later deploy plus a re-publish fills them in.

use serde_json::Value;

/// The structured brief, and the flat list it degrades into.
const KEYWORD_TARGETS: &str = "keywordTargets";
const KEYWORDS: &str = "keywords";

/// A bound on the retry loop. Well above the nine fields that can currently go
/// missing, and low enough that a CMS answering `Invalid key` forever stops.
pub const MAX_DROPPED: usize = 12;

/// The field name out of a `400 Invalid key <field>` refusal, and only that.
///
/// Matched on all three of status, name and the `details.key`/`message` pair
/// rather than on the message alone: `ValidationError` is also what a bad enum
/// value and a missing required field come back as, and neither is fixed by
/// deleting the field.
pub fn unknown_key(code: u16, detail: &str) -> Option<String> {
    if code != 400 {
        return None;
    }
    let error: Value = serde_json::from_str(detail).ok()?;
    let error = &error["error"];
    if error["name"] != "ValidationError" || error["details"]["source"] != "body" {
        return None;
    }
    let key = error["details"]["key"].as_str()?;
    // The message names the same key. A `details.key` that the message does not
    // corroborate is a different error wearing a similar shape.
    match error["message"].as_str()? == format!("Invalid key {key}") {
        true => Some(key.to_string()),
        false => None,
    }
}

/// Removes `key` from the body, carrying what can be saved into a field that
/// survives. `false` when the key was not there to remove — which means the
/// refusal is about something this cannot fix, and the caller must stop rather
/// than send the identical body again.
///
/// The one rescue is `keywordTargets`: its terms are the flat `keywords` list
/// with the priorities and intents stripped off, so they move there. Skipped
/// when `keywords` has itself already been refused, both because re-adding it
/// would fail again and because it would loop.
pub fn drop_field(payload: &mut Value, key: &str, dropped: &[String]) -> bool {
    let Some(data) = payload.get_mut("data").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(removed) = data.remove(key) else {
        return false;
    };
    if key == KEYWORD_TARGETS && !dropped.iter().any(|gone| gone == KEYWORDS) {
        let mut keywords = data
            .get(KEYWORDS)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for target in removed.as_array().unwrap_or(&Vec::new()) {
            let Some(term) = target["term"].as_str().map(str::trim).filter(|t| !t.is_empty())
            else {
                continue;
            };
            let known = keywords
                .iter()
                .any(|had| had.as_str().is_some_and(|had| had.eq_ignore_ascii_case(term)));
            if !known {
                keywords.push(Value::String(term.to_string()));
            }
        }
        if !keywords.is_empty() {
            data.insert(KEYWORDS.into(), Value::Array(keywords));
        }
    }
    true
}

/// What to tell whoever published, or `None` when nothing had to go.
///
/// Names the fields rather than counting them: "the CMS is behind" is not
/// actionable, and "it has no ogImage" is — it says which deploy is missing and
/// what the live page will be short of until it happens.
pub fn warning(dropped: &[String]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    let fields = dropped.join(", ");
    let rescued = dropped.iter().any(|gone| gone == KEYWORD_TARGETS)
        && !dropped.iter().any(|gone| gone == KEYWORDS);
    let mut warning = format!(
        "this CMS has no {fields} — dropped so the post could land, and still \
         in the local article; deploy the CMS schema and re-publish to fill them"
    );
    if rescued {
        warning.push_str(" (the target terms were saved in keywords)");
    }
    Some(warning)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refusal(key: &str) -> String {
        json!({
            "data": null,
            "error": {
                "status": 400,
                "name": "ValidationError",
                "message": format!("Invalid key {key}"),
                "details": { "key": key, "source": "body" }
            }
        })
        .to_string()
    }

    #[test]
    fn the_refused_field_is_read_off_the_error() {
        assert_eq!(
            unknown_key(400, &refusal(KEYWORD_TARGETS)),
            Some(KEYWORD_TARGETS.to_string())
        );
        assert_eq!(unknown_key(400, &refusal("ogImage")), Some("ogImage".into()));
    }

    /// Every one of these is a real refusal that deleting a field would not fix
    /// — and would corrupt the post by trying.
    #[test]
    fn nothing_else_is_treated_as_a_missing_field() {
        let enum_refusal = json!({
            "error": {
                "status": 400, "name": "ValidationError",
                "message": "intent must be one of the following values: informational, commercial",
                "details": { "key": "intent", "source": "body" }
            }
        })
        .to_string();
        let query_refusal = json!({
            "error": {
                "status": 400, "name": "ValidationError",
                "message": "Invalid key populate",
                "details": { "key": "populate", "source": "query" }
            }
        })
        .to_string();

        assert_eq!(unknown_key(400, &enum_refusal), None);
        assert_eq!(unknown_key(400, &query_refusal), None);
        assert_eq!(unknown_key(403, &refusal(KEYWORDS)), None, "a permission failure");
        assert_eq!(unknown_key(500, "<html>gateway</html>"), None);
        assert_eq!(unknown_key(400, ""), None);
    }

    #[test]
    fn a_refused_field_leaves_the_body() {
        let mut payload = json!({ "data": { "title": "A post", "ogImage": 9 } });
        assert!(drop_field(&mut payload, "ogImage", &[]));
        assert_eq!(payload, json!({ "data": { "title": "A post" } }));
    }

    /// The stop condition. A key that is not in the body means the refusal is
    /// about something else, and re-sending the identical body would loop.
    #[test]
    fn a_field_that_is_not_there_is_not_a_fix() {
        let mut payload = json!({ "data": { "title": "A post" } });
        assert!(!drop_field(&mut payload, "ogImage", &[]));
        assert!(!drop_field(&mut json!({ "title": "no data key" }), "title", &[]));
    }

    /// The case the live CMS is actually in: no `keywordTargets`, and nothing
    /// written into `keywords` for it to merge with.
    #[test]
    fn the_target_terms_survive_into_an_empty_keywords_list() {
        let mut payload = json!({ "data": { "keywordTargets": [
            { "term": "ai watermarking", "priority": "primary" },
            { "term": " provenance ", "priority": "secondary" },
            { "term": "  ", "priority": "secondary" },
        ] } });

        assert!(drop_field(&mut payload, KEYWORD_TARGETS, &[]));
        assert_eq!(payload["data"]["keywords"], json!(["ai watermarking", "provenance"]));
        assert!(payload["data"].get(KEYWORD_TARGETS).is_none());
    }

    #[test]
    fn terms_already_in_the_flat_list_are_not_repeated() {
        let mut payload = json!({ "data": {
            "keywords": ["AI watermarking"],
            "keywordTargets": [
                { "term": "ai watermarking", "priority": "primary" },
                { "term": "provenance", "priority": "secondary" },
            ],
        } });

        assert!(drop_field(&mut payload, KEYWORD_TARGETS, &[]));
        assert_eq!(payload["data"]["keywords"], json!(["AI watermarking", "provenance"]));
    }

    /// A CMS old enough to refuse both gets neither back — putting `keywords`
    /// in after it was refused would fail on the next attempt, forever.
    #[test]
    fn the_terms_are_not_rescued_into_a_field_the_cms_also_refused() {
        let mut payload = json!({ "data": { "keywordTargets": [
            { "term": "ai watermarking", "priority": "primary" },
        ] } });

        assert!(drop_field(&mut payload, KEYWORD_TARGETS, &[KEYWORDS.to_string()]));
        assert!(payload["data"].get(KEYWORDS).is_none());
        assert_eq!(payload["data"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn the_warning_names_the_fields_and_where_they_went() {
        assert_eq!(warning(&[]), None);

        let one = warning(&[KEYWORD_TARGETS.to_string()]).unwrap();
        assert!(one.contains("no keywordTargets"), "{one}");
        assert!(one.contains("saved in keywords"), "{one}");
        assert!(one.contains("re-publish"), "{one}");

        let both = warning(&[KEYWORD_TARGETS.to_string(), KEYWORDS.to_string()]).unwrap();
        assert!(both.contains("no keywordTargets, keywords"), "{both}");
        assert!(!both.contains("saved in keywords"), "nothing was rescued: {both}");
    }
}
