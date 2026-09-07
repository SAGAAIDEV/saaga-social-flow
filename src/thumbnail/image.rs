//! Generating thumbnail candidates through OpenRouter's image models.
//!
//! Chat completions rather than the dedicated `/images` endpoint, because this
//! stage sends *pictures in as well as out*: the camera still and the style
//! references ride as image parts alongside the brief, which the simpler endpoint
//! has no room for.
//!
//! Not through rig, despite rig being in this crate. rig 0.41 ships image
//! generation for gemini, openai, xai and huggingface — but not for its OpenRouter
//! provider, so using it would mean one path for Gemini and a hand-rolled one for
//! Seedream. One client against OpenRouter covers every model with one key, and
//! makes the model list configuration rather than code.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Generation is slow — minutes, for a Pro-tier model under load.
const TIMEOUT_SECS: u64 = 300;

/// One image model this project can draw with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// OpenRouter id, e.g. "google/gemini-3.1-flash-image".
    pub id: String,
    /// What the pane calls it.
    pub label: String,
}

/// What came back: the bytes, and which model made them.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub model: String,
    pub jpeg: Vec<u8>,
}

/// The request body for one candidate.
///
/// Pure so the payload can be asserted without spending money — the shape is the
/// part most likely to be wrong, and the least pleasant to debug live.
#[cfg(test)]
pub fn request_body(
    model: &str,
    prompt: &str,
    still: &[u8],
    screen: Option<&[u8]>,
    references: &[Vec<u8>],
) -> Value {
    request_body_for(model, prompt, still, screen, references, super::format::Format::Horizontal)
}

pub fn request_body_for(
    model: &str, prompt: &str, still: &[u8], screen: Option<&[u8]>,
    references: &[Vec<u8>], format: super::format::Format,
) -> Value {
    // Before the prompt, because the prompt is written as though the pictures are
    // already in the room: "make him look excited" has no referent until whatever
    // "him" is has been introduced. It also says when a screen did *not* come —
    // a talking-head layout has none, and a prompt asking for one should be read
    // knowing it is absent rather than looked for among the references.
    let mut content = vec![json!({
        "type": "text",
        "text": manifest(screen.is_some(), references.len())
    })];
    content.push(json!({ "type": "text", "text": prompt }));
    // Each picture is then named where it arrives. Every image used to be a bare
    // part in one list, leaving the model to guess from position that the first
    // was a person to draw and the rest were only a look to borrow — and a style
    // reference read as subject matter is how a stranger's face ends up in the
    // thumbnail.
    content.push(json!({
        "type": "text",
        "text": "The camera photo of the presenter. Keep the likeness."
    }));
    content.push(image_part(still));
    if let Some(screen) = screen {
        content.push(json!({
            "type": "text",
            "text": "Their screen — what the video is about. Not to be copied literally."
        }));
        content.push(image_part(screen));
    }
    if !references.is_empty() {
        content.push(json!({
            "type": "text",
            "text": "Style references. Match the look, never the content."
        }));
        content.extend(references.iter().map(|bytes| image_part(bytes)));
    }
    let mut body = json!({
        "model": model,
        "modalities": ["image", "text"],
        "messages": [{ "role": "user", "content": content }],
    });
    // Supply geometry separately from the creative brief for image providers.
    body["image_config"] = json!({ "aspect_ratio": format.aspect_ratio() });
    body
}

/// One line naming what is attached, in the order it arrives.
fn manifest(has_screen: bool, references: usize) -> String {
    let mut items = vec!["a camera photo of the presenter".to_string()];
    if has_screen {
        items.push("a capture of their screen".to_string());
    }
    match references {
        0 => {}
        1 => items.push("1 style reference".to_string()),
        many => items.push(format!("{many} style references")),
    }
    let listed = match items.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    let absent = match has_screen {
        true => "",
        false => " There is no screen capture with this one.",
    };
    format!("Attached, in this order: {listed}.{absent}")
}

fn image_part(bytes: &[u8]) -> Value {
    json!({ "type": "image_url", "image_url": { "url": data_url(bytes) } })
}

fn data_url(bytes: &[u8]) -> String {
    format!("data:{};base64,{}", media_type(bytes), base64(bytes))
}

/// An image's media type, read from its magic bytes.
///
/// Declared rather than assumed, because a reference is not always a JPEG:
/// [`crate::thumbnail::references::prepare`] keeps the original file whenever
/// Core Image cannot shrink it or the shrink does not pay, so a dropped PNG
/// arrives here still a PNG. Calling every part `image/jpeg` was a claim the
/// provider is entitled to check and reject.
fn media_type(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        // An ISO base-media `ftyp` box: HEIC and friends, which Core Image reads
        // happily and which a phone screenshot on this machine may well be.
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => "image/heic",
        // Unrecognised, and jpeg is what the rest of this stage produces.
        _ => "image/jpeg",
    }
}

