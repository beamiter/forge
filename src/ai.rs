//! Shim over the shared provider-neutral AI core in `jterm_core`.
//!
//! All client, prompt, and conversation-snapshot logic lives in
//! `jterm_core::ai`; only the Config-to-settings conversion is jterm4-local.

pub use jterm_core::ai::*;

fn settings(config: &crate::config::Config) -> AiSettings {
    AiSettings {
        enabled: config.ai_enabled,
        provider: config.ai_provider.clone(),
        api_key_file: config.ai_api_key_file.clone(),
        model: config.ai_model.clone(),
        base_url: config.ai_base_url.clone(),
        max_tokens: config.ai_max_tokens,
        redact_secrets: config.ai_redact_secrets,
    }
}

/// Build an [`AiClient`] from jterm4's Config (the former
/// `AiClient::from_config`).
pub fn client_from_config(config: &crate::config::Config) -> Result<AiClient, AiError> {
    AiClient::from_settings(&settings(config))
}
