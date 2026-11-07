//! Controlled native-reader setup/check. This never starts a host or provider.
use jterm_core::agent_workspace::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CORE: &str = "e8bd7a2b3edd48d7a0f8f0304fa3d8c864dab40e";
const LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Host {
    pid: u32,
    start_ticks: String,
    executable: PathBuf,
    state_dir: PathBuf,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    core: String,
    host: Host,
    mission: WorkspaceMission,
    report: WorkspaceMissionReport,
    scope: Value,
}
#[derive(Deserialize)]
struct Repo {
    name: String,
    full_name: String,
    revision: String,
    path: PathBuf,
}
fn require(ok: bool, error: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(error.into())
    }
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let m = f.metadata().map_err(|e| e.to_string())?;
    require(
        m.is_file() && m.len() <= LIMIT,
        "bounded_regular_json_required",
    )?;
    let mut data = Vec::new();
    f.take(LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    require(data.len() as u64 <= LIMIT, "json_exceeds_budget")?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    require(bytes.len() as u64 <= LIMIT, "receipt_exceeds_budget")?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(&bytes).map_err(|e| e.to_string())
}
fn canonical(path: &Path) -> bool {
    path.is_absolute() && fs::canonicalize(path).is_ok_and(|p| p == path)
}
fn start_ticks(stat: &str) -> Result<String, String> {
    let (_, rest) = stat.rsplit_once(") ").ok_or("invalid_process_stat")?;
    let fields: Vec<_> = rest.split_whitespace().collect();
    let ticks = fields.get(19).ok_or("missing_process_birth")?;
    require(
        !ticks.is_empty() && ticks.bytes().all(|b| b.is_ascii_digit()),
        "invalid_process_birth",
    )?;
    require(
        !matches!(fields.first(), Some(&"Z") | Some(&"X")),
        "host_not_live",
    )?;
    Ok((*ticks).into())
}
fn inspect_host(pid: u32, executable: &Path, state: &Path) -> Result<Host, String> {
    require(
        pid > 1 && canonical(executable) && canonical(state) && state.as_os_str().len() < 70,
        "canonical_short_state_and_host_required",
    )?;
    for dir in [
        state.to_path_buf(),
        state.join("run"),
        state.join("run/leases"),
    ] {
        let m = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        require(
            m.is_dir()
                && !m.file_type().is_symlink()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o077 == 0,
            "private_runtime_required",
        )?;
    }
    let socket = fs::symlink_metadata(state.join("run/host.sock")).map_err(|e| e.to_string())?;
    require(
        socket.file_type().is_socket()
            && socket.uid() == unsafe { libc::geteuid() }
            && socket.mode() & 0o077 == 0,
        "private_host_socket_required",
    )?;
    let proc = PathBuf::from(format!("/proc/{pid}"));
    require(
        fs::metadata(&proc).is_ok_and(|m| m.uid() == unsafe { libc::geteuid() }),
        "host_owner_changed",
    )?;
    require(
        fs::read_link(proc.join("exe")).is_ok_and(|p| p == executable),
        "host_executable_changed",
    )?;
    let cmdline = fs::read(proc.join("cmdline")).map_err(|e| e.to_string())?;
    let args: Vec<_> = cmdline
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    require(
        args.len() == 2
            && args[0] == executable.as_os_str().as_encoded_bytes()
            && args[1] == b"--jterm-session-host",
        "independent_host_argv_required",
    )?;
    Ok(Host {
        pid,
        start_ticks: start_ticks(
            &fs::read_to_string(proc.join("stat")).map_err(|e| e.to_string())?,
        )?,
        executable: executable.into(),
        state_dir: state.into(),
    })
}
fn alive(host: &Host) -> Result<(), String> {
    require(
        inspect_host(host.pid, &host.executable, &host.state_dir)? == *host,
        "independent_host_identity_changed",
    )
}
fn allowed(command: &WorkspaceCommand) -> bool {
    matches!(
        command,
        WorkspaceCommand::MissionList
            | WorkspaceCommand::ReadMission { .. }
            | WorkspaceCommand::NativeCatalog { .. }
            | WorkspaceCommand::CatalogPage { .. }
    )
}
fn request(command: WorkspaceCommand, host: &Host) -> Result<WorkspaceReply, String> {
    require(allowed(&command), "check_command_not_read_only")?;
    exchange(command, host)
}
#[derive(Deserialize)]
enum Response {
    Reply(Box<Result<WorkspaceReply, String>>),
    Error(String),
}
fn check_peer(stream: &UnixStream, host: &Host) -> Result<(), String> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    require(
        result == 0
            && length as usize == std::mem::size_of::<libc::ucred>()
            && credentials.pid == host.pid as libc::pid_t
            && credentials.uid == unsafe { libc::geteuid() },
        "socket_peer_is_not_recorded_host",
    )?;
    alive(host)
}
fn ready(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> Result<(), String> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        require(!left.is_zero(), "request_deadline")?;
        let millis = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, millis) };
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        require(
            result > 0 && Instant::now() < deadline,
            "request_deadline_or_poll_error",
        )?;
        require(fd.revents & events != 0, "socket_closed_or_poll_failed")?;
        return Ok(());
    }
}
fn connect(path: &Path, deadline: Instant) -> Result<UnixStream, String> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    require(
        !bytes.is_empty() && bytes.len() < address.sun_path.len() && !bytes.contains(&0),
        "invalid_host_socket_path",
    )?;
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (a, b) in address.sun_path.iter_mut().zip(bytes) {
        *a = *b as libc::c_char;
    }
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    require(fd >= 0, "socket_creation_failed")?;
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result != 0 {
        require(
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EINPROGRESS),
            "socket_connect_failed_without_retry",
        )?;
        ready(&stream, libc::POLLOUT, deadline)?;
        let mut error: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut libc::c_int).cast(),
                &mut len,
            )
        };
        require(
            rc == 0 && error == 0 && len as usize == std::mem::size_of::<libc::c_int>(),
            "socket_connect_failed",
        )?;
    }
    require(Instant::now() < deadline, "late_socket_connect")?;
    Ok(stream)
}
// Protocol 2 framing only, using core command/reply data. Unlike the normal
// frontend client, this observer binds the actual request stream to the recorded
// host PID. It cannot auto-start a host, retry writes, or execute provider code.
fn exchange(command: WorkspaceCommand, host: &Host) -> Result<WorkspaceReply, String> {
    let timeout = if matches!(command, WorkspaceCommand::InspectMission { .. }) {
        130
    } else {
        15
    };
    let deadline = Instant::now() + Duration::from_secs(timeout);
    alive(host)?;
    let mut stream = connect(&host.state_dir.join("run/host.sock"), deadline)?;
    check_peer(&stream, host)?;
    let mut message = serde_json::to_vec(&json!({"Command":{"version":2,"command":command}}))
        .map_err(|e| e.to_string())?;
    require(message.len() as u64 <= LIMIT, "request_exceeds_budget")?;
    message.push(b'\n');
    let mut written = 0;
    while written < message.len() {
        alive(host)?;
        require(Instant::now() < deadline, "request_deadline")?;
        match stream.write(&message[written..]) {
            Ok(0) => return Err("host_closed_during_write".into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ready(&stream, libc::POLLOUT, deadline)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.to_string()),
        }
    }
    let mut bytes = Vec::new();
    loop {
        alive(host)?;
        let left = deadline.saturating_duration_since(Instant::now());
        require(!left.is_zero(), "request_deadline")?;
        let mut buffer = [0_u8; 8192];
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ready(&stream, libc::POLLIN, deadline)?;
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        require(
            count > 0 && bytes.len() + count <= LIMIT as usize,
            "missing_or_oversized_response",
        )?;
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.iter().position(|b| *b == b'\n') {
            require(end + 1 == bytes.len(), "unexpected_extra_response_frame")?;
            break;
        }
    }
    require(Instant::now() < deadline, "late_response")?;
    check_peer(&stream, host)?;
    match serde_json::from_slice::<Response>(&bytes).map_err(|e| e.to_string())? {
        Response::Reply(result) => *result,
        Response::Error(error) => Err(error),
    }
}
fn repositories(root: &Path) -> Result<Vec<Repo>, String> {
    require(canonical(root), "canonical_acceptance_root_required")?;
    let input: Value = read_json(&root.join("observed-heads.json"))?;
    let repos: Vec<Repo> =
        serde_json::from_value(input["repositories"].clone()).map_err(|e| e.to_string())?;
    validate_repos(root, &repos)?;
    for repo in &repos {
        require(canonical(&repo.path), "canonical_checkout_required")?;
    }
    Ok(repos)
}
fn validate_repos(root: &Path, repos: &[Repo]) -> Result<(), String> {
    require(repos.len() == 5, "five_exact_repositories_required")?;
    let mut names = BTreeSet::new();
    for repo in repos {
        require(
            FOUR_TERMINAL_REPOSITORIES
                .iter()
                .any(|(n, _)| *n == repo.full_name)
                && names.insert(&repo.full_name),
            "unknown_or_duplicate_repository",
        )?;
        require(
            repo.full_name == format!("beamiter/{}", repo.name)
                && repo.path == root.join("repositories").join(&repo.name),
            "checkout_path_not_in_verified_set",
        )?;
        require(
            repo.revision.len() == 40
                && repo
                    .revision
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "full_lowercase_sha_required",
        )?;
        if repo.name == "jterm_core" {
            require(repo.revision == CORE, "compiled_core_mismatch")?;
        }
    }
    Ok(())
}
fn plan_matches(plan: &WorkspaceMission, repos: &[Repo]) -> Result<(), String> {
    plan.validate()?;
    require(
        plan.revision > 0
            && plan.evidence_after_ms > 0
            && plan.required_environment == "local"
            && plan.preset == WorkspaceMissionPreset::FourTerminals
            && plan.repositories.len() == 5,
        "exact_saved_five_repository_plan_required",
    )?;
    for r in &plan.repositories {
        let expected = repos
            .iter()
            .find(|p| p.full_name == r.identity.full_name)
            .ok_or("unexpected_saved_repository")?;
        require(
            r.path == expected.path
                && r.baseline == expected.revision
                && r.candidate.as_deref() == Some(&expected.revision)
                && r.identity
                    == WorkspaceMissionRepositoryIdentity::canonical(&expected.full_name)?,
            "candidate_or_baseline_changed",
        )?;
        require(
            r.target_ref == "refs/heads/master"
                && r.tasks.is_empty()
                && r.required_checks == ["validation"],
            "task_or_acceptance_contract_changed",
        )?;
        require(
            r.required_core_pin.as_deref()
                == if expected.name == "jterm_core" {
                    None
                } else {
                    Some(CORE)
                },
            "required_core_pin_changed",
        )?;
    }
    Ok(())
}
fn missing_validation(
    plan: &WorkspaceMission,
    report: &WorkspaceMissionReport,
) -> Result<(), String> {
    require(
        report.id == plan.id
            && report.revision == plan.revision
            && report.state != WorkspaceMissionState::Completed
            && report.needs_you()
            && report.repositories.len() == 5,
        "report_identity_or_completion_invalid",
    )?;
    let mut names = BTreeSet::new();
    for repo in &report.repositories {
        let expected = plan
            .repositories
            .iter()
            .find(|r| r.identity == repo.identity)
            .ok_or("report_repository_mismatch")?;
        require(
            names.insert(&repo.identity.full_name)
                && repo.path == expected.path
                && repo.candidate == expected.candidate
                && !repo.validated
                && repo.covered_checks.is_empty()
                && repo.missing_checks == ["validation"],
            "unexpected_validation_authority",
        )?;
        require(
            repo.checks.len() == 1
                && repo.checks[0].check_name == "validation"
                && repo.checks[0].environment == "local"
                && repo.checks[0].status == WorkspaceMissionCheckStatus::NotRun
                && repo.checks[0].receipt_id.is_none(),
            "validation_was_run_or_fabricated",
        )?;
    }
    Ok(())
}
fn scope() -> Value {
    json!({"provider_execution":"skipped_by_user","full_delivery":"not_claimed","setup":"production_Mission_API_not_native_UI","native_four_frontend_combination":"not_run"})
}
fn verified_setup(plan: &WorkspaceMission, report: &WorkspaceMissionReport) -> Result<(), String> {
    missing_validation(plan, report)?;
    require(
        report.state == WorkspaceMissionState::InProgress && report.errors.is_empty(),
        "setup_not_incomplete_in_progress",
    )?;
    for r in &report.repositories {
        require(
            r.committed && r.pushed && r.integrated && r.pins_current && r.errors.is_empty(),
            "setup_source_gates_not_verified",
        )?;
        let p = plan
            .repositories
            .iter()
            .find(|p| p.identity == r.identity)
            .ok_or("setup_identity_mismatch")?;
        let local = r
            .observed
            .as_ref()
            .ok_or("setup_local_observation_missing")?;
        require(
            Some(local.head.as_str()) == p.candidate.as_deref()
                && local.baseline == p.baseline
                && !local.dirty
                && !local.fingerprint.is_empty()
                && local.observed_at_ms >= plan.evidence_after_ms,
            "setup_local_observation_invalid",
        )?;
        let remote = r
            .pushed_observation
            .as_ref()
            .ok_or("setup_remote_observation_missing")?;
        require(
            remote.repository == p.identity
                && remote.ref_name == p.target_ref
                && Some(remote.head.as_str()) == p.candidate.as_deref()
                && remote.observed_at_ms >= plan.evidence_after_ms,
            "setup_remote_observation_invalid",
        )?;
        let integration = r
            .integration_observation
            .as_ref()
            .ok_or("setup_integration_observation_missing")?;
        require(
            integration.remote == *remote
                && integration.contains_candidate
                && Some(integration.candidate.as_str()) == p.candidate.as_deref(),
            "setup_integration_observation_invalid",
        )?;
    }
    Ok(())
}
fn normalize_time(actual: &mut u64, expected: u64) -> Result<(), String> {
    require(
        *actual >= expected && expected > 0,
        "observation_time_regressed",
    )?;
    *actual = expected;
    Ok(())
}
fn retained_observations(
    seed: &WorkspaceMissionReport,
    current: &WorkspaceMissionReport,
) -> Result<(), String> {
    let mut expected = seed.clone();
    expected.require_reinspection();
    let mut actual = current.clone();
    require(
        actual.repositories.len() == expected.repositories.len(),
        "report_repository_count_changed",
    )?;
    // Explicit native Inspect may recapture the same immutable facts at a later
    // time. Only those monotonic observation timestamps may differ; no head,
    // fingerprint, pin, remote, receipt, check, error or stale gate may change.
    for (a, e) in actual.repositories.iter_mut().zip(&expected.repositories) {
        let local = a.observed.as_mut().ok_or("local_observation_disappeared")?;
        normalize_time(
            &mut local.observed_at_ms,
            e.observed
                .as_ref()
                .ok_or("seed_local_missing")?
                .observed_at_ms,
        )?;
        let remote = a
            .pushed_observation
            .as_mut()
            .ok_or("remote_observation_disappeared")?;
        normalize_time(
            &mut remote.observed_at_ms,
            e.pushed_observation
                .as_ref()
                .ok_or("seed_remote_missing")?
                .observed_at_ms,
        )?;
        let integration = a
            .integration_observation
            .as_mut()
            .ok_or("integration_observation_disappeared")?;
        normalize_time(
            &mut integration.remote.observed_at_ms,
            e.integration_observation
                .as_ref()
                .ok_or("seed_integration_missing")?
                .remote
                .observed_at_ms,
        )?;
    }
    require(
        actual == expected,
        "retained_observation_or_stale_gate_changed",
    )
}
fn empty_conversations(reply: WorkspaceReply) -> Result<(), String> {
    match reply {
        WorkspaceReply::CatalogPage {
            rows,
            next_before,
            total,
        } if rows.is_empty() && next_before.is_none() && total == 0 => Ok(()),
        WorkspaceReply::CatalogPage {
            rows,
            next_before,
            total,
        } => {
            // Keep the hard empty-catalog gate. Only retain shape facts from
            // the already-requested first row, never text, IDs, paths or store
            // contents. This diagnoses a future failure without exempting it.
            let shape = rows.first().map(|row| {
                json!({
                    "kind": row.kind,
                    "runtime": row.runtime,
                    "title_is_default": row.title == "New chat",
                    "provider_is_api": row.provider == "API",
                    "cwd_empty": row.cwd.is_empty(),
                    "preview_empty": row.preview.is_empty(),
                    "tags_count": row.tags.len(),
                    "archived": row.archived,
                    "pinned": row.pinned
                })
            });
            Err(format!(
                "conversation_catalog_not_empty: {}",
                json!({"total": total, "returned_rows": rows.len(),
                    "next_before_present": next_before.is_some(), "first_row_shape": shape})
            ))
        }
        _ => Err("unexpected_conversation_catalog_reply".into()),
    }
}
fn no_runtime(host: &Host) -> Result<(), String> {
    match request(
        WorkspaceCommand::NativeCatalog {
            after: None,
            limit: 1,
        },
        host,
    )? {
        WorkspaceReply::NativeCatalog {
            entries,
            next_after,
        } if entries.is_empty() && next_after.is_none() => (),
        _ => return Err("native_runtime_catalog_not_empty".into()),
    }
    empty_conversations(request(
        WorkspaceCommand::CatalogPage {
            before: None,
            limit: 1,
        },
        host,
    )?)
}
fn seed(root: &Path, host: Host, out: &Path) -> Result<(), String> {
    require(!out.exists(), "seed_receipt_already_exists")?;
    let repos = repositories(root)?;
    no_runtime(&host)?;
    match request(WorkspaceCommand::MissionList, &host)? {
        WorkspaceReply::Missions { missions } if missions.is_empty() => (),
        _ => return Err("seed_requires_empty_isolated_mission_catalog".into()),
    }
    // Declared acceptance targets are not evidence. The production inspector
    // below must independently verify every exact candidate and public ref.
    let mut proposed = WorkspaceMission::four_terminals(
        "Shared native Mission",
        "Read the same five exact candidates in four native apps; validation and provider execution remain unverified",
        root.join("repositories"),
    );
    for repository in &mut proposed.repositories {
        let expected = repos
            .iter()
            .find(|r| r.full_name == repository.identity.full_name)
            .ok_or("missing_candidate")?;
        repository.baseline = expected.revision.clone();
        repository.candidate = Some(expected.revision.clone());
        if expected.name != "jterm_core" {
            repository.required_core_pin = Some(CORE.into());
        }
    }
    proposed.validate()?;
    let mission = match exchange(
        WorkspaceCommand::SaveMission {
            expected_revision: 0,
            mission: proposed,
        },
        &host,
    )? {
        WorkspaceReply::Mission {
            mission,
            report: None,
        } if mission.revision == 1 => mission,
        _ => return Err("unexpected_initial_save_reply".into()),
    };
    let report = match exchange(
        WorkspaceCommand::InspectMission {
            id: mission.id.clone(),
        },
        &host,
    )? {
        WorkspaceReply::Mission {
            mission: actual,
            report: Some(report),
        } if actual == mission => report,
        _ => return Err("setup_inspection_changed_plan".into()),
    };
    plan_matches(&mission, &repos)?;
    verified_setup(&mission, &report)?;
    no_runtime(&host)?;
    write_json(
        out,
        &Receipt {
            schema: 1,
            core: CORE.into(),
            host,
            mission,
            report,
            scope: scope(),
        },
    )
}
fn check(root: &Path, host: &Host, receipt: &Path, out: &Path) -> Result<(), String> {
    let saved: Receipt = read_json(receipt)?;
    require(
        saved.schema == 1 && saved.core == CORE && saved.host == *host,
        "seed_receipt_identity_mismatch",
    )?;
    let repos = repositories(root)?;
    plan_matches(&saved.mission, &repos)?;
    require(saved.scope == scope(), "seed_scope_changed")?;
    verified_setup(&saved.mission, &saved.report)?;
    no_runtime(host)?;
    match request(WorkspaceCommand::MissionList, host)? {
        WorkspaceReply::Missions { missions } if missions == [saved.mission.clone()] => (),
        _ => return Err("mission_catalog_changed".into()),
    }
    let report = match request(
        WorkspaceCommand::ReadMission {
            id: saved.mission.id.clone(),
        },
        host,
    )? {
        WorkspaceReply::Mission {
            mission,
            report: Some(report),
        } if mission == saved.mission => report,
        _ => return Err("full_saved_plan_changed".into()),
    };
    missing_validation(&saved.mission, &report)?;
    retained_observations(&saved.report, &report)?;
    no_runtime(host)?;
    alive(host)?;
    write_json(
        out,
        &json!({"schema":1,"status":"passed","scope":"host_state_check_only_not_native_UI","core":CORE,"host":host,"mission":saved.mission,"report":report,"full_plan_unchanged":true,"native_catalog_empty":true,"conversation_catalog_empty":true,"provider_execution":"skipped_by_user","hosted_task_validation":"not_run","five_repository_delivery":"not_completed"}),
    )
}
fn record_check_failure(out: &Path, error: String) -> Result<(), String> {
    // Preserve the rejected invariant instead of leaving the final gate with
    // only a missing-file error. This is a failure record, never a substitute
    // for a successful state check, and cannot overwrite an earlier receipt.
    let failure = json!({
        "schema": 1,
        "status": "failed",
        "scope": "host_state_check_only_not_native_UI",
        "core": CORE,
        "error": error,
        "provider_execution": "skipped_by_user",
        "hosted_task_validation": "not_run",
        "five_repository_delivery": "not_completed"
    });
    if let Err(write_error) = write_json(out, &failure) {
        return Err(format!(
            "{error}; failure_receipt_not_written: {write_error}"
        ));
    }
    Err(error)
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    require(args.len() == 7 || args.len() == 8, "usage: seed ROOT STATE PID HOST_BIN RECEIPT | check ROOT STATE PID HOST_BIN RECEIPT OUTPUT")?;
    let pid = args[4].parse().map_err(|_| "invalid_host_pid")?;
    let host = inspect_host(pid, Path::new(&args[5]), Path::new(&args[3]))?;
    match (args[1].as_str(), args.len()) {
        ("seed", 7) => seed(Path::new(&args[2]), host, Path::new(&args[6])),
        ("check", 8) => check(
            Path::new(&args[2]),
            &host,
            Path::new(&args[6]),
            Path::new(&args[7]),
        )
        .or_else(|error| record_check_failure(Path::new(&args[7]), error)),
        _ => Err("unsupported_operation".into()),
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("mission-native-state: {error}");
        std::process::exit(2);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn even_an_empty_api_placeholder_is_rejected_with_redacted_shape_only() {
        let row = WorkspaceRow {
            id: ConversationId("must-not-archive-this-id".into()),
            kind: ConversationKind::Api,
            title: "New chat".into(),
            provider: "API".into(),
            cwd: String::new(),
            preview: String::new(),
            updated_at_ms: 0,
            archived: false,
            pinned: false,
            tags: vec![],
            runtime: RuntimeState::Idle,
            facts: WorkspaceFacts::default(),
        };
        let error = empty_conversations(WorkspaceReply::CatalogPage {
            rows: vec![row],
            next_before: None,
            total: 1,
        })
        .unwrap_err();
        let detail: Value = serde_json::from_str(
            error
                .strip_prefix("conversation_catalog_not_empty: ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(detail["total"], 1);
        assert_eq!(detail["first_row_shape"]["kind"], "Api");
        assert_eq!(detail["first_row_shape"]["title_is_default"], true);
        assert!(!error.contains("must-not-archive-this-id"));
    }
    #[test]
    fn empty_catalog_requires_zero_total_and_no_cursor() {
        for (total, next_before) in [(0, None), (1, None), (0, Some(1))] {
            let result = empty_conversations(WorkspaceReply::CatalogPage {
                rows: vec![],
                next_before,
                total,
            });
            assert_eq!(result.is_ok(), total == 0 && next_before.is_none());
        }
    }
    #[test]
    fn failed_check_records_original_reason_without_claiming_success_or_overwriting() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "native-check-failure-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("failure.json");
        let original = "conversation_catalog_not_empty";
        assert_eq!(
            record_check_failure(&path, original.into()).unwrap_err(),
            original
        );
        let bytes = fs::read(&path).unwrap();
        let receipt: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt["status"], "failed");
        assert_eq!(receipt["error"], original);
        assert_eq!(receipt["scope"], "host_state_check_only_not_native_UI");
        assert_eq!(receipt["hosted_task_validation"], "not_run");
        assert_eq!(receipt["five_repository_delivery"], "not_completed");
        assert!(receipt.get("full_plan_unchanged").is_none());
        assert!(receipt.get("conversation_catalog_empty").is_none());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let second = record_check_failure(&path, "another_failure".into()).unwrap_err();
        assert!(second.starts_with("another_failure; failure_receipt_not_written:"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
    fn repos() -> Vec<Repo> {
        FOUR_TERMINAL_REPOSITORIES
            .iter()
            .map(|(name, _)| Repo {
                name: name.split('/').nth(1).unwrap().into(),
                full_name: (*name).into(),
                revision: CORE.into(),
                path: PathBuf::from("/tmp/root/repositories").join(name.split('/').nth(1).unwrap()),
            })
            .collect()
    }
    fn plan() -> WorkspaceMission {
        let mut m =
            WorkspaceMission::four_terminals("Test", "Do not execute", "/tmp/root/repositories");
        m.id = "mission:native-test".into();
        m.revision = 2;
        m.evidence_after_ms = 1;
        for r in &mut m.repositories {
            r.baseline = CORE.into();
            r.candidate = Some(CORE.into());
            if r.identity.full_name != "beamiter/jterm_core" {
                r.required_core_pin = Some(CORE.into());
            }
        }
        m
    }
    fn report() -> WorkspaceMissionReport {
        let p = plan();
        let observations = p
            .repositories
            .iter()
            .map(|r| {
                let remote = WorkspaceMissionRemoteEvidence {
                    repository: r.identity.clone(),
                    ref_name: r.target_ref.clone(),
                    head: CORE.into(),
                    observed_at_ms: 2,
                };
                WorkspaceMissionRepositoryEvidence {
                    identity: r.identity.clone(),
                    path: r.path.clone(),
                    local: Some(WorkspaceMissionLocalEvidence {
                        baseline: CORE.into(),
                        head: CORE.into(),
                        fingerprint: "test-only-fingerprint".into(),
                        modified: false,
                        dirty: false,
                        manifest_core_pin: r.required_core_pin.clone(),
                        lock_core_pin: r.required_core_pin.clone(),
                        observed_at_ms: 2,
                    }),
                    pushed: Some(remote.clone()),
                    integrated: Some(WorkspaceMissionIntegrationEvidence {
                        remote,
                        candidate: CORE.into(),
                        contains_candidate: true,
                    }),
                    validations: vec![],
                    error: None,
                }
            })
            .collect::<Vec<_>>();
        p.project(&observations)
    }
    #[test]
    fn genuine_shape_missing_validation_is_accepted() {
        assert!(verified_setup(&plan(), &report()).is_ok());
    }
    #[test]
    fn only_monotonic_recapture_times_may_change() {
        let seed = report();
        let mut current = seed.clone();
        current.require_reinspection();
        assert!(retained_observations(&seed, &current).is_ok());
        current.repositories[0]
            .observed
            .as_mut()
            .unwrap()
            .observed_at_ms = 3;
        assert!(retained_observations(&seed, &current).is_ok());
        current.repositories[0]
            .observed
            .as_mut()
            .unwrap()
            .observed_at_ms = 1;
        assert!(retained_observations(&seed, &current).is_err());
    }
    #[test]
    fn changed_or_removed_remote_facts_are_rejected() {
        let seed = report();
        let mut current = seed.clone();
        current.require_reinspection();
        current.repositories[0]
            .pushed_observation
            .as_mut()
            .unwrap()
            .head = "a".repeat(40);
        assert!(retained_observations(&seed, &current).is_err());
        current = seed.clone();
        current.require_reinspection();
        current.repositories[0].observed = None;
        assert!(retained_observations(&seed, &current).is_err());
    }
    #[test]
    fn changed_fingerprint_or_restored_authority_is_rejected() {
        let seed = report();
        let mut current = seed.clone();
        current.require_reinspection();
        current.repositories[0]
            .observed
            .as_mut()
            .unwrap()
            .fingerprint = "other".into();
        assert!(retained_observations(&seed, &current).is_err());
        current = seed.clone();
        current.require_reinspection();
        current.repositories[0].committed = true;
        assert!(retained_observations(&seed, &current).is_err());
    }
    #[test]
    fn fabricated_validation_receipt_is_rejected() {
        let mut r = report();
        r.repositories[0].checks[0].receipt_id = Some("fake".into());
        assert!(verified_setup(&plan(), &r).is_err());
        r = report();
        r.repositories[0].checks[0].status = WorkspaceMissionCheckStatus::Passed;
        assert!(verified_setup(&plan(), &r).is_err());
    }
    #[test]
    fn socket_peer_must_match_recorded_host_pid() {
        let (a, _b) = UnixStream::pair().unwrap();
        let host = Host {
            pid: 0,
            start_ticks: "0".into(),
            executable: "/nonexistent".into(),
            state_dir: "/nonexistent".into(),
        };
        assert_eq!(
            check_peer(&a, &host).unwrap_err(),
            "socket_peer_is_not_recorded_host"
        );
    }
    #[test]
    fn nonblocking_wait_is_bounded_without_data() {
        let (a, _b) = UnixStream::pair().unwrap();
        let start = Instant::now();
        assert!(ready(&a, libc::POLLIN, start + Duration::from_millis(20)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn socket_path_and_expired_deadline_rejected() {
        assert!(connect(
            Path::new(&format!("/{}", "a".repeat(110))),
            Instant::now() + Duration::from_secs(1)
        )
        .is_err());
        assert!(connect(
            Path::new("/tmp/nonexistent-native-mission-socket"),
            Instant::now()
        )
        .is_err());
    }
    #[test]
    fn exact_plan_is_accepted() {
        assert!(plan_matches(&plan(), &repos()).is_ok());
    }
    #[test]
    fn renamed_repo_is_rejected() {
        let mut r = repos();
        r[1].name = "other".into();
        assert!(validate_repos(Path::new("/tmp/root"), &r).is_err());
    }
    #[test]
    fn duplicate_repo_is_rejected() {
        let mut r = repos();
        r[1].full_name = r[0].full_name.clone();
        assert!(validate_repos(Path::new("/tmp/root"), &r).is_err());
    }
    #[test]
    fn foreign_core_is_rejected() {
        let mut r = repos();
        r[0].revision = "a".repeat(40);
        assert!(validate_repos(Path::new("/tmp/root"), &r).is_err());
    }
    #[test]
    fn changed_candidate_or_path_is_rejected() {
        for path in [false, true] {
            let mut m = plan();
            if path {
                m.repositories[1].path = "/tmp/foreign".into();
            } else {
                m.repositories[1].candidate = Some("a".repeat(40));
            }
            assert!(plan_matches(&m, &repos()).is_err());
        }
    }
    #[test]
    fn removed_check_cannot_claim_no_validation() {
        let mut m = plan();
        m.repositories[0].required_checks.clear();
        assert!(plan_matches(&m, &repos()).is_err());
    }
    #[test]
    fn changed_pin_or_target_is_rejected() {
        let mut m = plan();
        m.repositories[1].required_core_pin = None;
        assert!(plan_matches(&m, &repos()).is_err());
        m = plan();
        m.repositories[0].target_ref = "refs/heads/other".into();
        assert!(plan_matches(&m, &repos()).is_err());
    }
    #[test]
    fn missing_report_rows_or_completed_state_rejected() {
        let p = plan();
        let mut r = p.project(&[]);
        r.repositories.clear();
        assert!(missing_validation(&p, &r).is_err());
        r = p.project(&[]);
        r.state = WorkspaceMissionState::Completed;
        assert!(missing_validation(&p, &r).is_err());
    }
    #[test]
    fn writable_and_inspection_commands_rejected_by_checker() {
        assert!(!allowed(&WorkspaceCommand::InspectMission {
            id: "x".into()
        }));
        assert!(!allowed(&WorkspaceCommand::SaveMission {
            expected_revision: 0,
            mission: plan()
        }));
        assert!(!allowed(&WorkspaceCommand::CreateChat {
            title: "x".into()
        }));
    }
    #[test]
    fn process_stat_uses_last_paren_and_exact_birth() {
        let fields = ["S"]
            .into_iter()
            .chain(std::iter::repeat_n("0", 18))
            .chain(["9876"])
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            start_ticks(&format!("123 (odd ) name) {fields}")).unwrap(),
            "9876"
        );
        assert!(start_ticks(&format!("123 (x) {}", fields.replacen('S', "Z", 1))).is_err());
        assert!(start_ticks("123 (short) S 0").is_err());
    }
}
