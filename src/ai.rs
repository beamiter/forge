//! Provider-neutral AI helpers for terminal chat, NL-to-command, and Agent mode.
//!
//! Everything except forge's Config → client mapping lives in
//! `jterm_core::ai`: provider wire formats, the host-curl transport (secrets
//! stay out of argv), request-history budgeting, secret redaction, response
//! parsing, and the bounded conversation snapshot format. This module is a
//! thin re-export plus the one construction boundary forge owns. No function
//! reachable from here executes or submits a generated command.
//!
//! Privacy: nothing leaves the machine without an explicit user action
//! (clicking Explain, asking the panel, hitting `?` in the palette). AI-bound
//! text is bounded by callers and scrubbed for common high-confidence secret
//! formats at the shared provider boundary.

pub use jterm_core::ai::{
    agent_user_prompt, api_key_file_env_override, build_agent_system_prompt, build_system_prompt,
    default_api_key_path, nl_to_command_with_context_blocking_cancellable, resolve_api_key_file,
    truncate_for_context, user_prompt_with_block_context, write_api_key_file, AiCancellationToken,
    AiClient, AiError, AiSettings, BlockContext, ChatSnapshot, ConversationSnapshot,
    ConversationSnapshotError, Provider, Role, Turn, MAX_CONVERSATION_SNAPSHOT_JSON_BYTES,
    MAX_PERSISTED_CHATS,
};

/// Build the hardened shared AI client from forge's application-owned config.
/// The config carries only a credential path; key material is resolved inside
/// the shared client and is never copied into persistent configuration state.
///
/// `jterm_core::ai::AiClient::new` deliberately defers endpoint validation to
/// request-build time, but forge's contract (see
/// `config::resolve_ai_base_url`) is that an explicit unsafe destination is
/// rejected here, before any credential-file or network I/O.
pub fn client_from_config(config: &crate::config::Config) -> Result<AiClient, AiError> {
    if config.ai_enabled
        && !crate::config::ai_base_url_is_safe(&config.ai_provider, &config.ai_base_url)
    {
        return Err(AiError::InvalidConfiguration(
            "AI endpoint must use HTTPS unless plain HTTP targets a loopback host".into(),
        ));
    }
    AiClient::from_settings(&AiSettings {
        enabled: config.ai_enabled,
        provider: config.ai_provider.clone(),
        api_key_file: config.ai_api_key_file.clone(),
        model: config.ai_model.clone(),
        base_url: config.ai_base_url.clone(),
        max_tokens: config.ai_max_tokens,
        temperature: config.ai_temperature,
        redact_secrets: config.ai_redact_secrets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_api_key_path_is_per_app_identity() {
        // Tests never call identity::init, so the neutral "jterm" name holds.
        let path = default_api_key_path();
        assert!(path.ends_with("jterm/ai.key"), "unexpected path: {path}");
    }

    #[test]
    fn api_key_file_resolution_prefers_env_and_ignores_blanks() {
        // Neutral identity ⇒ JTERM_ prefix; no other test reads this variable.
        let var = "JTERM_AI_API_KEY_FILE";
        std::env::set_var(var, " /run/probe.key ");
        assert_eq!(
            api_key_file_env_override(),
            Some("/run/probe.key".to_string())
        );
        assert_eq!(
            resolve_api_key_file(Some("/cfg/ai.key")),
            Some("/run/probe.key".to_string())
        );
        std::env::set_var(var, "   ");
        assert_eq!(api_key_file_env_override(), None);
        assert_eq!(
            resolve_api_key_file(Some(" /cfg/ai.key ")),
            Some("/cfg/ai.key".to_string())
        );
        assert_eq!(resolve_api_key_file(Some("   ")), None);
        assert_eq!(resolve_api_key_file(None), None);
        std::env::remove_var(var);
    }

    #[test]
    fn client_from_config_respects_disabled_flag() {
        let mut config = crate::config::Config::safe_defaults();
        config.ai_enabled = false;
        assert!(matches!(
            client_from_config(&config),
            Err(AiError::Disabled)
        ));
    }

    #[test]
    fn client_from_config_builds_keyless_ollama_client() {
        let mut config = crate::config::Config::safe_defaults();
        config.ai_enabled = true;
        config.ai_provider = "ollama".into();
        config.ai_base_url = "http://localhost:11434".into();
        config.ai_model = "codellama:7b".into();
        config.ai_max_tokens = 512;
        let client = client_from_config(&config).expect("ollama needs no key");
        assert_eq!(client.provider, Provider::Ollama);
    }

    #[test]
    fn client_from_config_rejects_unsafe_endpoints_before_credentials_or_transport() {
        for (provider, endpoint) in [
            ("openai-compatible", "http://models.example.com:8000/v1"),
            ("ollama", "http://models.example.com:11434"),
            ("anthropic", "https://user:secret@example.com"),
        ] {
            let mut config = crate::config::Config::safe_defaults();
            config.ai_enabled = true;
            config.ai_provider = provider.into();
            config.ai_base_url = endpoint.into();
            config.ai_api_key_file = Some("/definitely/not/read/provider.key".into());
            let error = client_from_config(&config).unwrap_err();
            assert!(
                matches!(error, AiError::InvalidConfiguration(_)),
                "{provider} {endpoint}: {error}"
            );
            assert!(!error.to_string().contains(endpoint));
        }
    }
}
