//! "Test" beside each group of credentials: does this key actually work.
//!
//! A key can be present, well-formed, and still wrong — revoked, pasted with a
//! newline in it, or scoped to the wrong workspace. Every one of those failures
//! otherwise surfaces halfway through a publish, on a recording someone has
//! already made. So each check makes the cheapest real authenticated call the
//! service offers and reports what came back.
//!
//! Checks go through the same clients the app publishes with — `BufferClient`,
//! `Strapi` — rather than hand-rolled requests. A test that authenticates
//! differently from the real call is a test that can pass while publishing
//! fails.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use super::Group;

/// The outcome of one group's test, as the pane draws it.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub group: &'static str,
    pub ok: bool,
    /// One line, shown next to the button. On failure this is the service's own
    /// message where there is one — "insufficient permissions" points somewhere,
    /// "request failed" does not.
    pub message: String,
}

impl Outcome {
    fn ok(group: Group, message: impl Into<String>) -> Self {
        Self { group: group.slug(), ok: true, message: message.into() }
    }

    fn bad(group: Group, message: impl Into<String>) -> Self {
        Self { group: group.slug(), ok: false, message: message.into() }
    }

    /// The script that drops this verdict next to its section.
    ///
    /// Built here rather than at the call site so it can be tested: the pane
    /// runs it through `WebPane::eval`, which has no way to report a syntax
    /// error — a malformed script simply does nothing, and the button would
    /// look like it never finished.
    pub fn script(&self) -> String {
        let payload = serde_json::to_string(self).unwrap_or_else(|_| "null".into());
        format!(
            "(function(o){{if(!o)return;\
             var el=document.getElementById('result-'+o.group);if(!el)return;\
             el.className='result '+(o.ok?'ok':'bad');\
             el.textContent=(o.ok?'\u{2713} ':'\u{2717} ')+o.message;}})({payload})"
        )
    }
}

/// Run the check for one group. Blocking, and called on a worker thread.
pub fn run(group: Group) -> Outcome {
    let result = match group {
        Group::Llm => openrouter(),
        Group::Social => buffer(),
        Group::Blog => strapi(),
        Group::Video => youtube(),
        Group::Transcript => assemblyai(),
    };
    match result {
        Ok(message) => Outcome::ok(group, message),
        // The whole chain: `anyhow`'s context is where "http 401" gets the
        // "checking the OpenRouter key" that makes it readable.
        Err(err) => Outcome::bad(group, format!("{err:#}")),
    }
}

fn required(key: &str) -> Result<String> {
    let value = std::env::var(key).unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        bail!("{key} is not set");
    }
    // A key pasted out of a terminal or a chat message can arrive wrapped in
    // whitespace that survives into the header and fails as a bare 401.
    Ok(value.to_string())
}

/// Balance and rate-limit for the key. The one OpenRouter endpoint that costs
/// nothing and still proves the key is live.
fn openrouter() -> Result<String> {
    let key = required("OPENROUTER_API_KEY")?;
    let response = ureq::get("https://openrouter.ai/api/v1/key")
        .timeout(Duration::from_secs(20))
        .set("Authorization", &format!("Bearer {key}"))
        .call();
    let body: serde_json::Value = match response {
        Ok(response) => response.into_json().context("parsing the OpenRouter reply")?,
        Err(ureq::Error::Status(401, _)) => bail!("the key was rejected (401) — check it was copied whole"),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            bail!("OpenRouter http {code}: {}", text.trim());
        }
        Err(err) => return Err(err).context("reaching OpenRouter"),
    };
    let data = body.get("data").unwrap_or(&body);
    let label = data.get("label").and_then(|v| v.as_str()).unwrap_or("key");
    // `limit` is null on an unlimited key, which is not a failure to report.
    let remaining = data
        .get("limit_remaining")
        .and_then(serde_json::Value::as_f64)
        .map(|left| format!(", ${left:.2} remaining"))
        .unwrap_or_default();
    Ok(format!("{label} accepted{remaining}"))
}

/// Buffer's own client, then the channel list — which is what the schedule
/// stage reads, so an empty list here is the same empty list that would
/// otherwise surface as "no channel connected" at send time.
fn buffer() -> Result<String> {
    required("BUFFER_API_KEY")?;
    let client = crate::schedule::buffer::BufferClient::from_env()?;
    let channels = client.channels().context("listing Buffer channels")?;
    if channels.is_empty() {
        bail!("the key works, but this workspace has no connected channels");
    }
    let named: Vec<&str> = channels.iter().map(|c| c.service.as_str()).take(6).collect();
    Ok(format!("{} channels: {}", channels.len(), named.join(", ")))
}

