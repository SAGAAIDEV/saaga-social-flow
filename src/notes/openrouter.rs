//! OpenRouter model/provider catalog for the notes picker.

use anyhow::{Context, Result};

const MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const PROVIDERS_URL: &str = "https://openrouter.ai/api/v1/providers";
pub const AUTO_PROVIDER: &str = "Auto";

const FALLBACK: &[(&str, &str)] = &[
    ("google/gemini-2.5-flash", "Gemini 2.5 Flash"),
    ("google/gemini-2.5-pro", "Gemini 2.5 Pro"),
    ("anthropic/claude-sonnet-4", "Claude Sonnet 4"),
    ("openai/gpt-4.1", "GPT-4.1"),
    ("openai/gpt-4.1-mini", "GPT-4.1 Mini"),
    ("x-ai/grok-4", "Grok 4"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    pub label: String,
}

pub fn fallback_catalog() -> Vec<ModelChoice> {
    FALLBACK
        .iter()
        .map(|(id, label)| ModelChoice {
            id: (*id).into(),
            label: (*label).into(),
        })
        .collect()
}

pub fn default_model() -> String {
    FALLBACK[0].0.to_string()
}

#[cfg(test)]
pub fn index_in(list: &[ModelChoice], id: &str) -> usize {
    list.iter().position(|m| m.id == id).unwrap_or(0)
}

#[cfg(test)]
pub fn id_in(list: &[ModelChoice], idx: usize) -> String {
    list.get(idx)
        .map(|m| m.id.clone())
        .unwrap_or_else(default_model)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelMenuRow {
    Header(String),
    Model { id: String, label: String },
}

pub fn category_of(id: &str) -> String {
    match id.split('/').next().unwrap_or("other") {
        "google" => "Google".into(),
        "anthropic" => "Anthropic".into(),
        "openai" => "OpenAI".into(),
        "x-ai" => "xAI".into(),
        "meta-llama" => "Meta".into(),
        "mistralai" => "Mistral".into(),
        "deepseek" => "DeepSeek".into(),
        "qwen" => "Qwen".into(),
        "cohere" => "Cohere".into(),
        "amazon" => "Amazon".into(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                None => "Other".into(),
            }
        }
    }
}

fn short_label(model: &ModelChoice) -> String {
    model
        .label
        .split_once(": ")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| model.label.clone())
}

pub fn model_menu(list: &[ModelChoice]) -> Vec<ModelMenuRow> {
    let mut groups: std::collections::BTreeMap<String, Vec<&ModelChoice>> =
        std::collections::BTreeMap::new();
    for model in list {
        groups.entry(category_of(&model.id)).or_default().push(model);
    }
    let mut out = Vec::new();
    for (cat, mut models) in groups {
        models.sort_by(|a, b| short_label(a).cmp(&short_label(b)));
        out.push(ModelMenuRow::Header(cat));
        for model in models {
            out.push(ModelMenuRow::Model {
                id: model.id.clone(),
                label: short_label(model),
            });
        }
    }
    out
}

pub fn menu_index_of(menu: &[ModelMenuRow], id: &str) -> usize {
    menu.iter()
        .position(|row| matches!(row, ModelMenuRow::Model { id: mid, .. } if mid == id))
        .unwrap_or_else(|| menu.iter().position(|row| matches!(row, ModelMenuRow::Model { .. })).unwrap_or(0))
}

pub fn menu_id_at(menu: &[ModelMenuRow], idx: usize) -> Option<String> {
    match menu.get(idx) {
        Some(ModelMenuRow::Model { id, .. }) => Some(id.clone()),
        _ => None,
    }
}

pub fn ensure_model(list: &mut Vec<ModelChoice>, id: &str) {
    if id.is_empty() || list.iter().any(|m| m.id == id) {
        return;
    }
    list.insert(
        0,
        ModelChoice {
            label: id.to_string(),
            id: id.to_string(),
        },
    );
}

pub fn load_catalog() -> Vec<ModelChoice> {
    load_catalog_for(None)
}

pub fn load_catalog_for(provider: Option<&str>) -> Vec<ModelChoice> {
    match fetch_catalog(provider) {
        Ok(list) if !list.is_empty() => {
            eprintln!(
                "stream-recorder: loaded {} OpenRouter models{}",
                list.len(),
                provider
                    .filter(|p| *p != AUTO_PROVIDER)
                    .map(|p| format!(" for {p}"))
                    .unwrap_or_default()
            );
            list
        }
        Ok(_) => {
            eprintln!("stream-recorder: OpenRouter model list was empty; using built-in list");
            filter_fallback(provider)
        }
        Err(err) => {
            eprintln!(
                "stream-recorder: OpenRouter model list unavailable ({err:#}); using built-in list"
            );
            filter_fallback(provider)
        }
    }
}

/// The built-in list, used when OpenRouter cannot be reached.
///
/// Deliberately *not* filtered by provider. It used to compare the provider name
/// against [`category_of`], which mixes two different taxonomies: `category_of`
/// reads the model's vendor out of its id prefix ("Google", "Anthropic"), while
/// a provider is who *serves* the model ("Google AI Studio", "Together",
/// "Fireworks" — 105 of them, and most serve other vendors' models). "Google AI
/// Studio" never equals "Google", so the filter returned nothing and the caller
/// fell back to the unfiltered list anyway — by the longer route.
///
/// Offline there is no way to know which provider serves what, so the honest
/// answer is the whole list.
fn filter_fallback(provider: Option<&str>) -> Vec<ModelChoice> {
    if let Some(name) = provider.filter(|p| *p != AUTO_PROVIDER) {
        eprintln!(
            "stream-recorder: cannot filter the built-in model list by {name} — \
             showing all of it"
        );
    }
    fallback_catalog()
}

