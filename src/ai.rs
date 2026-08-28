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

/// Which AI action a finished block's context menu asked for. `Ask` keeps the
/// panel's long-standing opening question; `Explain` is the family's
/// failed-block Explain (ember's `AgentTaskIntent::Explain`, frost's
/// `FailedBlockAgentIntent::Explain`) routed into forge's read-only block-chat
/// panel. The sibling Fix action does not travel this channel: it creates an
/// agent task instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockAiIntent {
    Ask,
    Explain,
}

/// The fixed opening question seeded into the AI panel alongside a block's
/// context. Every variant is a compile-time constant: the command and output
/// are untrusted PTY evidence and travel only inside the framed `BlockContext`
/// envelope, never interpolated into the instruction where model-looking text
/// could impersonate forge (ember/frost's rule).
pub(crate) fn seeded_block_question(intent: BlockAiIntent, exit_code: i32) -> &'static str {
    match intent {
        BlockAiIntent::Ask => {
            if exit_code == 0 {
                "Explain what this command does and what its output means."
            } else {
                "This command failed. Diagnose the error and suggest a fix."
            }
        }
        // frost's wording, adapted only to the panel's "this command"
        // vocabulary: the chat panel cannot change files, so the request is
        // read-only by construction.
        BlockAiIntent::Explain => {
            "Explain this failed command: identify the root cause, cite the relevant evidence in its captured output, and propose the smallest safe next step. Do not propose changes unless I ask."
        }
    }
}

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
    fn api_key_file_resolution_preserves_exact_values_for_validation() {
        // Neutral identity ⇒ JTERM_ prefix; no other test reads this variable.
        let var = "JTERM_AI_API_KEY_FILE";
        std::env::set_var(var, " /run/probe.key ");
        assert_eq!(
            api_key_file_env_override(),
            Some(" /run/probe.key ".to_string())
        );
        assert_eq!(
            resolve_api_key_file(Some("/cfg/ai.key")),
            Some(" /run/probe.key ".to_string())
        );
        std::env::set_var(var, "   ");
        assert_eq!(api_key_file_env_override(), Some("   ".to_string()));
        assert_eq!(
            resolve_api_key_file(Some(" /cfg/ai.key ")),
            Some("   ".to_string())
        );
        std::env::remove_var(var);
        assert_eq!(
            resolve_api_key_file(Some(" /cfg/ai.key ")),
            Some(" /cfg/ai.key ".to_string())
        );
        assert_eq!(resolve_api_key_file(Some("")), None);
        assert_eq!(resolve_api_key_file(None), None);
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

    #[test]
    fn seeded_questions_are_fixed_strings_with_no_block_interpolation() {
        // The Ask defaults are the panel's long-standing behavior and must not
        // drift: success explains, anything else (including the -1 unknown
        // sentinel) diagnoses.
        assert_eq!(
            seeded_block_question(BlockAiIntent::Ask, 0),
            "Explain what this command does and what its output means."
        );
        assert_eq!(
            seeded_block_question(BlockAiIntent::Ask, 1),
            "This command failed. Diagnose the error and suggest a fix."
        );
        assert_eq!(
            seeded_block_question(BlockAiIntent::Ask, -1),
            "This command failed. Diagnose the error and suggest a fix."
        );
        let explain = seeded_block_question(BlockAiIntent::Explain, 2);
        assert!(explain.contains("root cause"));
        assert!(explain.contains("Do not propose changes unless I ask."));
        // A failed block whose output begs to be quoted still gets the bare
        // constant: evidence stays inside the framed context envelope.
        for intent in [BlockAiIntent::Ask, BlockAiIntent::Explain] {
            let question = seeded_block_question(intent, 1);
            assert!(!question.contains("rm -rf"));
            assert!(!question.contains("{cmd}"));
        }
    }
}
