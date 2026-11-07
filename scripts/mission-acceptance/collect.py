#!/usr/bin/env python3
"""Read-only public source/CI collector for the five-repository host contract."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import urllib.request

NAMES = ("jterm_core", "anvil", "forge", "ember", "frost")
CORE = "e8bd7a2b3edd48d7a0f8f0304fa3d8c864dab40e"
WORKFLOWS = {
    "jterm_core": ("ci.yml", "vim9-pixel-input.yml", "security.yml"),
    "anvil": ("ci.yml", "security.yml", "link-recovery.yml", "vim9-pixel-viewport.yml", "ui-layout.yml"),
    "forge": ("ci.yml", "flatpak.yml", "vim9-pixel-viewport.yml", "ui-layout.yml"),
    "ember": ("ci.yml", "security.yml", "ui-layout.yml"),
    "frost": ("ci.yml", "native-desktop.yml", "native-provider-acceptance.yml", "vim9-rewrite.yml", "ui-layout.yml"),
}
# These three fixed workflows have push paths filters. A manual run of the exact master
# is required when unrelated changes produce no push run. No other workflow may
# substitute a manual run. Anvil link recovery remains source/build-only with
# native recovery lanes explicitly SKIPPED. Forge Flatpak retains its full job.
MANUAL_WORKFLOWS = frozenset({("jterm_core", "security.yml"), ("anvil", "link-recovery.yml"), ("forge", "flatpak.yml")})
MISSION_FILES = {"anvil": "src/workspace_mission_ui.rs", "forge": "src/ui/workspace_missions.rs", "ember": "src/workspace_missions.rs", "frost": "src/workspace_mission.rs"}
HERE = Path(__file__).resolve().parent

def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def canonical_set(manifest):
    by_name = {r["name"]: r for r in manifest["repositories"]}
    return [(by_name[n]["full_name"], by_name[n]["revision"]) for n in NAMES]

def digest_set(manifest):
    return hashlib.sha256(json.dumps(canonical_set(manifest), separators=(",", ":")).encode()).hexdigest()

def validate_manifest(manifest, expected):
    if set(manifest) != {"schema", "contract", "frozen", "repositories"}:
        raise ValueError("Unexpected candidate fields; only public source identities are accepted")
    if manifest.get("schema") != 1 or manifest.get("contract") != "five-repository-host-v1":
        raise ValueError("Unknown acceptance contract")
    if manifest.get("frozen") is not True:
        raise ValueError("Candidates are not frozen; no checkout or host execution is allowed")
    entries = manifest.get("repositories", [])
    if len(entries) != 5 or {r.get("name") for r in entries} != set(NAMES):
        raise ValueError("Exactly the five authorized repository identities are required")
    for item in entries:
        if set(item) != {"name", "full_name", "revision"}:
            raise ValueError("Unexpected repository fields; no free-form data is accepted")
        if item.get("full_name") != "beamiter/" + item["name"] or not re.fullmatch(r"[0-9a-f]{40}", item.get("revision", "")):
            raise ValueError("Invalid public repository identity or exact commit")
    if next(r["revision"] for r in entries if r["name"] == "jterm_core") != CORE:
        raise ValueError("Harness core and candidate core differ")
    digest = digest_set(manifest)
    if not re.fullmatch(r"[0-9a-f]{64}", expected) or digest != expected:
        raise ValueError("The requested frozen candidate-set digest does not match")
    return digest

def fetch(full_name, suffix):
    if full_name not in {"beamiter/" + n for n in NAMES}:
        raise ValueError("Repository outside authorized set")
    url = f"https://api.github.com/repos/{full_name}/{suffix}"
    request = urllib.request.Request(url, headers={"Accept":"application/vnd.github+json", "User-Agent":"five-repository-host-contract-v1"})
    # Public reads only. No tokens, credentials, API writes, retries or redirects
    # to code-selected hosts are supported by this collector.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            raise ValueError("Unexpected redirect from fixed GitHub API endpoint")
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=30) as response:
        body = response.read(8 * 1024 * 1024 + 1)
    if len(body) > 8 * 1024 * 1024:
        raise ValueError("GitHub receipt exceeds budget")
    return json.loads(body)

def select_runs(item, response):
    if response.get("total_count", 0) > 100:
        raise ValueError("Workflow run pagination exceeds bounded receipt collector")
    candidates = response.get("workflow_runs", [])
    selected = []
    for filename in WORKFLOWS[item["name"]]:
        path = ".github/workflows/" + filename
        events = ("push", "workflow_dispatch") if (item["name"], filename) in MANUAL_WORKFLOWS else ("push",)
        matching = [r for r in candidates if r.get("event") in events and r.get("head_branch") == "master" and r.get("head_sha") == item["revision"] and r.get("path") == path]
        if not matching:
            raise ValueError(f"Missing exact-head master workflow ({'/'.join(events)}): {item['name']} {path}")
        latest = max(matching, key=lambda r: (r["id"], r.get("run_attempt", 1)))
        if latest.get("status") != "completed" or latest.get("conclusion") != "success":
            raise ValueError(f"Required workflow not successful: {item['name']} {path} {latest.get('status')} {latest.get('conclusion')}")
        selected.append({k:latest.get(k) for k in ("id", "name", "path", "head_sha", "head_branch", "event", "run_attempt", "status", "conclusion", "html_url", "created_at", "updated_at")})
    return selected

def receipts(manifest):
    result = []
    for item in manifest["repositories"]:
        remote = fetch(item["full_name"], "git/ref/heads/master")
        if remote.get("ref") != "refs/heads/master" or remote.get("object", {}).get("type") != "commit" or remote["object"].get("sha") != item["revision"]:
            raise ValueError(f"Frozen master moved: {item['full_name']}")
        response = fetch(item["full_name"], f"actions/runs?head_sha={item['revision']}&per_page=100")
        result.append({"repository":item["full_name"], "candidate":item["revision"], "observed_at":now(), "remote_ref":remote, "required_runs":select_runs(item,response)})
    return result

def git(path, *args):
    return subprocess.check_output(["git", "-C", str(path), *args], text=True).strip()

def source_preflight(root, manifest):
    for item in manifest["repositories"]:
        repo = root / "repositories" / item["name"]
        if git(repo, "rev-parse", "HEAD") != item["revision"] or git(repo, "status", "--porcelain=v1", "--untracked-files=all"):
            raise ValueError(f"Candidate changed or dirty: {item['name']}")
        if item["name"] in MISSION_FILES:
            if not (repo / MISSION_FILES[item["name"]]).is_file():
                raise ValueError(f"Published native Mission module missing: {item['name']}")
    # Reuse the strict source/pin/adapter checker, which emits a separately
    # scoped source receipt and does not assert native UI or task validation.
    subprocess.run([sys.executable, str(HERE / "audit-source-set.py"), str(root)], check=True)

def prepare(root, manifest_path, expected):
    data = manifest_path.read_bytes()
    if len(data) > 64 * 1024:
        raise ValueError("Frozen candidate input exceeds budget")
    manifest = json.loads(data)
    digest = validate_manifest(manifest, expected)
    if os.environ.get("GITHUB_ACTIONS") == "true":
        owner = os.environ.get("GITHUB_REPOSITORY")
        workflow_head = os.environ.get("GITHUB_SHA")
        if os.environ.get("GITHUB_REF") != "refs/heads/master" or not any(r["full_name"] == owner and r["revision"] == workflow_head for r in manifest["repositories"]):
            raise ValueError("Dispatch must use the reviewed master commit in the frozen set")
    # Fail closed BEFORE creating output or checking out unready repositories.
    before = receipts(manifest)
    root.mkdir(parents=True, exist_ok=False)
    (root / "evidence").mkdir()
    (root / "frozen-candidates.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (root / "evidence" / "ci-before.json").write_text(json.dumps(before, indent=2) + "\n")
    (root / "evidence" / "workflow-identity.json").write_text(json.dumps({key:os.environ.get(key) for key in ("GITHUB_REPOSITORY", "GITHUB_SHA", "GITHUB_REF", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT")}, indent=2) + "\n")
    observed = {"schema":1, "observed_at":now(), "candidate_set_sha256":digest, "repositories":[]}
    for item in manifest["repositories"]:
        repo = root / "repositories" / item["name"]
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        subprocess.run(["git", "-C", str(repo), "remote", "add", "origin", "https://github.com/" + item["full_name"] + ".git"], check=True)
        subprocess.run(["git", "-C", str(repo), "fetch", "--depth=1", "origin", item["revision"]], check=True)
        subprocess.run(["git", "-C", str(repo), "checkout", "--detach", "FETCH_HEAD"], check=True)
        observed["repositories"].append({**item, "path":str(repo), "target_ref":"refs/heads/master"})
    (root / "observed-heads.json").write_text(json.dumps(observed, indent=2) + "\n")
    shutil.copyfile(root / "observed-heads.json", root / "evidence" / "observed-heads.json")
    source_preflight(root, manifest)

def finish(root, expected):
    manifest = json.loads((root / "frozen-candidates.json").read_text())
    validate_manifest(manifest, expected)
    source_preflight(root, manifest)
    after = receipts(manifest)
    result = json.loads((root / "evidence" / "host-acceptance-result.json").read_text())
    if result.get("status") != "passed" or result.get("delivery") != "incomplete" or result.get("provider_execution") != "not_run":
        raise ValueError("Host result missing or exceeds the declared acceptance scope")
    (root / "evidence" / "ci-after.json").write_text(json.dumps(after, indent=2) + "\n")
    (root / "evidence" / "final-receipt.json").write_text(json.dumps({"contract":"five-repository-host-v1", "candidate_set_sha256":expected, "finished_at":now(), "host_contract":"passed", "five_repository_delivery":"not_completed", "native_four_frontend_combination":"not_run", "hosted_task_validation":"not_run", "workflow_status_is_not_a_native_semantic_assertion":True}, indent=2) + "\n")

def main():
    command, root, expected, *extra = sys.argv[1:]
    root = Path(root).resolve()
    if command == "prepare":
        if len(extra) > 1:
            raise ValueError("Unexpected prepare arguments")
        prepare(root, Path(extra[0]) if extra else HERE / "frozen-candidates.json", expected)
    elif command == "finish":
        if extra:
            raise ValueError("Unexpected finish arguments")
        finish(root, expected)
    else:
        raise ValueError("Unknown collector operation")

if __name__ == "__main__":
    main()
