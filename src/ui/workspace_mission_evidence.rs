//! Read-only, exact-receipt capture for a selected shared Mission reason.
//! Stable row IDs preserve focus; only a full current item may request evidence.
use jterm_core::agent_workspace::{
    WorkspaceClient, WorkspaceCommand, WorkspaceEvent, WorkspaceMissionAttention, WorkspaceReply,
    WorkspaceResultReport,
};

const DISPLAY_BYTES: usize = 256 * 1024;

fn bounded_capture(text: &str) -> (&str, bool) {
    let mut end = text.len().min(DISPLAY_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], end != text.len())
}

pub(super) struct MissionEvidence {
    client: WorkspaceClient,
    pending: Option<u64>,
    pub selected: Option<WorkspaceMissionAttention>,
    pub report: Option<Box<WorkspaceResultReport>>,
    pub notice: String,
}

impl Default for MissionEvidence {
    fn default() -> Self {
        Self {
            client: WorkspaceClient::new(),
            pending: None,
            selected: None,
            report: None,
            notice: "Select a reason to review its details and exact available receipt.".into(),
        }
    }
}

impl MissionEvidence {
    pub fn select(
        &mut self,
        item: &WorkspaceMissionAttention,
        current: &[WorkspaceMissionAttention],
        dirty: bool,
    ) {
        if !dirty
            && current.contains(item)
            && self.selected.as_ref() == Some(item)
            && self.pending.is_some()
        {
            return;
        }
        self.pending = None;
        self.report = None;
        self.selected = None;
        if dirty || !current.contains(item) {
            self.notice = "This reason changed. Save or discard edits and inspect the current plan before reviewing its evidence.".into();
            return;
        }
        self.selected = Some(item.clone());
        let Some(task) = &item.task else {
            self.notice = "No unambiguous hosted run is attached to this reason. Its saved details are shown below; use the plan and existing task controls to investigate.".into();
            return;
        };
        match self.client.send(WorkspaceCommand::InspectResult {
            id: task.conversation.clone(),
        }) {
            Ok(id) => {
                self.pending = Some(id);
                self.notice = "Reading the exact hosted run and receipt…".into();
            }
            Err(error) => self.notice = format!("Evidence was not requested: {error}"),
        }
    }

