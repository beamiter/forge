//! Real public-code host contract, not native UI or provider acceptance.
use jterm_core::agent_workspace::*;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn save(root: &Path, name: &str, value: &impl serde::Serialize) -> Result<(), String> {
    fs::write(
        root.join("evidence").join(name),
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
// WorkspaceReply currently carries string errors. Classify only the exact CAS
// rejection from the pinned core, never a transport error or a substring match.
const PINNED_CORE_CAS_CONFLICT: &str =
    "Mission changed in another view; reload before retrying. Your draft was not applied.";
fn verify_stale_cas(
    write: Result<WorkspaceReply, String>,
    readback: Result<WorkspaceReply, String>,
    expected: &WorkspaceMission,
) -> Result<Value, String> {
    match write {
        Err(error) if error == PINNED_CORE_CAS_CONFLICT => (),
        Err(error) => {
            return Err(format!(
                "Stale write did not return the pinned revision-conflict rejection: {error}"
            ))
        }
        Ok(_) => return Err("Stale write unexpectedly succeeded".into()),
    }
    match readback {
        Ok(WorkspaceReply::Mission {
            mission,
            report: Some(report),
        }) if mission == *expected
            && report.id == expected.id
            && report.revision == expected.revision =>
        {
            Ok(
                json!({"rejection_kind":"pinned_core_revision_conflict", "error":PINNED_CORE_CAS_CONFLICT, "independent_readback_unchanged":true, "mission":mission, "historical_report":report}),
            )
        }
        Ok(_) => Err(
            "Independent client did not read back the unchanged frozen plan and report identity"
                .into(),
        ),
        Err(error) => Err(format!("Independent frozen-plan readback failed: {error}")),
    }
}
fn request(client: &WorkspaceClient, command: WorkspaceCommand) -> Result<WorkspaceReply, String> {
    let id = client.send(command)?;
    let deadline = Instant::now() + Duration::from_secs(135);
    loop {
        for event in client.poll() {
            let WorkspaceEvent::Reply { request_id, result } = event;
            if request_id == id {
                return result;
            }
        }
        if Instant::now() > deadline {
            return Err("Acceptance request deadline".into());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn settle(workbench: &mut MissionWorkbench) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(135);
    while workbench.busy {
        workbench.poll();
        if Instant::now() > deadline {
            return Err("Acceptance workbench deadline".into());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    if let Some(error) = &workbench.notice {
        return Err(error.clone());
    }
    Ok(())
}
fn start(state: &Path, log: &Path) -> Result<Host, String> {
    let mut host = Host(
        Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg("--jterm-session-host")
            .env("JTERM_WORKSPACE_STATE_DIR", state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(File::create(log).map_err(|e| e.to_string())?))
            .spawn()
            .map_err(|e| e.to_string())?,
    );
    let client = WorkspaceClient::for_state_dir(state);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if request(&client, WorkspaceCommand::MissionList).is_ok() {
            return Ok(host);
        }
        if let Some(status) = host.0.try_wait().map_err(|e| e.to_string())? {
            return Err(format!(
                "Host exited {status}: {}",
                fs::read_to_string(log).unwrap_or_default()
            ));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "Host not ready: {}",
                fs::read_to_string(log).unwrap_or_default()
            ));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}
fn inspect(w: &mut MissionWorkbench) -> Result<WorkspaceMissionReport, String> {
    w.inspect();
    settle(w)?;
    w.report.clone().ok_or("Inspect returned no report".into())
}
fn check_no_validation(report: &WorkspaceMissionReport) -> Result<(), String> {
    require(
        report.state != WorkspaceMissionState::Completed,
        "Missing task validation was called Completed",
    )?;
    require(
        report.needs_you(),
        "Missing validation disappeared from Needs-you",
    )?;
    require(
        report.repositories.len() == 5,
        "Report does not contain five repositories",
    )?;
    for r in &report.repositories {
        require(
            !r.validated && !r.complete(),
            "Repository without validation was declared complete",
        )?;
        require(
            r.missing_checks.iter().any(|s| s == "validation"),
            "Required validation was removed",
        )?;
        require(
            r.checks.iter().all(|c| c.receipt_id.is_none()),
            "A validation receipt was fabricated",
        )?;
    }
    Ok(())
}
fn check_historical(report: &WorkspaceMissionReport) -> Result<(), String> {
    check_no_validation(report)?;
    require(
        report
            .repositories
            .iter()
            .all(|r| !r.committed && !r.pushed && !r.integrated && !r.pins_current),
        "Historical read restored current success gates",
    )?;
    require(
        report
            .repositories
            .iter()
            .all(|r| r.integration_observation.is_some()),
        "Dated remote receipts were lost",
    )
}
fn check_positive(report: &WorkspaceMissionReport, input: &Value) -> Result<(), String> {
    check_no_validation(report)?;
    for expected in input["repositories"]
        .as_array()
        .ok_or("Missing repository inputs")?
    {
        let name = expected["full_name"]
            .as_str()
            .ok_or("Missing repository name")?;
        let sha = expected["revision"].as_str().ok_or("Missing candidate")?;
        let r = report
            .repositories
            .iter()
            .find(|r| r.identity.full_name == name)
            .ok_or("Missing repository report")?;
        let local = r.observed.as_ref().ok_or("Actual local evidence missing")?;
        require(
            local.head == sha && !local.dirty && !local.fingerprint.is_empty(),
            "Incorrect or dirty observed candidate",
        )?;
        require(
            r.candidate.as_deref() == Some(sha)
                && r.committed
                && r.pushed
                && r.integrated
                && r.pins_current,
            "Actual candidate/source/publication gate did not pass",
        )?;
        let receipt = r
            .integration_observation
            .as_ref()
            .ok_or("Missing integration receipt")?;
        require(
            receipt.remote.repository.full_name == name
                && receipt.remote.ref_name == "refs/heads/master"
                && receipt.remote.head == sha
                && receipt.candidate == sha
                && receipt.contains_candidate
                && receipt.remote.observed_at_ms > 0,
            "Remote receipt does not identify the exact candidate and ref",
        )?;
    }
    Ok(())
}
fn check_unpublished(
    report: &WorkspaceMissionRepositoryReport,
    frozen_remote: &str,
    candidate: &str,
) -> Result<(), String> {
    const PIN_ERROR: &str = "Manifest and lockfile do not both pin the required core commit";
    const ANCESTRY_ERROR: &str =
        "Integration evidence does not establish candidate ancestry on the target ref";
    let receipt = report
        .integration_observation
        .as_ref()
        .ok_or("Unpublished negative lacks actual remote ancestry observation")?;
    require(
        report.identity.full_name == "beamiter/forge"
            && report.candidate.as_deref() == Some(candidate)
            && report.committed
            && !report.pins_current
            && !report.pushed
            && report.pushed_observation.is_none()
            && !report.integrated
            && receipt.remote.repository == report.identity
            && receipt.remote.ref_name == "refs/heads/master"
            && receipt.remote.head == frozen_remote
            && receipt.remote.observed_at_ms > 0
            && receipt.candidate == candidate
            && candidate != frozen_remote
            && !receipt.contains_candidate,
        "Unpublished negative does not prove absence from the actual frozen public ref",
    )?;
    require(
        report.errors.len() == 2
            && report.errors.iter().any(|e| e == PIN_ERROR)
            && report.errors.iter().any(|e| e == ANCESTRY_ERROR),
        "Unexpected source/remote error contaminated the unpublished negative",
    )
}
fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Mission contract fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Mission contract fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .map_err(|e| e.to_string())?;
    require(
        output.status.success(),
        &format!(
            "Local fixture Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
fn negative_plan(
    w: &mut MissionWorkbench,
    base: &WorkspaceMission,
    path: &Path,
    title: &str,
    candidate: Option<String>,
) -> Result<WorkspaceMissionReport, String> {
    let mut plan = base.clone();
    plan.id = WorkspaceMission::new_id();
    plan.revision = 0;
    plan.title = title.into();
    let forge = plan
        .repositories
        .iter_mut()
        .find(|r| r.identity.full_name == "beamiter/forge")
        .ok_or("Forge not in plan")?;
    forge.path = path.into();
    if candidate.is_some() {
        forge.candidate = candidate;
    }
    w.save(plan);
    settle(w)?;
    inspect(w)
}
fn flow(root: &Path, input: &Value) -> Result<Value, String> {
    let state = root.join("isolated-host-state");
    require(
        !state.exists(),
        "State exists; this contract requires a fresh run directory",
    )?;
    let host = start(&state, &root.join("evidence/host-initial.log"))?;
    let roots: Vec<(String, PathBuf)> = input["repositories"]
        .as_array()
        .ok_or("Missing repositories")?
        .iter()
        .map(|r| {
            Ok((
                r["full_name"].as_str().ok_or("Missing full_name")?.into(),
                PathBuf::from(r["path"].as_str().ok_or("Missing path")?),
            ))
        })
        .collect::<Result<_, String>>()?;
    require(roots.len() == 5, "Five actual checkouts are required")?;
    for (name, path) in &roots {
        require(
            path == &root.join("repositories").join(
                name.strip_prefix("beamiter/")
                    .ok_or("Unauthorized repository")?,
            ),
            "Repository path outside this isolated run",
        )?;
    }
    let mut w = MissionWorkbench::with_client(WorkspaceClient::for_state_dir(&state));
    w.create("Real five-repository host contract", "Exact candidates and publication receipts; no provider or validation receipts are invented", &roots);
    settle(&mut w)?;
    let original = w
        .selected
        .clone()
        .ok_or("Mission creation did not select a plan")?;
    require(original.revision == 1, "Mission was not durably saved")?;
    save(root, "01-created.json", &original)?;
    // These are four independent production protocol/controller clients, NOT
    // an assertion that four rendered application UIs were exercised.
    for label in [
        "anvil-client",
        "forge-client",
        "ember-client",
        "frost-client",
    ] {
        let client = WorkspaceClient::for_state_dir(&state);
        match request(
            &client,
            WorkspaceCommand::ReadMission {
                id: original.id.clone(),
            },
        )? {
            WorkspaceReply::Mission { mission, report } => {
                require(
                    mission == original && report.is_none(),
                    "Independent client saw a different saved Mission",
                )?;
                save(root, &format!("02-{label}.json"), &mission)?;
            }
            _ => return Err("Unexpected shared read reply".into()),
        }
    }
    inspect(&mut w)?;
    require(
        w.can_adopt_candidates(),
        "Fresh real inspection did not offer all five clean candidates",
    )?;
    w.adopt_candidates();
    settle(&mut w)?;
    require(
        !w.can_adopt_candidates(),
        "Candidate save failed to consume inspection authority",
    )?;
    let positive = inspect(&mut w)?;
    check_positive(&positive, input)?;
    save(
        root,
        "03-positive-frozen.json",
        &json!({"mission":w.selected,"report":positive}),
    )?;
    let frozen = w.selected.clone().ok_or("Frozen Mission missing")?;
    for label in [
        "anvil-client",
        "forge-client",
        "ember-client",
        "frost-client",
    ] {
        let mut observer = MissionWorkbench::with_client(WorkspaceClient::for_state_dir(&state));
        observer.refresh();
        settle(&mut observer)?;
        observer.select(&frozen.id);
        settle(&mut observer)?;
        require(
            observer.selected.as_ref() == Some(&frozen),
            "Client selected a different Mission identity",
        )?;
        let historical = observer
            .report_for(&frozen.id)
            .ok_or("Shared dated report missing")?;
        check_historical(historical)?;
        require(
            observer.needs_attention() == 1 && !observer.can_adopt_candidates(),
            "Shared incomplete Mission disappeared from attention or granted stale adoption",
        )?;
        save(
            root,
            &format!("03-{label}-needs-you.json"),
            &json!({"mission_id":frozen.id,"revision":frozen.revision,"attention_count":observer.needs_attention(),"report":historical}),
        )?;
    }
    let client = WorkspaceClient::for_state_dir(&state);
    let rejected = request(
        &client,
        WorkspaceCommand::SaveMission {
            expected_revision: original.revision,
            mission: original,
        },
    );
    // Even an uncertain write outcome is read back once; it is never resubmitted.
    let independent = WorkspaceClient::for_state_dir(&state);
    let readback = request(
        &independent,
        WorkspaceCommand::ReadMission {
            id: frozen.id.clone(),
        },
    );
    let cas_receipt = verify_stale_cas(rejected, readback, &frozen)?;
    save(root, "04-stale-cas-rejected.json", &cas_receipt)?;
    drop(independent);
    // The request is queued before refresh. A late successful inspection must
    // not restore authority revoked by refresh, even with the same revision.
    w.inspect();
    w.refresh();
    settle(&mut w)?;
    require(
        !w.can_adopt_candidates(),
        "Old in-flight inspection survived refresh",
    )?;
    check_historical(w.report.as_ref().ok_or("Refresh lost historical report")?)?;
    save(root, "05-refresh-barrier.json", &w.report)?;
    let refreshed = inspect(&mut w)?;
    check_positive(&refreshed, input)?;
    let mut changed = frozen.clone();
    changed
        .goal
        .push_str("; acceptance edit invalidates old evidence");
    w.save(changed);
    settle(&mut w)?;
    require(
        !w.can_adopt_candidates() && w.report.is_none(),
        "Saving the acceptance contract kept old evidence current",
    )?;
    let after_edit = inspect(&mut w)?;
    check_positive(&after_edit, input)?;
    save(
        root,
        "06-contract-change.json",
        &json!({"mission":w.selected,"report":after_edit}),
    )?;
    let saved = w.selected.clone().ok_or("Saved positive Mission missing")?;
    require(
        saved.evidence_after_ms >= frozen.evidence_after_ms
            && saved.revision == frozen.revision + 1,
        "Host acceptance boundary did not advance",
    )?;
    // All negative mutations are confined to a NEW disposable local clone.
    let forge_path = roots
        .iter()
        .find(|(n, _)| n == "beamiter/forge")
        .ok_or("Forge root missing")?
        .1
        .clone();
    let negative = root.join("negative-forge");
    require(!negative.exists(), "Negative clone already exists")?;
    git(
        root,
        &[
            "clone",
            "--no-hardlinks",
            forge_path.to_str().ok_or("Non UTF8 path")?,
            negative.to_str().ok_or("Non UTF8 path")?,
        ],
    )?;
    // Local cloning changes origin to the source path. Restore and verify the
    // actual authorized public origin so remote rejection cannot masquerade as
    // proof that the new local commit is absent from the frozen public master.
    const FORGE_ORIGIN: &str = "https://github.com/beamiter/forge.git";
    git(&negative, &["remote", "set-url", "origin", FORGE_ORIGIN])?;
    require(
        git(&negative, &["remote", "get-url", "origin"])? == FORGE_ORIGIN,
        "Negative clone did not retain the fixed public Forge origin",
    )?;
    let frozen_forge = git(&forge_path, &["rev-parse", "HEAD"])?;
    let dirty = negative.join("mission-contract-dirty.txt");
    require(!dirty.exists(), "Unexpected negative fixture collision")?;
    fs::write(
        &dirty,
        "Deliberate local dirty-input negative; never published\n",
    )
    .map_err(|e| e.to_string())?;
    let dirty_report = negative_plan(&mut w, &saved, &negative, "Negative dirty input", None)?;
    let dirty_forge = dirty_report
        .repositories
        .iter()
        .find(|r| r.identity.full_name == "beamiter/forge")
        .ok_or("Forge report absent")?;
    require(
        dirty_forge.observed.as_ref().is_some_and(|l| l.dirty)
            && !dirty_forge.committed
            && !w.can_adopt_candidates(),
        "Dirty input did not close candidate adoption",
    )?;
    check_no_validation(&dirty_report)?;
    save(root, "07-negative-dirty.json", &dirty_report)?;
    fs::remove_file(dirty).map_err(|e| e.to_string())?;
    const CORE: &str = "4ffa774138ee89dc2916ce21786c45e411196ca4";
    const OLD_CORE: &str = "f8566bb9cb6e9cca3258c8050c6dc4e9b96da8ff";
    for name in ["Cargo.toml", "Cargo.lock"] {
        let path = negative.join(name);
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        require(text.contains(CORE), "Negative pin source not found")?;
        fs::write(&path, text.replace(CORE, OLD_CORE)).map_err(|e| e.to_string())?;
    }
    git(&negative, &["add", "--", "Cargo.toml", "Cargo.lock"])?;
    git(
        &negative,
        &[
            "commit",
            "-m",
            "Local-only deliberate core pin mismatch for host contract",
        ],
    )?;
    let negative_head = git(&negative, &["rev-parse", "HEAD"])?;
    let bad_pin = negative_plan(
        &mut w,
        &saved,
        &negative,
        "Negative unpublished core pin",
        Some(negative_head.clone()),
    )?;
    let bad_forge = bad_pin
        .repositories
        .iter()
        .find(|r| r.identity.full_name == "beamiter/forge")
        .ok_or("Forge report absent")?;
    require(
        !bad_forge.pins_current && !bad_forge.pushed && !bad_forge.integrated,
        "Wrong/unpublished core pin passed delivery gates",
    )?;
    check_unpublished(bad_forge, &frozen_forge, &negative_head)?;
    require(
        bad_forge.observed.as_ref().is_some_and(|l| {
            l.manifest_core_pin.as_deref() == Some(OLD_CORE)
                && l.lock_core_pin.as_deref() == Some(OLD_CORE)
        }),
        "Pin mismatch was not actually observed from committed files",
    )?;
    require(
        bad_pin.state == WorkspaceMissionState::PartiallyIntegrated
            && bad_pin.repositories.iter().filter(|r| r.integrated).count() == 4,
        "Partial integration was not distinguished",
    )?;
    check_no_validation(&bad_pin)?;
    save(
        root,
        "08-negative-pin-partial.json",
        &json!({"scope":"Deliberate isolated local fixture, not historical user delivery","unpublished_commit":negative_head,"report":bad_pin}),
    )?;
    let missing = negative_plan(
        &mut w,
        &saved,
        &root.join("deliberately-missing-repository"),
        "Negative missing repository",
        None,
    )?;
    let missing_forge = missing
        .repositories
        .iter()
        .find(|r| r.identity.full_name == "beamiter/forge")
        .ok_or("Forge report absent")?;
    require(
        missing_forge.observed.is_none()
            && !missing_forge.errors.is_empty()
            && !w.can_adopt_candidates(),
        "Missing checkout was accepted",
    )?;
    check_no_validation(&missing)?;
    save(root, "09-negative-missing.json", &missing)?;
    match request(&client, WorkspaceCommand::Refresh)? {
        WorkspaceReply::Catalog { rows } => require(
            rows.is_empty(),
            "Mission operations created task/runtime rows",
        )?,
        _ => return Err("Unexpected task catalog".into()),
    }
    drop(w);
    drop(client);
    drop(host);
    let _reopened = start(&state, &root.join("evidence/host-reopened.log"))?;
    let mut read = MissionWorkbench::with_client(WorkspaceClient::for_state_dir(&state));
    read.refresh();
    settle(&mut read)?;
    read.select(&saved.id);
    settle(&mut read)?;
    require(
        read.selected.as_ref() == Some(&saved),
        "Host restart lost or changed the exact saved plan",
    )?;
    require(
        !read.can_adopt_candidates(),
        "Historical read granted candidate adoption",
    )?;
    check_historical(
        read.report
            .as_ref()
            .ok_or("Historical report missing after restart")?,
    )?;
    save(
        root,
        "10-reopened.json",
        &json!({"mission":read.selected,"report":read.report}),
    )?;
    let final_inspection = inspect(&mut read)?;
    check_positive(&final_inspection, input)?;
    save(root, "11-reinspected.json", &final_inspection)?;
    for (name, path) in &roots {
        require(
            git(path, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty(),
            &format!("Positive candidate was mutated: {name}"),
        )?;
    }
    Ok(
        json!({"status":"passed","contract":"five-repository-host-v1","delivery":"incomplete","provider_execution":"not_run","hosted_task_validation":"not_run","native_four_frontend_ui":"not_run","scope":"Real five public checkouts and real production host with four protocol clients; not four application UI tests","cases":["durable shared Mission identity","exact five candidate adoption","exact dated public master receipts","missing validation stays incomplete","stale CAS rejected","in-flight refresh evidence barrier","acceptance edit invalidates report","dirty input disables adoption","committed wrong pin localized","unpublished fixture yields partial integration","missing checkout rejected","same plan survives host restart","historical receipts cannot restore success","fresh reinspection restores source gates only","no provider or task rows created","positive candidates remain clean"]}),
    )
}
fn main() {
    if let Some(code) = run_embedded_if_requested() {
        std::process::exit(code);
    }
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("isolated contract run directory");
    let input: Value = serde_json::from_slice(
        &fs::read(root.join("observed-heads.json")).expect("prepared exact inputs"),
    )
    .expect("valid exact inputs");
    let result = flow(&root, &input);
    let output = match &result {
        Ok(value) => value.clone(),
        Err(error) => {
            json!({"status":"blocked_or_failed","delivery":"incomplete","provider_execution":"not_run","error":error})
        }
    };
    save(&root, "host-acceptance-result.json", &output).expect("write bounded result");
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    if result.is_err() {
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> WorkspaceMission {
        let mut plan = WorkspaceMission::new("Frozen fixture", "Exact acceptance goal");
        plan.revision = 2;
        plan
    }
    fn readback(plan: WorkspaceMission) -> Result<WorkspaceReply, String> {
        let report = WorkspaceMissionReport {
            id: plan.id.clone(),
            revision: plan.revision,
            state: WorkspaceMissionState::NeedsAttention,
            repositories: vec![],
            errors: vec![],
        };
        Ok(WorkspaceReply::Mission {
            mission: plan,
            report: Some(report),
        })
    }
    #[test]
    fn exact_conflict_and_unchanged_independent_readback_are_required() {
        let plan = plan();
        assert!(verify_stale_cas(
            Err(PINNED_CORE_CAS_CONFLICT.into()),
            readback(plan.clone()),
            &plan
        )
        .is_ok());
    }
    #[test]
    fn timeout_disconnect_and_unrelated_errors_cannot_pass_as_cas() {
        let plan = plan();
        for error in [
            "Acceptance request deadline",
            "Session host closed its response",
            "Operation not permitted",
            "Mission no longer exists",
            "Mission revision changed; reload before saving",
        ] {
            assert!(
                verify_stale_cas(Err(error.into()), readback(plan.clone()), &plan).is_err(),
                "Accepted unrelated failure: {error}"
            );
        }
    }
    #[test]
    fn conflict_text_wrapped_in_transport_error_is_not_a_conflict() {
        let plan = plan();
        assert!(verify_stale_cas(
            Err(format!("transport failed: {PINNED_CORE_CAS_CONFLICT}")),
            readback(plan.clone()),
            &plan
        )
        .is_err());
    }
    #[test]
    fn successful_stale_write_is_rejected() {
        let plan = plan();
        assert!(verify_stale_cas(readback(plan.clone()), readback(plan.clone()), &plan).is_err());
    }
    #[test]
    fn readback_transport_failure_cannot_pass() {
        let plan = plan();
        assert!(verify_stale_cas(
            Err(PINNED_CORE_CAS_CONFLICT.into()),
            Err("timeout".into()),
            &plan
        )
        .is_err());
    }
    #[test]
    fn changed_plan_with_same_revision_is_rejected() {
        let plan = plan();
        let mut changed = plan.clone();
        changed.goal = "Unexpected overwrite".into();
        assert!(verify_stale_cas(
            Err(PINNED_CORE_CAS_CONFLICT.into()),
            readback(changed),
            &plan
        )
        .is_err());
    }
    #[test]
    fn changed_revision_is_rejected() {
        let plan = plan();
        let mut changed = plan.clone();
        changed.revision += 1;
        assert!(verify_stale_cas(
            Err(PINNED_CORE_CAS_CONFLICT.into()),
            readback(changed),
            &plan
        )
        .is_err());
    }
    #[test]
    fn missing_report_after_rejection_is_not_preserved_state() {
        let plan = plan();
        let readback = Ok(WorkspaceReply::Mission {
            mission: plan.clone(),
            report: None,
        });
        assert!(verify_stale_cas(Err(PINNED_CORE_CAS_CONFLICT.into()), readback, &plan).is_err());
    }
    fn unpublished_report() -> WorkspaceMissionRepositoryReport {
        serde_json::from_value(json!({
            "identity": {"github_id":1170023936, "full_name":"beamiter/forge"},
            "path":"/fixture/forge", "candidate":"a".repeat(40), "observed":null,
            "modified":true, "committed":true, "pushed":false, "integrated":false,
            "pushed_observation":null,
            "integration_observation":{
                "remote":{"repository":{"github_id":1170023936, "full_name":"beamiter/forge"},
                          "ref_name":"refs/heads/master", "head":"b".repeat(40), "observed_at_ms":1},
                "candidate":"a".repeat(40), "contains_candidate":false},
            "validated":false, "pins_current":false, "checks":[], "covered_checks":[],
            "missing_checks":["validation"],
            "errors":["Manifest and lockfile do not both pin the required core commit",
                      "Integration evidence does not establish candidate ancestry on the target ref"]
        })).unwrap()
    }
    #[test]
    fn unpublished_requires_actual_exact_public_remote_observation() {
        assert!(check_unpublished(&unpublished_report(), &"b".repeat(40), &"a".repeat(40)).is_ok());
    }
    #[test]
    fn unsupported_origin_or_missing_receipt_cannot_pass_as_unpublished() {
        let mut report = unpublished_report();
        report.integration_observation = None;
        report.errors.push("Unsupported local origin".into());
        assert!(check_unpublished(&report, &"b".repeat(40), &"a".repeat(40)).is_err());
    }
    #[test]
    fn unrelated_remote_error_cannot_pass_as_unpublished() {
        let mut report = unpublished_report();
        report.errors.push("Remote request timed out".into());
        assert!(check_unpublished(&report, &"b".repeat(40), &"a".repeat(40)).is_err());
    }
    #[test]
    fn changed_remote_head_cannot_pass_as_frozen_unpublished() {
        let mut report = unpublished_report();
        report.integration_observation.as_mut().unwrap().remote.head = "c".repeat(40);
        assert!(check_unpublished(&report, &"b".repeat(40), &"a".repeat(40)).is_err());
    }
    #[test]
    fn positive_ancestry_cannot_pass_as_unpublished() {
        let mut report = unpublished_report();
        report
            .integration_observation
            .as_mut()
            .unwrap()
            .contains_candidate = true;
        assert!(check_unpublished(&report, &"b".repeat(40), &"a".repeat(40)).is_err());
    }
    #[test]
    fn wrong_candidate_cannot_pass_as_unpublished() {
        let mut report = unpublished_report();
        report.integration_observation.as_mut().unwrap().candidate = "c".repeat(40);
        assert!(check_unpublished(&report, &"b".repeat(40), &"a".repeat(40)).is_err());
    }
}

// Exercise the actual production reducer under the same pinned core, without GTK.
#[cfg(test)]
#[path = "../../../../src/ui/workspace_mission_evidence.rs"]
mod forge_mission_evidence;

#[cfg(test)]
mod evidence_poll_regression {
    #[test]
    fn empty_poll_does_not_create_evidence() {
        let mut evidence = super::forge_mission_evidence::MissionEvidence::default();
        evidence.poll(&[], false);
        assert!(evidence.report.is_none());
        assert!(evidence.selected.is_none());
    }
}