/// Base64, written out rather than pulled in — one small encoder against a new
/// dependency for the whole crate.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes a `data:` URL, or plain base64, into bytes.
pub fn decode_data_url(url: &str) -> Result<Vec<u8>> {
    let payload = match url.split_once(";base64,") {
        Some((_, payload)) => payload,
        None if url.starts_with("data:") => bail!("image data url is not base64"),
        None => url,
    };
    decode_base64(payload.trim())
}

fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let clean: Vec<u8> = text
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = value(*c).with_context(|| format!("bad base64 byte {c:?}"))?;
            n |= v << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    Ok(out)
}

/// Pulls every image out of a completion response.
///
/// Tolerant about shape: OpenRouter normalises across providers, and a model that
/// answers with a bare url string rather than an object should still work.
pub fn images_from(body: &Value) -> Vec<String> {
    let Some(message) = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
    else {
        return Vec::new();
    };
    message
        .get("images")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("image_url")
                        .and_then(|url| url.get("url"))
                        .or_else(|| item.get("url"))
                        .or(Some(item))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Generates one candidate. `images` is the still followed by the active references.
pub fn generate(
    api_key: &str,
    model: &str,
    prompt: &str,
    still: &[u8],
    screen: Option<&[u8]>,
    references: &[Vec<u8>],
    format: super::format::Format,
) -> Result<Candidate> {
    let response = ureq::post(ENDPOINT)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .set("HTTP-Referer", "https://github.com/saaga-martech/stream-recorder")
        .set("X-Title", "stream-recorder thumbnails")
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .send_json(request_body_for(model, prompt, still, screen, references, format));

    let body: Value = match response {
        Ok(response) => response.into_json().context("parsing the image response")?,
        Err(ureq::Error::Status(code, response)) => {
            let detail = response.into_string().unwrap_or_default();
            bail!("{model} returned {code}: {}", detail.trim());
        }
        Err(err) => return Err(err).with_context(|| format!("calling {model}")),
    };
    if let Some(error) = body.get("error") {
        bail!("{model}: {error}");
    }

    let urls = images_from(&body);
    let Some(first) = urls.first() else {
        // A text-only answer usually means a refusal, and the text says why.
        let said = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or("no image and no explanation");
        bail!("{model} returned no image: {}", said.trim());
    };
    Ok(Candidate {
        model: model.to_string(),
        jpeg: decode_data_url(first)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for sample in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foob"[..],
            &b"fooba"[..],
            &b"foobar"[..],
        ] {
            let encoded = base64(sample);
            assert_eq!(decode_base64(&encoded).unwrap(), sample, "{sample:?}");
        }
    }

    /// Pinned against the RFC 4648 vectors, since a hand-rolled encoder that is
    /// subtly wrong would produce images no model can read.
    #[test]
    fn base64_matches_the_published_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn binary_bytes_survive_the_round_trip() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode_base64(&base64(&bytes)).unwrap(), bytes);
    }

    const JPEG: [u8; 4] = [0xFF, 0xD8, 0xFF, 0xE0];
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn the_request_carries_the_brief_and_the_still() {
        let body = request_body("google/gemini-3.1-flash-image", "a brief", &JPEG, None, &[]);
        assert_eq!(body["model"], "google/gemini-3.1-flash-image");
        assert_eq!(body["modalities"][0], "image");
        assert_eq!(body["image_config"]["aspect_ratio"], "16:9");
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        // The manifest comes first, then the brief written against it.
        assert!(content[0]["text"].as_str().unwrap().starts_with("Attached"));
        assert_eq!(content[1]["text"], "a brief");
        // The still is introduced before it arrives, or it is just an image.
        assert!(content[2]["text"].as_str().unwrap().contains("presenter"));
        assert_eq!(content[3]["type"], "image_url");
        assert!(content[3]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        assert_eq!(content.len(), 4, "no reference preamble when there are none");
    }

    /// The prompt is written as though the pictures are already in the room, so
    /// it has to be read after being told which ones turned up.
    #[test]
    fn the_manifest_names_what_is_attached() {
        let both = manifest(true, 2);
        assert!(both.contains("a camera photo of the presenter"));
        assert!(both.contains("a capture of their screen"));
        assert!(both.contains("and 2 style references"));
        assert!(!both.contains("no screen capture"));

        assert!(manifest(true, 1).contains("1 style reference."));
    }

    /// A talking-head layout has no screen. Saying so stops a prompt that asks
    /// for one from being answered out of the style references instead.
    #[test]
    fn the_manifest_says_when_no_screen_came() {
        let alone = manifest(false, 0);
        assert_eq!(
            alone,
            "Attached, in this order: a camera photo of the presenter. \
             There is no screen capture with this one."
        );
    }

    /// The whole point of the split: the model is told which image is a person to
    /// draw and which are only a look to borrow. Unlabelled, a style reference
    /// reads as subject matter and someone else's face lands in the thumbnail.
    #[test]
    fn style_references_are_announced_as_style_and_not_as_subject() {
        let body = request_body("m", "a brief", &JPEG, None, &[PNG.to_vec(), JPEG.to_vec()]);
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        let note = content[4]["text"].as_str().unwrap();
        assert!(note.contains("Style references"));
        assert!(note.contains("never the content"));
        assert_eq!(content[5]["type"], "image_url");
        assert_eq!(content[6]["type"], "image_url");
        assert_eq!(content.len(), 7);
    }

    /// The screen answers a different question from either the presenter or the
    /// style refs — it is what the video is *about* — so it gets said out loud
    /// and sits between them.
    #[test]
    fn the_screen_grab_is_introduced_as_subject_matter() {
        let body = request_body("m", "a brief", &JPEG, Some(&PNG), &[JPEG.to_vec()]);
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        assert!(content[2]["text"].as_str().unwrap().contains("presenter"));
        assert_eq!(content[3]["type"], "image_url");
        let about = content[4]["text"].as_str().unwrap();
        assert!(about.contains("Their screen"));
        assert!(about.contains("Not to be copied literally"));
        assert!(content[5]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        // Then the style references, still last and still labelled as style.
        assert!(content[6]["text"].as_str().unwrap().contains("Style references"));
        assert_eq!(content.len(), 8);
    }

    /// A talking-head layout has no screen, and the request must not grow a part
    /// promising one.
    #[test]
    fn no_screen_leaves_the_payload_as_it_was() {
        let body = request_body("m", "a brief", &JPEG, None, &[]);
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        assert_eq!(content.len(), 4);
        assert!(!body.to_string().contains("Their screen"));
        // And it says so rather than leaving the absence to be inferred.
        assert!(body.to_string().contains("no screen capture"));
    }

    /// `prepare` stores the original when a shrink does not pay, so a dropped PNG
    /// stays a PNG — and every part used to be declared jpeg regardless.
    #[test]
    fn each_image_declares_the_type_its_bytes_actually_are() {
        let body = request_body("m", "a brief", &JPEG, None, &[PNG.to_vec()]);
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        assert!(content[3]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        assert!(content[5]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn the_common_image_formats_are_recognised() {
        assert_eq!(media_type(&JPEG), "image/jpeg");
        assert_eq!(media_type(&PNG), "image/png");
        assert_eq!(media_type(b"GIF89a...."), "image/gif");
        assert_eq!(media_type(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(media_type(b"\0\0\0\x18ftypheic"), "image/heic");
        // Unknown falls back to what the rest of this stage produces.
        assert_eq!(media_type(b"nonsense"), "image/jpeg");
    }

    #[test]
    fn images_are_read_from_the_normalised_shape() {
        let body = json!({
            "choices": [{ "message": {
                "images": [{ "image_url": { "url": "data:image/png;base64,Zm9v" } }]
            }}]
        });
        assert_eq!(images_from(&body), vec!["data:image/png;base64,Zm9v"]);
        assert_eq!(decode_data_url(&images_from(&body)[0]).unwrap(), b"foo");
    }

    /// Providers differ; a bare url string should not lose the image.
    #[test]
    fn a_bare_url_string_is_also_accepted() {
        let body = json!({ "choices": [{ "message": { "images": ["Zm9v"] } }] });
        assert_eq!(images_from(&body), vec!["Zm9v"]);
        assert_eq!(decode_data_url("Zm9v").unwrap(), b"foo");
    }

    #[test]
    fn a_text_only_answer_yields_no_images() {
        let body = json!({ "choices": [{ "message": { "content": "I cannot draw that." } }] });
        assert!(images_from(&body).is_empty());
        assert!(images_from(&json!({})).is_empty());
    }

    #[test]
    fn a_non_base64_data_url_is_refused_rather_than_mangled() {
        assert!(decode_data_url("data:image/svg+xml,<svg/>").is_err());
        assert!(decode_data_url("data:image/png;base64,!!!!").is_err());
    }
}