fn fetch_catalog(provider: Option<&str>) -> Result<Vec<ModelChoice>> {
    let mut req = ureq::get(MODELS_URL)
        .query("output_modalities", "text")
        .query("sort", "most-popular")
        .query("limit", "80")
        .timeout(std::time::Duration::from_secs(15));
    if let Some(name) = provider.filter(|p| *p != AUTO_PROVIDER) {
        req = req.query("providers", name);
    }
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.trim().is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
    }
    let body: serde_json::Value = req
        .call()
        .context("listing OpenRouter models")?
        .into_json()
        .context("parsing OpenRouter models")?;
    parse_catalog(&body)
}

pub(crate) fn parse_catalog(body: &serde_json::Value) -> Result<Vec<ModelChoice>> {
    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .context("OpenRouter models missing data")?;
    let mut out = Vec::new();
    for item in data {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if id.contains("embed") || id.contains("rerank") {
            continue;
        }
        if let Some(outs) = item.pointer("/architecture/output_modalities") {
            if let Some(arr) = outs.as_array() {
                if !arr.iter().any(|m| m.as_str() == Some("text")) {
                    continue;
                }
            }
        }
        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(id);
        out.push(ModelChoice {
            id: id.to_string(),
            label: name.to_string(),
        });
    }
    Ok(out)
}

pub fn load_providers() -> Vec<String> {
    match fetch_provider_names() {
        Ok(mut names) if !names.is_empty() => {
            names.sort();
            names.dedup();
            let mut out = vec![AUTO_PROVIDER.to_string()];
            out.extend(names);
            eprintln!("stream-recorder: loaded {} OpenRouter providers", out.len() - 1);
            out
        }
        Ok(_) | Err(_) => {
            let mut out = vec![AUTO_PROVIDER.to_string()];
            out.extend(
                fallback_catalog()
                    .iter()
                    .map(|m| category_of(&m.id))
                    .collect::<std::collections::BTreeSet<_>>(),
            );
            out
        }
    }
}

fn fetch_provider_names() -> Result<Vec<String>> {
    let mut req = ureq::get(PROVIDERS_URL).timeout(std::time::Duration::from_secs(15));
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.trim().is_empty() {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
    }
    let body: serde_json::Value = req
        .call()
        .context("listing OpenRouter providers")?
        .into_json()
        .context("parsing OpenRouter providers")?;
    Ok(parse_provider_list(&body))
}

pub(crate) fn parse_provider_list(body: &serde_json::Value) -> Vec<String> {
    body.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("name").and_then(|n| n.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn provider_index(list: &[String], saved: Option<&str>) -> usize {
    saved
        .and_then(|name| list.iter().position(|p| p == name))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_menu_groups_and_skips_headers() {
        let list = vec![
            ModelChoice {
                id: "google/gemini-2.5-flash".into(),
                label: "Google: Gemini 2.5 Flash".into(),
            },
            ModelChoice {
                id: "anthropic/claude-sonnet-4".into(),
                label: "Anthropic: Claude Sonnet 4".into(),
            },
        ];
        let menu = model_menu(&list);
        assert!(matches!(&menu[0], ModelMenuRow::Header(h) if h == "Anthropic"));
        assert!(matches!(&menu[1], ModelMenuRow::Model { label, .. } if label == "Claude Sonnet 4"));
        assert!(matches!(&menu[2], ModelMenuRow::Header(h) if h == "Google"));
        assert_eq!(menu_id_at(&menu, 0), None);
        assert_eq!(
            menu_id_at(&menu, 1).as_deref(),
            Some("anthropic/claude-sonnet-4")
        );
        assert_eq!(menu_index_of(&menu, "google/gemini-2.5-flash"), 3);
    }

    #[test]
    fn parse_catalog_keeps_text_models() {
        let body = serde_json::json!({
            "data": [
                {"id":"openai/gpt-4","name":"GPT-4","architecture":{"output_modalities":["text"]}},
                {"id":"acme/embed","name":"Embed","architecture":{"output_modalities":["embeddings"]}},
                {"id":"acme/rerank-v1","name":"Rerank"}
            ]
        });
        let list = parse_catalog(&body).expect("parses");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "openai/gpt-4");
        assert_eq!(list[0].label, "GPT-4");
    }

    #[test]
    fn parse_provider_list_reads_names() {
        let body = serde_json::json!({
            "data": [
                {"name":"Google","slug":"google"},
                {"name":"Anthropic","slug":"anthropic"}
            ]
        });
        let names = parse_provider_list(&body);
        assert!(names.contains(&"Google".to_string()));
        assert!(names.contains(&"Anthropic".to_string()));
    }

    #[test]
    fn an_unknown_model_id_falls_back_to_the_first() {
        let list = fallback_catalog();
        assert_eq!(index_in(&list, "nope"), 0);
        assert_eq!(id_in(&list, 99), default_model());
    }
}
