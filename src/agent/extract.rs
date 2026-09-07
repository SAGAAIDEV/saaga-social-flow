use anyhow::Result;
use rig::client::CompletionClient;
use rig::extractor::ExtractorBuilder;
use rig::providers::openrouter::ProviderPreferences;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::notes::AUTO_PROVIDER;

use super::trace::LlmStep;

pub fn extract<T>(
    prompt_id: &str,
    step: &str,
    preamble: &super::prompt::Resolved,
    prompt: String,
    model: &str,
    provider: Option<&str>,
) -> Result<(T, LlmStep)>
where
    T: JsonSchema + for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
{
    let client = super::openrouter_client()?;
    let mut builder = ExtractorBuilder::<_, T>::new(client.completion_model(model))
        .preamble(&preamble.text)
        .retries(2);
    if let Some(name) = provider.filter(|p| !p.is_empty() && *p != AUTO_PROVIDER) {
        let prefs = ProviderPreferences::new()
            .order([name])
            .allow_fallbacks(true);
        builder = builder.additional_params(serde_json::json!({ "provider": prefs }));
    }
    let value = super::block_on(builder.build().extract(prompt.clone()))?
        .map_err(|err| anyhow::anyhow!("extractor {prompt_id}: {err}"))?;
    let recorded = LlmStep::new(prompt_id, step, model, provider, preamble, prompt, &value)?;
    Ok((value, recorded))
}