    pub fn reconcile(&mut self, current: &[WorkspaceMissionAttention], dirty: bool) {
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| dirty || !current.contains(selected))
        {
            self.pending = None;
            self.selected = None;
            self.report = None;
            self.notice = "The selected reason or plan changed. Previous evidence was cleared; select a current reason again.".into();
        }
    }

    pub fn poll(&mut self, current: &[WorkspaceMissionAttention], dirty: bool) {
        self.reconcile(current, dirty);
        for WorkspaceEvent::Reply { request_id, result } in self.client.poll() {
            self.receive(request_id, result);
        }
    }

    fn receive(&mut self, request_id: u64, result: Result<WorkspaceReply, String>) {
        if self.pending != Some(request_id) {
            return;
        }
        self.pending = None;
        self.report = None;
        match result {
            Ok(WorkspaceReply::Result { report })
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|selected| selected.matches_result(&report)) =>
            {
                self.notice = "Exact receipt captured below. This is read-only evidence, not a new validation or delivery pass.".into();
                self.report = Some(report);
            }
            Ok(_) => {
                self.notice = "The host no longer exposes this exact run and receipt. No newer result was substituted. Inspect the Mission again.".into();
            }
            Err(error) => self.notice = format!("Could not read the exact receipt: {error}"),
        }
    }

    pub fn text(&self) -> String {
        let Some(item) = &self.selected else {
            return String::new();
        };
        let mut text = format!(
            "{}\n{}\nCandidate: {}\nCheck: {}\nRequired environment: {}\nReceipt: {}\n",
            item.kind.label(),
            item.detail,
            item.candidate.as_deref().unwrap_or("Not frozen"),
            item.check_name.as_deref().unwrap_or("Not applicable"),
            item.environment.as_deref().unwrap_or("Not recorded"),
            item.receipt_id.as_deref().unwrap_or("Not available"),
        );
        if let Some(task) = &item.task {
            text.push_str(&format!(
                "Saved task: {}\nSaved run: {}\n",
                task.task_id, task.run_id
            ));
        }
        if let Some(report) = &self.report {
            let (output, output_clipped) = bounded_capture(&report.validation_output);
            text.push_str(&format!(
                "\nCaptured run: {} · sequence {}\nValidation: {} · evidence current: {}\n{}\n\nValidation output{}:\n{}",
                report.run_id,
                report.source_sequence,
                report.validation_status,
                report.validation_current,
                report.error.as_deref().unwrap_or(""),
                if report.validation_output_truncated || output_clipped { " (truncated)" } else { "" },
                output,
            ));
            if let Some(current) = &report.current {
                let (diff, diff_clipped) = bounded_capture(&current.diff);
                text.push_str(&format!(
                    "\n\nCurrent worktree capture at {} ms (not necessarily the validation input)\nHEAD: {}\nFingerprint: {}\nDiff{}:\n{}",
                    current.captured_at_ms,
                    current.head,
                    current.fingerprint,
                    if current.diff_truncated || diff_clipped {
                        " (truncated)"
                    } else {
                        ""
                    },
                    diff,
                ));
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::agent_task::TaskId;
    use jterm_core::agent_workspace::{
        ConversationId, WorkspaceMissionAttentionKind, WorkspaceMissionTaskRef,
    };

    fn fixture() -> (WorkspaceMissionAttention, Box<WorkspaceResultReport>) {
        let task_id = TaskId::new();
        let conversation = ConversationId(format!(
            "jterm:task:v1:{}",
            serde_json::to_string(&("host", "forge", task_id.to_string())).unwrap()
        ));
        let item = WorkspaceMissionAttention {
            id: "stable-reason".into(),
            mission_id: "mission".into(),
            revision: 1,
            mission_title: "Delivery".into(),
            repository: None,
            candidate: Some("a".repeat(40)),
            kind: WorkspaceMissionAttentionKind::ValidationFailed,
            check_name: Some("validation".into()),
            environment: Some("Linux".into()),
            receipt_id: Some("workspace:run-1:7".into()),
            task: Some(WorkspaceMissionTaskRef {
                conversation: conversation.clone(),
                task_id,
                run_id: "run-1".into(),
                source_sequence: 5,
            }),
            detail: "The required check failed".into(),
        };
        let report = Box::new(WorkspaceResultReport {
            conversation,
            task_id,
            run_id: "run-1".into(),
            source_sequence: 7,
            baseline: None,
            current: None,
            validation: None,
            validation_current: false,
            validation_status: "Failed".into(),
            validation_output: "exact captured failure".into(),
            validation_output_truncated: true,
            error: None,
        });
        (item, report)
    }

    #[test]
    fn exact_receipt_is_read_only_and_keeps_failure_and_truncation_visible() {
        let (item, report) = fixture();
        let mut evidence = MissionEvidence {
            selected: Some(item),
            pending: Some(1),
            ..Default::default()
        };
        evidence.receive(1, Ok(WorkspaceReply::Result { report }));
        assert!(evidence.report.is_some());
        let text = evidence.text();
        assert!(text.contains("Validation: Failed · evidence current: false"));
        assert!(text.contains("Validation output (truncated)"));
        assert!(text.contains("exact captured failure"));
    }

    #[test]
    fn newer_receipt_and_other_run_cannot_replace_selected_evidence() {
        for changed_run in [false, true] {
            let (item, mut report) = fixture();
            if changed_run {
                report.run_id = "run-2".into();
            } else {
                report.source_sequence += 1;
            }
            let mut evidence = MissionEvidence {
                selected: Some(item),
                pending: Some(1),
                ..Default::default()
            };
            evidence.receive(1, Ok(WorkspaceReply::Result { report }));
            assert!(evidence.report.is_none());
            assert!(evidence.notice.contains("No newer result was substituted"));
        }
    }

    #[test]
    fn same_id_and_revision_do_not_authorize_changed_receipts_or_details() {
        let (item, report) = fixture();
        for dirty in [false, true] {
            let mut changed = item.clone();
            if !dirty {
                changed.detail.push_str(" changed");
            }
            let mut evidence = MissionEvidence {
                selected: Some(item.clone()),
                pending: Some(1),
                ..Default::default()
            };
            evidence.reconcile(std::slice::from_ref(&changed), dirty);
            evidence.receive(
                1,
                Ok(WorkspaceReply::Result {
                    report: report.clone(),
                }),
            );
            assert!(evidence.selected.is_none());
            assert!(evidence.report.is_none());
            evidence.select(&item, &[changed], dirty);
            assert!(evidence.pending.is_none());
        }
    }

    #[test]
    fn unlinked_reason_and_late_reply_never_start_or_replace_a_capture() {
        let (mut item, report) = fixture();
        item.task = None;
        let mut evidence = MissionEvidence::default();
        evidence.select(&item, std::slice::from_ref(&item), false);
        assert!(evidence.pending.is_none());
        assert!(evidence.notice.contains("No unambiguous hosted run"));
        evidence.receive(99, Ok(WorkspaceReply::Result { report }));
        assert!(evidence.report.is_none());
        assert!(evidence.text().contains("workspace:run-1:7"));
    }

    #[test]
    fn large_captures_keep_a_bounded_valid_utf8_prefix() {
        let text = "中🙂".repeat(DISPLAY_BYTES);
        let (shown, truncated) = bounded_capture(&text);
        assert!(truncated);
        assert!(shown.len() <= DISPLAY_BYTES);
        assert!(text.starts_with(shown));
        assert_eq!(bounded_capture("small"), ("small", false));
    }

    #[test]
    fn repeated_click_does_not_reissue_an_inflight_read() {
        let (item, _) = fixture();
        let mut evidence = MissionEvidence {
            selected: Some(item.clone()),
            pending: Some(99),
            ..Default::default()
        };
        evidence.select(&item, std::slice::from_ref(&item), false);
        assert_eq!(evidence.pending, Some(99));
    }
}
