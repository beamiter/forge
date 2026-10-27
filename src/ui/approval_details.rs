//! Read-only projection of the exact, frozen approval supplied by the core.
use crate::agent_task::{CodexAppServerApproval, CodexAppServerApprovalKind, TaskId};

pub(super) const PAGE_BYTES: usize = 4096;
const DETAIL_BYTES: usize = 512 * 1024;

/// Local render identity is separate from the worker's private protocol identity.
pub(super) fn is_current(
    task: TaskId,
    approval: &CodexAppServerApproval,
    generation: u64,
    current_task: TaskId,
    current_approvals: &[CodexAppServerApproval],
    current_generation: u64,
) -> bool {
    task == current_task
        && generation == current_generation
        && current_approvals.iter().any(|current| current == approval)
}

/// Bounded output, with UTF-8-safe pages. No reconstructed commands or diffs.
pub(super) fn pages(approval: &CodexAppServerApproval) -> Vec<String> {
    let mut text = String::new();
    let mut truncated = false;
    let mut append = |value: &str| {
        let remaining = DETAIL_BYTES.saturating_sub(text.len());
        let mut end = remaining.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        text.push_str(&value[..end]);
        truncated |= end != value.len();
    };
    append("Frozen request details supplied by the provider. Read-only.\n");
    append("Command, directory and reason are limited to 4096 bytes each by the protocol; incomplete approval evidence is rejected by the core.\n\n");
    for (label, value) in [
        ("Item", Some(approval.item_id.as_str())),
        ("Command", approval.command.as_deref()),
        ("Directory", approval.cwd.as_deref()),
        ("Reason", approval.reason.as_deref()),
    ] {
        append(label);
        append(":\n");
        append(
            value
                .filter(|value| !value.is_empty())
                .unwrap_or("Not supplied by the protocol."),
        );
        append("\n\n");
    }
    if approval.kind == CodexAppServerApprovalKind::FileChange {
        append("Files (frozen at request registration):\n");
        if approval.file_changes.is_empty() {
            append("File content not supplied.\n");
        }
        for file in &approval.file_changes {
            for (label, value) in [
                ("Path", file.path.as_str()),
                ("Change", file.kind.as_str()),
                (
                    "Move destination",
                    file.move_path.as_deref().unwrap_or("Not supplied."),
                ),
                ("Diff", file.diff.as_str()),
            ] {
                append(label);
                append(":\n");
                append(value);
                append("\n\n");
            }
        }
    }
    if truncated {
        text.push_str("\n[Display limit reached. Remaining supplied details omitted; this is not the full scope.]\n");
    }
    let mut pages = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let mut end = PAGE_BYTES.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        pages.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{ApprovalId, CodexAppServerApprovalFileChange};

    pub(super) fn approval() -> CodexAppServerApproval {
        CodexAppServerApproval {
            id: ApprovalId::new(),
            kind: CodexAppServerApprovalKind::Command,
            item_id: "item-1".into(),
            command: Some(format!("{} END <b>literal</b>", "x".repeat(2000))),
            cwd: Some("/tmp/project".into()),
            reason: None,
            file_paths: vec![],
            file_changes: vec![],
        }
    }

    #[test]
    fn long_command_is_complete_and_missing_reason_is_honest() {
        let approval = approval();
        let result = pages(&approval).concat();
        assert!(result.contains(approval.command.as_ref().unwrap()));
        assert!(result.contains("Reason:\nNot supplied by the protocol."));
        assert!(pages(&approval).iter().all(|page| page.len() <= PAGE_BYTES));
    }

    #[test]
    fn multiple_file_changes_keep_exact_frozen_evidence_across_pages() {
        let mut approval = approval();
        approval.kind = CodexAppServerApprovalKind::FileChange;
        approval.file_changes = (0..3)
            .map(|i| CodexAppServerApprovalFileChange {
                path: format!("src/{i}.rs"),
                kind: "update".into(),
                diff: format!("{}\nEND-{i}", "+界\n".repeat(1500)),
                move_path: Some(format!("src/new-{i}.rs")),
            })
            .collect();
        let result = pages(&approval).concat();
        for file in &approval.file_changes {
            assert!(result.contains(&file.path));
            assert!(result.contains(&file.diff));
            assert!(result.contains(file.move_path.as_ref().unwrap()));
        }
        assert!(pages(&approval).len() > 2);
        assert!(pages(&approval).iter().all(|page| page.len() <= PAGE_BYTES));
    }

    #[test]
    fn same_task_replacement_cancellation_and_aba_are_not_current() {
        let task = TaskId::new();
        let approval = approval();
        assert!(is_current(
            task,
            &approval,
            1,
            task,
            std::slice::from_ref(&approval),
            1
        ));
        assert!(!is_current(
            task,
            &approval,
            1,
            TaskId::new(),
            std::slice::from_ref(&approval),
            1
        ));
        assert!(!is_current(
            task,
            &approval,
            1,
            task,
            std::slice::from_ref(&approval),
            2
        ));
        assert!(!is_current(task, &approval, 1, task, &[], 1));
        let mut replacement = approval.clone();
        replacement.reason = Some("changed".into());
        assert!(!is_current(task, &approval, 1, task, &[replacement], 1));
    }

    #[test]
    fn defensive_limit_is_explicit_and_bounded() {
        let mut approval = approval();
        approval.command = Some("界".repeat(DETAIL_BYTES));
        let result = pages(&approval).concat();
        assert!(result.contains("Remaining supplied details omitted"));
        assert!(result.len() < DETAIL_BYTES + PAGE_BYTES);
    }
}
