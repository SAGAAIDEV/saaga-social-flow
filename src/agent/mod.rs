//! Rig OpenRouter client, extractors, and LLM trace files.

pub mod extract;
pub mod notes;
pub mod prompt;
pub mod reflect;
pub mod titles;
pub mod trace;

use anyhow::{Context, Result};
use rig::providers::openrouter;

pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rig=trace"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

pub fn openrouter_client() -> Result<openrouter::Client> {
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .context("OPENROUTER_API_KEY unset")?;
    openrouter::Client::new(&key).map_err(|err| anyhow::anyhow!(err))
}

pub fn block_on<F: std::future::Future>(fut: F) -> Result<F::Output> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting tokio for a Rig call")?
        .block_on(fut))
}
