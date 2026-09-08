//! The blurb's shape on the wire, and on its way back.
//!
//! Split from the job in [`super`] so it can be asserted without spending
//! money: the request shape is the part most likely to be wrong and the least
//! pleasant to debug live, and the salvage in [`parse`] exists precisely
//! because models do not always answer the way they were asked to.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Per figure, not per pass — each is its own request.
const TIMEOUT_SECS: u64 = 120;

/// One figure in, one blurb out: build, send, parse, clip, and refuse an empty
/// caption.
///
/// The whole round trip lives behind one call so [`super`] holds only the job —
/// which figures are left, what context each one gets, and what to do when one
/// fails — and never the shape of a request.
pub(super) fn ask(
    model: &str,
    provider: Option<&str>,
    preamble: &str,
    context: &str,
    jpeg: &[u8],
) -> Result<Blurb> {
    let body = request_body(model, provider, preamble, context, jpeg);
    let blurb = parse(&post(&body)?)?.clipped();
    if !blurb.is_usable() {
        bail!("the model returned an empty caption");
    }
    Ok(blurb)
}

/// What comes back.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Blurb {
    pub caption: String,
    #[serde(default)]
    pub alt: String,
}

/// Strapi's own ceiling on a caption field, and a sensible one for alt text.
const CAPTION_MAX: usize = 240;
const ALT_MAX: usize = 160;

impl Blurb {
    /// Trims what the model overshot on.
    ///
    /// Length rules stay prose in the prompt and are enforced here, the same
    /// division [`crate::blog::generate`] documents: models treat a schema's
    /// length bound as advice, and a hard bounce loses the whole blurb over two
    /// words.
    fn clipped(self) -> Blurb {
        Blurb {
            caption: clip(&self.caption, CAPTION_MAX),
            alt: clip(&self.alt, ALT_MAX),
        }
    }

    fn is_usable(&self) -> bool {
        !self.caption.trim().is_empty()
    }
}

/// Cuts at a word boundary, so a clipped caption ends in a word rather than
/// mid-syllable.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max).collect();
    match truncated.rsplit_once(' ') {
        Some((head, _)) if head.chars().count() > max / 2 => format!("{}…", head.trim_end()),
        _ => format!("{}…", truncated.trim_end()),
    }
}

/// The request, built without touching the network so the shape can be asserted.
pub fn request_body(
    model: &str,
    provider: Option<&str>,
    preamble: &str,
    context: &str,
    jpeg: &[u8],
) -> Value {
    // The picture before the words about it, for the same reason
    // `thumbnail::image` orders its parts that way: the text refers to "this
    // figure", which has no referent until the figure is in the room.
    let content = vec![
        json!({ "type": "text", "text": "The figure:" }),
        json!({
            "type": "image_url",
            "image_url": { "url": format!(
                "data:{};base64,{}",
                mime_of(jpeg),
                crate::thumbnail::image::base64(jpeg)
            ) }
        }),
        json!({ "type": "text", "text": context }),
    ];
    let mut body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": preamble },
            { "role": "user", "content": content },
        ],
        // A named schema rather than "give me JSON": the two fields are the
        // entire output, and a model that wraps them in prose costs the blurb.
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "figure_blurb",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "caption": { "type": "string" },
                        "alt": { "type": "string" },
                    },
                    "required": ["caption", "alt"],
                    "additionalProperties": false,
                },
            },
        },
    });
    if let Some(name) = provider.filter(|p| !p.is_empty() && *p != crate::notes::AUTO_PROVIDER) {
        body["provider"] = json!({ "order": [name], "allow_fallbacks": true });
    }
    body
}

fn post(body: &Value) -> Result<Value> {
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .context("OPENROUTER_API_KEY unset")?;
    let response = ureq::post(ENDPOINT)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .send_json(body.clone());
    match response {
        Ok(response) => response.into_json().context("parsing the blurb response"),
        // The body is where OpenRouter says *why*, and it is the difference
        // between "no credit" and "that model cannot see pictures".
        Err(ureq::Error::Status(code, response)) => {
            let detail = response
                .into_string()
                .unwrap_or_else(|_| "no body".to_string());
            bail!("openrouter answered {code}: {}", detail.trim())
        }
        Err(err) => Err(err).context("calling openrouter"),
    }
}

/// Pulls the blurb out of a chat completion.
pub fn parse(response: &Value) -> Result<Blurb> {
    let content = response
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .context("the response carried no message content")?;
    // Fenced JSON, despite the schema: a model that ignores `response_format`
    // still usually answers with the right object inside a code fence, and
    // salvaging it is cheaper than a retry.
    let text = content.trim();
    let json = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|rest| rest.rsplit_once("```"))
        .map(|(body, _)| body.trim())
        .unwrap_or(text);
    serde_json::from_str(json).with_context(|| format!("the blurb was not JSON: {json}"))
}