/// A token can read the CMS and still be unable to write to it. `list_authors`
/// is the same lookup the Blog pane populates its byline dropdown from, so this
/// answers the question that pane will ask next.
fn strapi() -> Result<String> {
    required("STRAPI_API_URL")?;
    required("STRAPI_API_TOKEN")?;
    let client = crate::blog::strapi::Strapi::from_env()?;
    let authors = client.list_authors().context("listing Strapi authors")?;
    Ok(format!("connected, {} authors visible", authors.len()))
}

/// YouTube is the one credential a request cannot verify on its own: the client
/// id and secret only mean anything inside an OAuth exchange that opens a
/// browser and asks a human to sign in. So this checks the shape and says where
/// the real test is, rather than reporting a pass it did not earn.
fn youtube() -> Result<String> {
    let id = required("YOUTUBE_CLIENT_ID")?;
    required("YOUTUBE_CLIENT_SECRET")?;
    if !id.ends_with(".apps.googleusercontent.com") {
        bail!(
            "that does not look like a Google OAuth client id — it should end in \
             .apps.googleusercontent.com"
        );
    }
    match crate::publish::connected_channel() {
        Some(channel) => Ok(format!("signed in as {channel}")),
        None => Ok("client looks right — press Connect on the YouTube tab to sign in".into()),
    }
}

fn assemblyai() -> Result<String> {
    let key = required("ASSEMBLYAI_API_KEY")?;
    let response = ureq::get("https://api.assemblyai.com/v2/transcript?limit=1")
        .timeout(Duration::from_secs(20))
        .set("Authorization", &key)
        .call();
    match response {
        Ok(_) => Ok("key accepted".into()),
        Err(ureq::Error::Status(401, _)) => bail!("the key was rejected (401)"),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            bail!("AssemblyAI http {code}: {}", text.trim());
        }
        Err(err) => Err(err).context("reaching AssemblyAI"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every check begins by refusing to make a network call it cannot
    /// authenticate — an unset key should read as "not set", not as a timeout.
    #[test]
    fn an_unset_key_is_named_rather_than_dialled() {
        let err = required("SETTINGS_CHECK_DEFINITELY_UNSET").unwrap_err();
        assert!(err.to_string().contains("is not set"), "{err}");
    }

    /// The pane matches outcomes to sections by slug, so the two have to agree.
    #[test]
    fn an_outcome_carries_the_slug_the_pane_keys_on() {
        let outcome = Outcome::bad(Group::Llm, "nope");
        assert_eq!(outcome.group, Group::Llm.slug());
        assert!(!outcome.ok);
    }

    /// `eval` cannot report a syntax error, so the shape of the script is
    /// checked here instead: balanced delimiters, and the element id it targets.
    #[test]
    fn the_result_script_is_balanced_and_targets_its_section() {
        let script = Outcome::ok(Group::Blog, "connected").script();
        assert!(script.contains("result-'+o.group"), "{script}");
        assert!(script.contains(r#""group":"blog""#), "{script}");
        assert_eq!(
            script.matches('{').count(),
            script.matches('}').count(),
            "unbalanced braces: {script}"
        );
        assert_eq!(
            script.matches('(').count(),
            script.matches(')').count(),
            "unbalanced parens: {script}"
        );
    }

    /// A service's own error text is interpolated into a script. Anything in it
    /// that could end the literal early has to survive as data instead —
    /// `serde_json` is what does that, and this checks it is actually in the
    /// path rather than the message being pasted in raw.
    ///
    /// The invariant is a round trip: the payload the script carries has to
    /// parse back to exactly the message that went in.
    #[test]
    fn a_quote_in_an_error_message_cannot_break_out_of_the_script() {
        let hostile = "bad key: '\"; alert(1); //";
        let script = Outcome::bad(Group::Llm, hostile).script();

        // From the `})(` that closes the function and opens its call — not the
        // last `(` in the string, which is inside the payload here.
        let start = script.rfind("})(").expect("the call site") + 3;
        let json = &script[start..script.len() - 1];
        let parsed: serde_json::Value =
            serde_json::from_str(json).expect("the payload is valid JSON");
        assert_eq!(parsed["message"], hostile, "the message did not survive intact");
        assert_eq!(parsed["ok"], false);

        // And the raw text never appears unescaped, which is what would let it
        // close the literal and run.
        assert!(!script.contains(r#"'"; alert"#), "unescaped payload: {script}");
    }
}
