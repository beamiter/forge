//! Pure fuzzy-ranking layer for the unified command palette.
//!
//! Prefixes narrow the source: `>` actions, `@` persisted history, `:`
//! workflows and `?` natural-language command generation.

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::path::PathBuf;

use crate::keybindings::{Action, KeybindingMap};
use crate::workflows::Workflow;

const MAX_AI_QUERY_BYTES: usize = 64 * 1024;
const MAX_AI_QUERY_LABEL_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteMode {
    All,
    Commands,
    History,
    Ai,
    Workflows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Query {
    pub(crate) mode: PaletteMode,
    pub(crate) text: String,
}

impl Query {
    pub(crate) fn parse(raw: &str, default_mode: PaletteMode) -> Self {
        let trimmed = raw.trim_start();
        for (prefix, mode) in [
            ('>', PaletteMode::Commands),
            ('@', PaletteMode::History),
            ('?', PaletteMode::Ai),
            (':', PaletteMode::Workflows),
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return Self {
                    mode,
                    text: rest.trim_start().to_string(),
                };
            }
        }
        Self {
            mode: default_mode,
            text: trimmed.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Accept {
    Action(Action),
    TypeCommand(String),
    AskAi(String),
    RunWorkflow(PathBuf),
}

/// One immutable history row captured when the palette opens. Keeping this
/// separate from the JSONL wire record lets live Block history represent an
/// unreported status without inventing a successful exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryEntry {
    pub(crate) command: String,
    pub(crate) cwd: Option<String>,
    pub(crate) exit_code: Option<i32>,
}

impl From<jterm_core::command_history::CommandHistoryRecord> for HistoryEntry {
    fn from(record: jterm_core::command_history::CommandHistoryRecord) -> Self {
        Self {
            command: record.command,
            cwd: record.cwd,
            exit_code: Some(record.exit_code),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) tier: u8,
    pub(crate) score: i64,
    pub(crate) label: String,
    pub(crate) sublabel: Option<String>,
    pub(crate) right: Option<String>,
    pub(crate) accept: Accept,
}

pub(crate) fn gather(
    query: &Query,
    keybindings: &KeybindingMap,
    history: &[HistoryEntry],
    workflows: &[Workflow],
    limit: usize,
) -> Vec<Entry> {
    let matcher = SkimMatcherV2::default().smart_case();
    let mut entries = Vec::new();

    if matches!(query.mode, PaletteMode::All | PaletteMode::Commands) {
        for (action, binding) in keybindings.all_bound_actions() {
            push_if_match(
                &matcher,
                &query.text,
                Entry {
                    tier: 0,
                    score: 0,
                    label: action.name().to_string(),
                    sublabel: None,
                    right: (!binding.is_empty()).then_some(binding),
                    accept: Accept::Action(action),
                },
                &mut entries,
            );
        }
    }

    if matches!(query.mode, PaletteMode::All | PaletteMode::Workflows) {
        for workflow in workflows {
            let tag_text = workflow.tags.join(",");
            let searchable = if tag_text.is_empty() {
                workflow.description.clone()
            } else if workflow.description.is_empty() {
                tag_text.clone()
            } else {
                format!("{} · {tag_text}", workflow.description)
            };
            push_if_match(
                &matcher,
                &query.text,
                Entry {
                    tier: 1,
                    score: 0,
                    label: format!("⚙ {}", workflow.name),
                    sublabel: Some(if searchable.is_empty() {
                        workflow.command.clone()
                    } else {
                        searchable
                    }),
                    right: (!tag_text.is_empty()).then(|| format!(":{tag_text}")),
                    accept: Accept::RunWorkflow(workflow.source_path.clone()),
                },
                &mut entries,
            );
        }
    }

    if query.mode == PaletteMode::Ai {
        let text = query.text.trim();
        let too_large = text.len() > MAX_AI_QUERY_BYTES;
        let display_text = bounded_label(
            &jterm_core::review_input::safe_inline_display(text, 16 * 1024),
            MAX_AI_QUERY_LABEL_CHARS,
        );
        entries.push(Entry {
            tier: 0,
            score: i64::MAX,
            label: if text.is_empty() {
                "Type a natural-language request after ?".to_string()
            } else if too_large {
                "AI request is too large (64 KiB limit)".to_string()
            } else {
                format!("Ask AI: {display_text}")
            },
            sublabel: Some(if text.is_empty() {
                "e.g. ? find files modified today".to_string()
            } else if too_large {
                "Shorten the request before generating a command".to_string()
            } else {
                "Generates a shell command for review before running".to_string()
            }),
            right: Some("?".to_string()),
            accept: if text.is_empty() || too_large {
                Accept::TypeCommand(String::new())
            } else {
                Accept::AskAi(text.to_string())
            },
        });
        return entries;
    }

    if matches!(query.mode, PaletteMode::All | PaletteMode::History) {
        let count = history.len();
        for (index, item) in history.iter().enumerate() {
            if jterm_core::review_input::validate(&item.command).is_err() {
                continue;
            }
            let cwd = item
                .cwd
                .as_deref()
                .filter(|cwd| {
                    cwd.len() <= 16 * 1024
                        && !cwd.chars().any(char::is_control)
                        && !jterm_core::review_input::contains_visual_spoofing(cwd)
                })
                .map(|cwd| bounded_label(cwd, 256))
                .unwrap_or_default();
            let status = match item.exit_code {
                Some(0) => "success".to_string(),
                Some(exit_code) => format!("exit {exit_code}"),
                None => "status unreported".to_string(),
            };
            push_if_match(
                &matcher,
                &query.text,
                Entry {
                    tier: 2,
                    score: (count - index) as i64,
                    label: bounded_label(&item.command, 512),
                    sublabel: Some(if cwd.is_empty() {
                        status
                    } else {
                        format!("{status} · {cwd}")
                    }),
                    right: None,
                    accept: Accept::TypeCommand(item.command.clone()),
                },
                &mut entries,
            );
        }
    }

    entries.sort_by(|a, b| a.tier.cmp(&b.tier).then(b.score.cmp(&a.score)));
    entries.truncate(limit);
    entries
}

fn bounded_label(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn push_if_match(
    matcher: &SkimMatcherV2,
    needle: &str,
    mut entry: Entry,
    entries: &mut Vec<Entry>,
) {
    if needle.is_empty() {
        entries.push(entry);
        return;
    }
    let primary = matcher.fuzzy_match(&entry.label, needle);
    let secondary = entry
        .sublabel
        .as_deref()
        .and_then(|value| matcher.fuzzy_match(value, needle));
    if let Some(score) = match (primary, secondary) {
        (Some(a), Some(b)) => Some(a.max(b / 2)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b / 2),
        (None, None) => None,
    } {
        entry.score = entry.score.saturating_add(score.saturating_mul(10_000));
        entries.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_recovery_and_failed_navigation_actions_reach_palette_dispatch() {
        let bindings = KeybindingMap::from_defaults();
        for action in [
            Action::UndoClearBlocks,
            Action::JumpToPrevFailed,
            Action::JumpToNextFailed,
        ] {
            let entries = gather(
                &Query {
                    mode: PaletteMode::Commands,
                    text: action.name().to_string(),
                },
                &bindings,
                &[],
                &[],
                100,
            );
            assert!(entries
                .iter()
                .any(|entry| matches!(&entry.accept, Accept::Action(found) if *found == action)));
        }
    }

    #[test]
    fn indexed_remote_actions_are_visible_in_the_command_palette() {
        let bindings = KeybindingMap::from_defaults();
        for action in [Action::ConnectRemote(0), Action::ConnectRemote(8)] {
            let entries = gather(
                &Query {
                    mode: PaletteMode::Commands,
                    text: action.name().to_string(),
                },
                &bindings,
                &[],
                &[],
                100,
            );
            assert!(entries
                .iter()
                .any(|entry| matches!(&entry.accept, Accept::Action(found) if *found == action)));
        }
    }

    #[test]
    fn prefixes_select_sources() {
        assert_eq!(
            Query::parse("  @ cargo", PaletteMode::All),
            Query {
                mode: PaletteMode::History,
                text: "cargo".into()
            }
        );
        assert_eq!(
            Query::parse(":deploy", PaletteMode::All).mode,
            PaletteMode::Workflows
        );
        assert_eq!(
            Query::parse("? explain", PaletteMode::All).mode,
            PaletteMode::Ai
        );
        assert_eq!(
            Query::parse("> close", PaletteMode::All).mode,
            PaletteMode::Commands
        );
    }

    #[test]
    fn fuzzy_actions_are_ranked_and_limited() {
        let entries = gather(
            &Query::parse("> newtab", PaletteMode::All),
            &KeybindingMap::from_defaults(),
            &[],
            &[],
            5,
        );
        assert!(!entries.is_empty());
        assert!(entries[0].label.to_ascii_lowercase().contains("new tab"));
        assert!(entries.len() <= 5);
    }

    #[test]
    fn ai_query_never_auto_executes() {
        let entries = gather(
            &Query::parse("? list large files", PaletteMode::All),
            &KeybindingMap::from_defaults(),
            &[],
            &[],
            10,
        );
        assert!(matches!(&entries[0].accept, Accept::AskAi(text) if text == "list large files"));
    }

    #[test]
    fn ai_query_labels_are_safe_but_the_model_request_stays_exact() {
        let request = "list\n\u{202e}\u{fff0}\u{e0080}files";
        let entries = gather(
            &Query::parse(&format!("? {request}"), PaletteMode::All),
            &KeybindingMap::from_defaults(),
            &[],
            &[],
            10,
        );

        assert_eq!(entries[0].label, "Ask AI: list����files");
        assert!(matches!(&entries[0].accept, Accept::AskAi(text) if text == request));
    }

    #[test]
    fn oversized_ai_queries_are_not_activatable() {
        let query = Query {
            mode: PaletteMode::Ai,
            text: "x".repeat(MAX_AI_QUERY_BYTES + 1),
        };
        let entries = gather(&query, &KeybindingMap::from_defaults(), &[], &[], 10);

        assert!(entries[0].label.contains("too large"));
        assert!(matches!(&entries[0].accept, Accept::TypeCommand(text) if text.is_empty()));
    }

    #[test]
    fn history_palette_rejects_visual_spoofing_and_bounds_labels() {
        let path = std::env::temp_dir().join(format!(
            "forge-palette-history-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let safe = "x".repeat(600);
        let contents = format!(
            "{}\n{}\n",
            serde_json::json!({"command": safe, "exit_code": 0}),
            serde_json::json!({"command": "echo safe\u{202e}txt", "exit_code": 0})
        );
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let history: Vec<HistoryEntry> = jterm_core::command_history::read_recent(&path, 10)
            .unwrap()
            .into_iter()
            .map(HistoryEntry::from)
            .collect();
        let entries = gather(
            &Query::parse("@", PaletteMode::All),
            &KeybindingMap::from_defaults(),
            &history,
            &[],
            10,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label.chars().count(), 513);
        assert!(entries[0].label.ends_with('…'));
        assert!(matches!(&entries[0].accept, Accept::TypeCommand(command) if command.len() == 600));
        let _ = std::fs::remove_file(path);
    }
}