/// The picture's type, read off its first bytes: figures are WebP now and were
/// JPEG before, and a project can hold both. Anything unrecognised is sent as
/// JPEG, which is what every figure was until the format changed.
fn mime_of(bytes: &[u8]) -> &'static str {
    match bytes {
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WebP figure has to be declared as one: a data URL that calls it a JPEG
    /// is refused by some providers and silently misread by others.
    #[test]
    fn the_data_url_names_the_format_the_bytes_are_in() {
        let webp = b"RIFF\x10\x00\x00\x00WEBPVP8L";
        assert_eq!(mime_of(webp), "image/webp");
        assert_eq!(mime_of(b"\xff\xd8jpeg"), "image/jpeg");
        assert_eq!(mime_of(b"\x89PNG\r\n"), "image/png");
        let body = request_body("m", None, "p", "c", webp);
        assert!(body["messages"][1]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/webp;base64,"));
    }

    #[test]
    fn the_picture_arrives_before_the_words_about_it() {
        let body = request_body("m", None, "preamble", "context", b"\xff\xd8jpeg");
        let content = &body["messages"][1]["content"];
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[2]["text"], "context");
        assert!(content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        assert_eq!(body["messages"][0]["content"], "preamble");
    }
    /// The schema is what keeps the two fields from arriving wrapped in prose.
    #[test]
    fn the_request_pins_the_output_shape() {
        let body = request_body("m", None, "p", "c", b"x");
        assert_eq!(body["response_format"]["type"], "json_schema");
        let schema = &body["response_format"]["json_schema"]["schema"];
        assert_eq!(schema["required"][0], "caption");
        assert_eq!(schema["required"][1], "alt");
        assert_eq!(schema["additionalProperties"], false);
    }
    /// "Auto" is a menu label, not a provider. Sending it as one routes every
    /// blurb to a provider that does not exist.
    #[test]
    fn the_auto_provider_is_not_sent_as_a_route() {
        let auto = request_body("m", Some(crate::notes::AUTO_PROVIDER), "p", "c", b"x");
        assert!(auto.get("provider").is_none());
        assert!(request_body("m", Some(""), "p", "c", b"x")
            .get("provider")
            .is_none());
        let pinned = request_body("m", Some("google-vertex"), "p", "c", b"x");
        assert_eq!(pinned["provider"]["order"][0], "google-vertex");
    }
    #[test]
    fn a_plain_json_answer_parses() {
        let response = json!({
            "choices": [{ "message": { "content":
                "{\"caption\":\"The retry storm.\",\"alt\":\"A log of 429s\"}" } }]
        });
        let blurb = parse(&response).unwrap();
        assert_eq!(blurb.caption, "The retry storm.");
        assert_eq!(blurb.alt, "A log of 429s");
    }
    /// Models fence JSON even when told not to, and a fenced answer is a
    /// complete blurb behind three backticks.
    #[test]
    fn a_fenced_answer_parses_too() {
        for content in [
            "```json\n{\"caption\":\"Fenced.\",\"alt\":\"a\"}\n```",
            "```\n{\"caption\":\"Fenced.\",\"alt\":\"a\"}\n```",
        ] {
            let response = json!({ "choices": [{ "message": { "content": content } }] });
            assert_eq!(parse(&response).unwrap().caption, "Fenced.", "{content}");
        }
    }
    #[test]
    fn a_response_with_no_content_is_an_error() {
        assert!(parse(&json!({ "choices": [] })).is_err());
        assert!(parse(&json!({ "error": { "message": "no credit" } })).is_err());
    }
    /// An empty caption would publish as a figure with nothing under it, and the
    /// ledger row would stop it ever being rewritten.
    #[test]
    fn an_empty_caption_is_not_usable() {
        assert!(!Blurb {
            caption: "  ".into(),
            alt: "alt".into()
        }
        .is_usable());
        assert!(Blurb {
            caption: "Something.".into(),
            alt: String::new()
        }
        .is_usable());
    }
    #[test]
    fn an_overlong_caption_is_clipped_at_a_word() {
        let long = "word ".repeat(80);
        let blurb = Blurb {
            caption: long,
            alt: "a".repeat(300),
        }
        .clipped();
        assert!(
            blurb.caption.chars().count() <= CAPTION_MAX + 1,
            "{}",
            blurb.caption
        );
        assert!(blurb.caption.ends_with('…'));
        assert!(
            !blurb.caption.contains("wor…"),
            "cut mid-word: {}",
            blurb.caption
        );
        assert!(blurb.alt.chars().count() <= ALT_MAX + 1);
    }
    #[test]
    fn a_short_blurb_is_left_alone() {
        let blurb = Blurb {
            caption: "  Short. ".into(),
            alt: " alt ".into(),
        }
        .clipped();
        assert_eq!(blurb.caption, "Short.");
        assert_eq!(blurb.alt, "alt");
    }
}
