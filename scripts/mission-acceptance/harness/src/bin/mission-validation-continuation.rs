//! Read-only preflight of existing hosted tasks. Never runs validation or claims delivery.
use jterm_core::agent_task::TaskValidationStatus;
use jterm_core::agent_workspace::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const CORE: &str = "4ffa774138ee89dc2916ce21786c45e411196ca4";
const INPUT_BYTES: usize = 65536;
const TASK_LIMIT: usize = 16;
const TOTAL_SECONDS: u64 = 180;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema: u32,
    frozen: bool,
    core: String,
    state_dir: PathBuf,
    mission_id: String,
    mission_revision: u64,
    repositories: Vec<Repository>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    full_name: String,
    revision: String,
    baseline: String,
    path: PathBuf,
}
fn require(ok: bool, reason: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(reason.into())
    }
}
fn full_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 4096
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && !path.as_os_str().as_encoded_bytes().contains(&0)
}
fn validate_input(input: &Input) -> Result<(), String> {
    require(
        input.schema == 1 && input.frozen && input.core == CORE,
        "input_not_frozen_for_compiled_core",
    )?;
    require(absolute_path(&input.state_dir), "invalid_state_path")?;
    require(
        input.mission_id.starts_with("mission:")
            && input.mission_id.len() > 8
            && input.mission_id.len() <= 256
            && !input.mission_id.chars().any(char::is_control)
            && input.mission_revision > 0,
        "missing_saved_mission_identity",
    )?;
    require(
        input.repositories.len() == 5,
        "exactly_five_repositories_required",
    )?;
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for repo in &input.repositories {
        require(
            FOUR_TERMINAL_REPOSITORIES
                .iter()
                .any(|(name, _)| *name == repo.full_name)
                && names.insert(&repo.full_name),
            "unapproved_or_duplicate_repository",
        )?;
        require(
            full_sha(&repo.revision) && full_sha(&repo.baseline),
            "full_lowercase_commit_required",
        )?;
        require(
            absolute_path(&repo.path) && paths.insert(&repo.path),
            "invalid_or_duplicate_checkout",
        )?;
        if repo.full_name == "beamiter/jterm_core" {
            require(repo.revision == CORE, "core_candidate_mismatch")?;
        }
    }
    Ok(())
}
fn read_input(path: &Path) -> Result<Input, String> {
    // O_NONBLOCK makes even an attacker-substituted FIFO fail without hanging.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "input_open_failed")?;
    let metadata = file.metadata().map_err(|_| "input_metadata_failed")?;
    require(
        metadata.is_file() && metadata.len() <= INPUT_BYTES as u64,
        "input_must_be_bounded_regular_file",
    )?;
    let mut data = Vec::new();
    file.take(INPUT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "input_read_failed")?;
    require(data.len() <= INPUT_BYTES, "input_byte_budget_exceeded")?;
    let input: Input = serde_json::from_slice(&data).map_err(|_| "input_schema_invalid")?;
    validate_input(&input)?;
    Ok(input)
}
fn existing_host(state: &Path) -> Result<(), String> {
    // Core uses a private hashed runtime path for long roots. Do not copy that
    // algorithm or create a second endpoint: only its simple short-root case.
    require(
        absolute_path(state) && state.as_os_str().len() < 70,
        "existing_short_absolute_state_required",
    )?;
    require(
        fs::canonicalize(state).is_ok_and(|p| p == state),
        "state_missing_or_not_canonical",
    )?;
    for path in [
        state.to_path_buf(),
        state.join("run"),
        state.join("run/leases"),
    ] {
        let m = fs::symlink_metadata(path).map_err(|_| "existing_private_runtime_required")?;
        require(
            m.is_dir()
                && !m.file_type().is_symlink()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o077 == 0,
            "existing_private_runtime_required",
        )?;
    }
    let m = fs::symlink_metadata(state.join("run/host.sock"))
        .map_err(|_| "existing_host_socket_required")?;
    require(
        m.file_type().is_socket() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
        "existing_private_host_socket_required",
    )
}
fn allowed(command: &WorkspaceCommand) -> bool {
    matches!(
        command,
        WorkspaceCommand::ReadMission { .. }
            | WorkspaceCommand::InspectNative { .. }
            | WorkspaceCommand::InspectResult { .. }
    )
}
fn remaining(deadline: Instant) -> Result<Duration, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    require(!left.is_zero(), "overall_request_deadline")?;
    Ok(left.min(Duration::from_secs(70)))
}
fn request(
    client: &WorkspaceClient,
    command: WorkspaceCommand,
    deadline: Instant,
) -> Result<WorkspaceReply, String> {
    require(allowed(&command), "command_not_read_only_allowlisted")?;
    let request_deadline = Instant::now() + remaining(deadline)?;
    let id = client.send(command).map_err(|_| "request_not_sent")?;
    loop {
        // A response arriving after the budget cannot restore freshness.
        require(
            Instant::now() < request_deadline && Instant::now() < deadline,
            "request_deadline",
        )?;
        for WorkspaceEvent::Reply { request_id, result } in client.poll() {
            if request_id == id {
                return result.map_err(|_| "host_request_failed".to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn read_mission(
    client: &WorkspaceClient,
    input: &Input,
    deadline: Instant,
) -> Result<WorkspaceMission, String> {
    match request(
        client,
        WorkspaceCommand::ReadMission {
            id: input.mission_id.clone(),
        },
        deadline,
    )? {
        WorkspaceReply::Mission { mission, .. } => Ok(mission),
        _ => Err("unexpected_mission_reply".into()),
    }
}
fn check_plan(input: &Input, mission: &WorkspaceMission) -> Result<(), String> {
    require(
        mission.id == input.mission_id && mission.revision == input.mission_revision,
        "saved_mission_identity_changed",
    )?;
    require(mission.validate().is_ok(), "saved_mission_invalid")?;
    require(
        mission.required_environment == "local" && mission.evidence_after_ms > 0,
        "only_local_validation_contract_supported",
    )?;
    require(
        mission.repositories.len() == 5,
        "saved_repository_set_changed",
    )?;
    let mut seen = BTreeSet::new();
    let mut tasks = BTreeSet::new();
    for repo in &mission.repositories {
        require(
            seen.insert(&repo.identity.full_name),
            "saved_repository_duplicate",
        )?;
        let expected = input
            .repositories
            .iter()
            .find(|r| r.full_name == repo.identity.full_name)
            .ok_or("saved_repository_set_changed")?;
        let canonical = WorkspaceMissionRepositoryIdentity::canonical(&expected.full_name)
            .map_err(|_| "saved_repository_identity_invalid")?;
        require(
            repo.identity == canonical
                && repo.path == expected.path
                && repo.baseline == expected.baseline
                && repo.candidate.as_deref() == Some(&expected.revision)
                && repo.target_ref == "refs/heads/master",
            "saved_candidate_or_checkout_changed",
        )?;
        let pin = if expected.full_name == "beamiter/jterm_core" {
            None
        } else {
            Some(CORE)
        };
        require(
            repo.required_core_pin.as_deref() == pin,
            "saved_core_pin_changed",
        )?;
        require(
            repo.required_checks == ["validation"],
            "only_required_validation_supported",
        )?;
        for task in &repo.tasks {
            require(
                task.validate().is_ok()
                    && tasks.insert((task.conversation.clone(), task.run_id.clone())),
                "duplicate_or_invalid_hosted_task",
            )?;
        }
    }
    require(tasks.len() <= TASK_LIMIT, "hosted_task_budget_exceeded")
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotFacts {
    task: WorkspaceMissionTaskRef,
    path: PathBuf,
    baseline: String,
    idle: bool,
    can_validate: bool,
    recipe_visible: bool,
    command: String,
    shell: String,
    status: TaskValidationStatus,
}
impl SnapshotFacts {
    fn from_native(snapshot: &NativeSnapshot) -> Self {
        Self {
            task: WorkspaceMissionTaskRef {
                conversation: snapshot.id.clone(),
                task_id: snapshot.task.id,
                run_id: snapshot.incarnation.clone(),
                source_sequence: snapshot.sequence,
            },
            path: snapshot.task.worktree_path.clone(),
            baseline: snapshot.task.base_commit.clone(),
            idle: !snapshot.preparing
                && !snapshot.live
                && !snapshot.ownership_locked
                && !snapshot.interrupted
                && !snapshot.handoff_pending
                && snapshot.terminal.is_none()
                && !snapshot.validation.as_ref().is_some_and(|v| v.preparing)
                && snapshot.task.validation.status != TaskValidationStatus::Running,
            can_validate: snapshot.can_start_validation,
            recipe_visible: !snapshot.validation_evidence_redacted,
            command: snapshot
                .task
                .validation_command()
                .unwrap_or_default()
                .to_owned(),
            shell: snapshot
                .task
                .validation_shell()
                .unwrap_or_default()
                .to_owned(),
            status: snapshot.task.validation.status,
        }
    }
}
fn inspect_native(
    client: &WorkspaceClient,
    task: &WorkspaceMissionTaskRef,
    deadline: Instant,
) -> Result<SnapshotFacts, String> {
    match request(
        client,
        WorkspaceCommand::InspectNative {
            id: task.conversation.clone(),
        },
        deadline,
    )? {
        WorkspaceReply::Native {
            snapshot,
            control: None,
        } => Ok(SnapshotFacts::from_native(&snapshot)),
        _ => Err("unexpected_native_or_control_lease_reply".into()),
    }
}
fn check_snapshot(
    snapshot: &SnapshotFacts,
    task: &WorkspaceMissionTaskRef,
    repo: &WorkspaceMissionRepository,
) -> Result<(), String> {
    require(
        snapshot.task.conversation == task.conversation
            && snapshot.task.task_id == task.task_id
            && snapshot.task.run_id == task.run_id
            && snapshot.task.source_sequence >= task.source_sequence,
        "hosted_task_identity_changed",
    )?;
    require(
        snapshot.path == repo.path && snapshot.baseline == repo.baseline,
        "hosted_task_checkout_changed",
    )?;
    require(snapshot.idle, "task_not_idle")?;
    require(
        snapshot.recipe_visible
            && !snapshot.command.trim().is_empty()
            && !snapshot.shell.trim().is_empty(),
        "original_validation_recipe_unavailable",
    )
}
fn assess_result(
    mission: &WorkspaceMission,
    repo: &WorkspaceMissionRepository,
    task: &WorkspaceMissionTaskRef,
    before: &SnapshotFacts,
    after: &SnapshotFacts,
    report: &WorkspaceResultReport,
) -> Result<Option<String>, String> {
    check_snapshot(before, task, repo)?;
    check_snapshot(after, task, repo)?;
    require(
        before == after && WorkspaceMissionTaskRef::from_result(report) == before.task,
        "task_or_sequence_changed_during_capture",
    )?;
    require(report.error.is_none(), "worktree_capture_failed")?;
    let current = report.current.as_ref().ok_or("current_worktree_missing")?;
    require(
        Some(current.head.as_str()) == repo.candidate.as_deref()
            && current.base_commit == repo.baseline
            && current.status.is_empty()
            && !current.status_truncated
            && current.untracked_files == 0
            && !current.fingerprint.is_empty(),
        "current_checkout_not_clean_frozen_candidate",
    )?;
    let baseline = report.baseline.as_ref().ok_or("task_baseline_missing")?;
    require(baseline.head == repo.baseline, "task_baseline_changed")?;
    let Some(evidence) = &report.validation else {
        require(
            before.can_validate,
            "original_validation_action_unavailable",
        )?;
        return Ok(None);
    };
    if before.status != TaskValidationStatus::Passed
        || evidence.started_at_ms <= mission.evidence_after_ms
    {
        require(
            before.can_validate,
            "original_validation_action_unavailable",
        )?;
        return Ok(None);
    }
    require(
        evidence.command == before.command
            && evidence.shell == before.shell
            && Path::new(&evidence.cwd) == repo.path,
        "validation_recipe_or_checkout_changed",
    )?;
    let receipt = WorkspaceMissionValidationEvidence::from_result(
        "validation",
        "local",
        report,
        before.status,
    )
    .map_err(|_| "validation_receipt_stale_or_unbound")?;
    require(
        receipt.candidate == repo.candidate.as_deref().unwrap_or_default()
            && receipt.status == TaskValidationStatus::Passed,
        "validation_receipt_candidate_changed",
    )?;
    Ok(Some(receipt.receipt_id))
}
fn observe(input: &Input) -> Result<Value, String> {
    validate_input(input)?;
    existing_host(&input.state_dir)?;
    let deadline = Instant::now() + Duration::from_secs(TOTAL_SECONDS);
    let client = WorkspaceClient::for_state_dir(&input.state_dir);
    let mission = read_mission(&client, input, deadline)?;
    check_plan(input, &mission)?;
    let mut checks = Vec::new();
    let mut blocked = false;
    let mut action_required = false;
    for repo in &mission.repositories {
        if repo.tasks.is_empty() {
            blocked = true;
            checks.push(json!({"repository":repo.identity.full_name,"status":"blocked","reason":"no_real_hosted_task_linked"}));
        }
        for (index, task) in repo.tasks.iter().enumerate() {
            let result = (|| {
                let before = inspect_native(&client, task, deadline)?;
                check_snapshot(&before, task, repo)?;
                let report = match request(
                    &client,
                    WorkspaceCommand::InspectResult {
                        id: task.conversation.clone(),
                    },
                    deadline,
                )? {
                    WorkspaceReply::Result { report } => report,
                    _ => return Err("unexpected_result_reply".into()),
                };
                let after = inspect_native(&client, task, deadline)?;
                assess_result(&mission, repo, task, &before, &after, &report)
            })();
            let (status, reason, receipt) = match result {
                Ok(Some(receipt)) => (
                    "current_local_validation_observed",
                    "existing_receipt_matches_capture",
                    Some(receipt),
                ),
                Ok(None) => {
                    action_required = true;
                    (
                        "action_required",
                        "use_existing_task_controls_to_validate_then_repeat_preflight",
                        None,
                    )
                }
                Err(reason) => {
                    blocked = true;
                    checks.push(json!({"repository":repo.identity.full_name,"task_index":index,"status":"blocked","reason":reason}));
                    continue;
                }
            };
            checks.push(json!({"repository":repo.identity.full_name,"task_index":index,"status":status,"reason":reason,"receipt_id":receipt}));
        }
    }
    // A same-revision changed plan also fails; do not trust revision alone.
    require(
        read_mission(&client, input, deadline)? == mission,
        "mission_changed_during_observation",
    )?;
    Ok(
        json!({"schema":1,"status":if blocked {"blocked"} else if action_required {"action_required"} else {"current_local_validation_observed"},
        "compiled_core":CORE,"mission_id":mission.id,"mission_revision":mission.revision,"checks":checks,
        "host_binary_core_provenance":"not_verified_by_protocol","delivery":"not_evaluated",
        "task_control_sent":false,"validation_started":false,"point_in_time_only":true}),
    )
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.len() == 1 {
        read_input(Path::new(&args[0])).and_then(|input| observe(&input))
    } else {
        Err("usage_one_explicit_input_json_required".into())
    };
    match result {
        Ok(value) => {
            println!("{value}");
            if value["status"] != "current_local_validation_observed" {
                std::process::exit(2);
            }
        }
        Err(reason) => {
            println!(
                "{}",
                json!({"schema":1,"status":"blocked","reason":reason,"compiled_core":CORE,
                "delivery":"not_evaluated","task_control_sent":false,"validation_started":false})
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::agent_task::{worktree::WorktreeReviewSnapshot, TaskId};
    use std::os::unix::fs::{symlink, PermissionsExt};
    fn input() -> Input {
        Input {
            schema: 1,
            frozen: true,
            core: CORE.into(),
            state_dir: "/tmp/example-host".into(),
            mission_id: "mission:fixture".into(),
            mission_revision: 1,
            repositories: FOUR_TERMINAL_REPOSITORIES
                .iter()
                .map(|(name, _)| Repository {
                    full_name: (*name).into(),
                    revision: if *name == "beamiter/jterm_core" {
                        CORE.into()
                    } else {
                        "b".repeat(40)
                    },
                    baseline: "a".repeat(40),
                    path: PathBuf::from("/tmp").join(name.split('/').next_back().unwrap()),
                })
                .collect(),
        }
    }
    fn mission(input: &Input) -> WorkspaceMission {
        let mut m = WorkspaceMission::new("Test", "Read only");
        m.id = input.mission_id.clone();
        m.revision = input.mission_revision;
        m.required_environment = "local".into();
        m.evidence_after_ms = 10;
        m.repositories = input
            .repositories
            .iter()
            .map(|r| {
                let mut repo = WorkspaceMissionRepository::new(&r.full_name, &r.path).unwrap();
                repo.baseline = r.baseline.clone();
                repo.candidate = Some(r.revision.clone());
                if r.full_name != "beamiter/jterm_core" {
                    repo.required_core_pin = Some(CORE.into());
                }
                repo
            })
            .collect();
        m
    }
    fn fixture() -> (
        WorkspaceMission,
        WorkspaceMissionRepository,
        WorkspaceMissionTaskRef,
        SnapshotFacts,
        WorkspaceResultReport,
    ) {
        let m = mission(&input());
        let repo = m.repositories[1].clone();
        let id = TaskId::new();
        let task = WorkspaceMissionTaskRef {
            conversation: ConversationId(format!(
                "jterm:task:v1:{}",
                serde_json::to_string(&("host", "forge", id.to_string())).unwrap()
            )),
            task_id: id,
            run_id: "real-run-reference".into(),
            source_sequence: 7,
        };
        let snapshot = SnapshotFacts {
            task: task.clone(),
            path: repo.path.clone(),
            baseline: repo.baseline.clone(),
            idle: true,
            can_validate: true,
            recipe_visible: true,
            command: "cargo test --locked".into(),
            shell: "/bin/sh".into(),
            status: TaskValidationStatus::Passed,
        };
        let current = WorktreeReviewSnapshot {
            head: repo.candidate.clone().unwrap(),
            branch: "test".into(),
            base_commit: repo.baseline.clone(),
            fingerprint: "captured-fingerprint".into(),
            status: String::new(),
            diff: String::new(),
            diff_truncated: false,
            status_truncated: false,
            untracked_files: 0,
            captured_at_ms: 30,
            attribution: "test fixture only".into(),
        };
        let mut baseline = current.clone();
        baseline.head = repo.baseline.clone();
        let evidence = WorkspaceValidationEvidence {
            fingerprint: current.fingerprint.clone(),
            head: current.head.clone(),
            base_commit: current.base_commit.clone(),
            command: snapshot.command.clone(),
            cwd: repo.path.to_str().unwrap().into(),
            shell: snapshot.shell.clone(),
            started_at_ms: 20,
            completed_at_ms: Some(25),
            unchanged_after_run: true,
            detail: None,
        };
        let report = WorkspaceResultReport {
            conversation: task.conversation.clone(),
            task_id: id,
            run_id: task.run_id.clone(),
            source_sequence: 7,
            baseline: Some(baseline),
            current: Some(current),
            validation: Some(evidence),
            validation_current: true,
            validation_status: "Not trusted as status".into(),
            validation_output: String::new(),
            validation_output_truncated: false,
            error: None,
        };
        (m, repo, task, snapshot, report)
    }
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("mvp-{}-{}", std::process::id(), TaskId::new()));
        fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn valid_input_and_plan() {
        let i = input();
        validate_input(&i).unwrap();
        check_plan(&i, &mission(&i)).unwrap();
    }
    #[test]
    fn unfrozen_and_wrong_core_rejected() {
        let mut i = input();
        i.frozen = false;
        assert!(validate_input(&i).is_err());
        i.frozen = true;
        i.core = "c".repeat(40);
        assert!(validate_input(&i).is_err());
    }
    #[test]
    fn identity_duplicates_and_other_repositories_rejected() {
        let mut i = input();
        i.repositories[1] = i.repositories[0].clone();
        assert!(validate_input(&i).is_err());
        i = input();
        i.repositories[1].full_name = "other/anvil".into();
        assert!(validate_input(&i).is_err());
    }
    #[test]
    fn short_uppercase_and_nonabsolute_paths_rejected() {
        let mut i = input();
        i.repositories[1].revision = "ABC".into();
        assert!(validate_input(&i).is_err());
        i = input();
        i.repositories[1].path = "relative".into();
        assert!(validate_input(&i).is_err());
    }
    #[test]
    fn plan_path_pin_revision_and_environment_are_bound() {
        let i = input();
        let m = mission(&i);
        for which in 0..4 {
            let mut changed = m.clone();
            match which {
                0 => changed.revision += 1,
                1 => changed.repositories[1].path = "/tmp/other".into(),
                2 => changed.repositories[1].required_core_pin = None,
                _ => changed.required_environment = "CI".into(),
            };
            assert!(check_plan(&i, &changed).is_err());
        }
    }
    #[test]
    fn only_three_read_commands_allowed() {
        let id = ConversationId("test".into());
        assert!(allowed(&WorkspaceCommand::ReadMission {
            id: "mission:x".into()
        }));
        assert!(allowed(&WorkspaceCommand::InspectNative { id: id.clone() }));
        assert!(allowed(&WorkspaceCommand::InspectResult { id }));
        assert!(!allowed(&WorkspaceCommand::MissionList));
        assert!(!allowed(&WorkspaceCommand::InspectMission {
            id: "mission:x".into()
        }));
    }
    #[test]
    fn expired_budget_fails_before_send() {
        let client = WorkspaceClient::for_state_dir("/nonexistent/preflight-must-not-create");
        assert!(request(
            &client,
            WorkspaceCommand::ReadMission {
                id: "mission:x".into()
            },
            Instant::now()
        )
        .is_err());
        assert!(!Path::new("/nonexistent/preflight-must-not-create").exists());
    }
    #[test]
    fn missing_state_is_not_created() {
        let p = temp();
        let missing = p.join("missing");
        assert!(existing_host(&missing).is_err());
        assert!(!missing.exists());
        fs::remove_dir(p).unwrap();
    }
    #[test]
    fn empty_state_does_not_create_runtime() {
        let p = temp();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(existing_host(&p).is_err());
        assert!(fs::read_dir(&p).unwrap().next().is_none());
        fs::remove_dir(p).unwrap();
    }
    #[test]
    fn input_symlink_directory_and_fifo_rejected() {
        let p = temp();
        let link = p.join("link");
        symlink("missing", &link).unwrap();
        assert!(read_input(&link).is_err());
        assert!(read_input(&p).is_err());
        let fifo = p.join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_input(&fifo).is_err());
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn input_budget_and_unknown_fields_rejected() {
        let p = temp();
        let f = p.join("input");
        fs::write(&f, vec![b' '; INPUT_BYTES + 1]).unwrap();
        assert!(read_input(&f).is_err());
        fs::write(&f, b"{\"schema\":1,\"surprise\":true}").unwrap();
        assert!(read_input(&f).is_err());
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn exact_existing_receipt_observed() {
        let (m, r, t, s, report) = fixture();
        assert!(assess_result(&m, &r, &t, &s, &s, &report)
            .unwrap()
            .is_some());
    }
    #[test]
    fn changed_sequence_run_or_recipe_rejected() {
        let (m, r, t, s, report) = fixture();
        for which in 0..3 {
            let mut after = s.clone();
            match which {
                0 => after.task.source_sequence += 1,
                1 => after.task.run_id = "other".into(),
                _ => after.command = "other".into(),
            };
            assert!(assess_result(&m, &r, &t, &s, &after, &report).is_err());
        }
    }
    #[test]
    fn wrong_result_conversation_or_sequence_rejected() {
        let (m, r, t, s, report) = fixture();
        let mut bad = report.clone();
        bad.conversation = ConversationId("other".into());
        assert!(assess_result(&m, &r, &t, &s, &s, &bad).is_err());
        bad = report;
        bad.source_sequence += 1;
        assert!(assess_result(&m, &r, &t, &s, &s, &bad).is_err());
    }
    #[test]
    fn dirty_changed_head_and_baseline_rejected() {
        let (m, r, t, s, report) = fixture();
        for which in 0..3 {
            let mut bad = report.clone();
            let c = bad.current.as_mut().unwrap();
            match which {
                0 => c.status = " M file".into(),
                1 => c.head = "f".repeat(40),
                _ => c.base_commit = "f".repeat(40),
            };
            assert!(assess_result(&m, &r, &t, &s, &s, &bad).is_err());
        }
    }
    #[test]
    fn busy_redacted_or_unavailable_recipe_rejected() {
        let (m, r, t, s, report) = fixture();
        for which in 0..3 {
            let mut bad = s.clone();
            match which {
                0 => bad.idle = false,
                1 => bad.recipe_visible = false,
                _ => bad.command.clear(),
            };
            assert!(assess_result(&m, &r, &t, &bad, &bad, &report).is_err());
        }
    }
    #[test]
    fn stale_failed_or_missing_validation_needs_action() {
        let (m, r, t, s, report) = fixture();
        let mut bad = report.clone();
        bad.validation = None;
        assert_eq!(assess_result(&m, &r, &t, &s, &s, &bad).unwrap(), None);
        bad = report.clone();
        bad.validation.as_mut().unwrap().started_at_ms = m.evidence_after_ms;
        assert_eq!(assess_result(&m, &r, &t, &s, &s, &bad).unwrap(), None);
        let mut failed = s;
        failed.status = TaskValidationStatus::Failed;
        assert_eq!(
            assess_result(&m, &r, &t, &failed, &failed, &report).unwrap(),
            None
        );
    }
    #[test]
    fn old_worktree_receipt_and_other_cwd_rejected() {
        let (m, r, t, s, report) = fixture();
        for which in 0..3 {
            let mut bad = report.clone();
            match which {
                0 => bad.validation_current = false,
                1 => bad.validation.as_mut().unwrap().fingerprint = "other".into(),
                _ => bad.validation.as_mut().unwrap().cwd = "/other".into(),
            };
            assert!(assess_result(&m, &r, &t, &s, &s, &bad).is_err());
        }
    }
    #[test]
    fn ui_status_cannot_promote_receipt() {
        let (m, r, t, mut s, mut report) = fixture();
        s.status = TaskValidationStatus::NotRun;
        report.validation_status = "Passed".into();
        assert_eq!(assess_result(&m, &r, &t, &s, &s, &report).unwrap(), None);
    }
    #[test]
    fn truncated_status_and_untracked_files_rejected() {
        let (m, r, t, s, report) = fixture();
        let mut bad = report.clone();
        bad.current.as_mut().unwrap().status_truncated = true;
        assert!(assess_result(&m, &r, &t, &s, &s, &bad).is_err());
        bad = report;
        bad.current.as_mut().unwrap().untracked_files = 1;
        assert!(assess_result(&m, &r, &t, &s, &s, &bad).is_err());
    }
    #[test]
    fn same_revision_changed_plan_is_not_equal() {
        let i = input();
        let m = mission(&i);
        let mut changed = m.clone();
        changed.goal.push_str(" changed");
        assert_eq!(changed.revision, m.revision);
        assert_ne!(changed, m);
    }
    // Mirrors the production completed-validation snapshot shape. The retained
    // terminal descriptor identifies history and is not a running-process flag.
    #[test]
    fn completed_native_snapshot_retains_descriptor_without_being_busy() {
        let (m, r, t, expected, report) = fixture();
        let mut native: NativeSnapshot = serde_json::from_value(json!({
            "id":t.conversation,"namespace":"forge","incarnation":t.run_id,"sequence":t.source_sequence,
            "task":{"id":t.task_id,"title":"synthetic regression only","provider":"codex",
                "status":"ready_for_review","repo_root":r.path,"worktree_path":r.path,
                "branch":"test","base_commit":r.baseline,"created_at_ms":1,"updated_at_ms":25,
                "validation":{"status":"passed"},"brief":{"goal":"fixture", "validation":{
                    "command":expected.command,"shell":expected.shell}}},
            "preparing":false,"live":false,"ownership_locked":false,"interrupted":false,
            "can_prompt":false,"can_resume":false,"can_start_validation":true,
            "can_continue_in_terminal":false,"events":[],"provider_home":"/tmp/provider",
            "source_profile":"/tmp/profile","handoff_pending":false,"operations":[],
            "validation":{"run_id":"validation-1","preparing":false,"output":"",
                "output_truncated":false,"evidence":report.validation,
                "terminal":{"id":"jterm:validation:fixture","epoch":"fixture-epoch",
                    "cwd":r.path,"label":"Validation"}}
        })).unwrap();
        assert!(native.validation.as_ref().unwrap().terminal.is_some());
        let facts = SnapshotFacts::from_native(&native);
        assert!(facts.idle);
        assert!(assess_result(&m, &r, &t, &facts, &facts, &report)
            .unwrap()
            .is_some());
        native.task.validation.status = TaskValidationStatus::Running;
        assert!(!SnapshotFacts::from_native(&native).idle);
        native.task.validation.status = TaskValidationStatus::Passed;
        native.ownership_locked = true;
        assert!(!SnapshotFacts::from_native(&native).idle);
    }
    #[test]
    fn accepted_task_receipt_can_be_observed_but_unavailable_action_is_not_offered() {
        let (m, r, t, mut s, mut report) = fixture();
        s.can_validate = false;
        assert!(assess_result(&m, &r, &t, &s, &s, &report)
            .unwrap()
            .is_some());
        report.validation = None;
        assert!(assess_result(&m, &r, &t, &s, &s, &report).is_err());
    }
}
