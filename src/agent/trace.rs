use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STEP_JSON: &str = "llm.json";
pub const LEDGER: &str = "llm.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmStep {
    pub id: String,
    pub prompt_id: String,
    pub step: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub preamble: String,
    /// Which preamble version produced this — `Some(0)` builtin, `Some(n)` a
    /// recorded overlay, `None` an overlay edited outside the version ledger.
    /// Without it a reflection loop cannot tell whether its own last rewrite helped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<u32>,
    /// Content hash of `preamble`. Always present, so attribution survives even
    /// when the version label is unknown.
    #[serde(default)]
    pub prompt_hash: String,
    pub prompt: String,
    pub output: Value,
}

impl LlmStep {
    pub fn new(
        prompt_id: impl Into<String>,
        step: impl Into<String>,
        model: impl Into<String>,
        provider: Option<&str>,
        preamble: &super::prompt::Resolved,
        prompt: impl Into<String>,
        output: impl Serialize,
    ) -> Result<Self> {
        let prompt_id = prompt_id.into();
        let step = step.into();
        Ok(Self {
            id: step_id(&prompt_id),
            prompt_id,
            step,
            model: model.into(),
            provider: provider
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            preamble: preamble.text.clone(),
            prompt_version: preamble.version,
            prompt_hash: preamble.hash.clone(),
            prompt: prompt.into(),
            output: serde_json::to_value(output).context("serializing llm output")?,
        })
    }
}

fn step_id(prompt_id: &str) -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{prompt_id}-{ms}")
}

pub fn write_step(beside: &Path, root: &Path, step: &LlmStep) -> Result<()> {
    std::fs::create_dir_all(beside)
        .with_context(|| format!("creating {}", beside.display()))?;
    let path = beside.join(STEP_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(step).context("serializing llm step")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))?;

    let line = serde_json::to_string(step).context("serializing llm ledger line")? + "\n";
    let ledger = root.join(LEDGER);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger)
        .with_context(|| format!("opening {}", ledger.display()))?;
    use std::io::Write;
    file.write_all(line.as_bytes())
        .with_context(|| format!("appending {}", ledger.display()))?;

    tracing::info!(
        id = %step.id,
        prompt_id = %step.prompt_id,
        step = %step.step,
        model = %step.model,
        provider = step.provider.as_deref().unwrap_or("Auto"),
        prompt = %step.prompt,
        output = %step.output,
        "llm step"
    );
    eprintln!(
        "stream-recorder: logged {} → {} and {}",
        step.prompt_id,
        path.display(),
        ledger.display()
    );
    Ok(())
}

/// Reads back every logged step.
///
/// The read half of a pair whose write half is live: [`log`] appends on every
/// extractor run. Kept rather than deleted because the reflect stage
/// (`reflect.prompts`) is built on reading past prompts and their outputs, and
/// because a ledger nothing can read is not a ledger. Covered by its own test.
#[allow(dead_code)]
pub fn load_ledger(root: &Path) -> Result<Vec<LlmStep>> {
    let path = root.join(LEDGER);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .with_context(|| format!("parsing {} line {}", path.display(), i + 1))?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::prompt::{hash_of, Resolved};

    #[test]
    fn write_step_keeps_prompt_and_output() {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-llm-{}-{}",
            std::process::id(),
            "write"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let step = LlmStep::new(
            "titles.chapter_cards",
            "titles",
            "google/gemini-2.5-flash",
            Some("Google"),
            &Resolved {
                text: "preamble".into(),
                version: Some(0),
                hash: hash_of("preamble"),
            },
            "Video: Demo",
            serde_json::json!({"chapters":[{"n":1,"title":"Hook"}]}),
        )
        .unwrap();
        write_step(&dir, &dir, &step).unwrap();
        let back: LlmStep =
            serde_json::from_str(&std::fs::read_to_string(dir.join(STEP_JSON)).unwrap()).unwrap();
        assert_eq!(back.prompt_id, "titles.chapter_cards");
        assert_eq!(back.prompt, "Video: Demo");
        assert_eq!(back.output["chapters"][0]["title"], "Hook");
        let ledger = load_ledger(&dir).unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].step, "titles");
        // The version travels with the step, so a later reflection can attribute
        // this output to the exact preamble that produced it.
        assert_eq!(ledger[0].prompt_version, Some(0));
        assert_eq!(ledger[0].prompt_hash, hash_of("preamble"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
