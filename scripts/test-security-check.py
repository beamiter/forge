#!/usr/bin/env python3
"""Offline regressions for security-check.sh's cargo-audit failure handling."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

SCRIPT = Path(__file__).with_name("security-check.sh")
WORKFLOW = SCRIPT.parent.parent / ".github/workflows/security-audit.yml"
AUDIT_CONFIG = SCRIPT.parent.parent / ".cargo/audit.toml"
LOCKFILES = ("Cargo.lock", "prototypes/ascii-organism/Cargo.lock",
             "scripts/mission-acceptance/harness/Cargo.lock",
             "tools/eva-native-render/Cargo.lock")
DIAGNOSTICS = {
    "lookup": (
        "error: couldn't check if the package is yanked: "
        "not found: No such crate in crates.io index: chrono"
    ),
    "update": "warning: couldn't update crates.io index: connection failed",
    "open": "warning: couldn't open crates.io index: permission denied",
}
FAKE_CARGO = r"""#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
with open(os.environ["MOCK_CALLS"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(args) + "\n")
scenario = json.loads(os.environ["MOCK_SCENARIO"])
if args[0] == "metadata":
    manifest = args[args.index("--manifest-path") + 1] if "--manifest-path" in args else "Cargo.toml"
    if manifest == scenario.get("metadata_failure"):
        print("error: locked graph is invalid", file=sys.stderr)
        sys.exit(23)
    sys.exit(0)
if args[0] != "audit":
    print("unexpected cargo command", file=sys.stderr)
    sys.exit(91)
lockfile = args[args.index("--file") + 1]
print(f"Scanning {lockfile} for vulnerabilities")
if lockfile == scenario.get("lockfile"):
    for stream in ("stdout", "stderr"):
        value = scenario.get(stream, "")
        if value:
            print(value, file=getattr(sys, stream))
    sys.exit(scenario.get("status", 0))
sys.exit(0)
"""


FAKE_GREP = r"""#!/usr/bin/env python3
import json
import os
import subprocess
import sys

data = sys.stdin.read()
scenario = json.loads(os.environ["MOCK_SCENARIO"])
if f"Scanning {scenario['lockfile']} for vulnerabilities" in data:
    sys.exit(scenario["grep_status"])
sys.exit(subprocess.run(
    [os.environ["REAL_GREP"], *sys.argv[1:]], input=data, text=True, check=False
).returncode)
"""


class AuditGateTests(unittest.TestCase):
    def run_audit(self, scenario=None):
        # TMPDIR can keep all test writes in an isolated review directory.
        with tempfile.TemporaryDirectory(prefix="forge-security-check-") as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            (root / "bin").mkdir()
            shutil.copyfile(SCRIPT, root / "scripts/security-check.sh")
            cargo = root / "bin/cargo"
            cargo.write_text(FAKE_CARGO, encoding="utf-8")
            cargo.chmod(0o700)
            audit = root / "bin/cargo-audit"
            audit.write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8")
            audit.chmod(0o700)
            if scenario and "grep_status" in scenario:
                grep = root / "bin/grep"
                grep.write_text(FAKE_GREP, encoding="utf-8")
                grep.chmod(0o700)
            calls_path = root / "calls.jsonl"
            env = {
                **os.environ,
                "PATH": str(root / "bin") + os.pathsep + os.environ["PATH"],
                "MOCK_CALLS": str(calls_path),
                "MOCK_SCENARIO": json.dumps(scenario or {}),
                "REAL_GREP": shutil.which("grep") or "/usr/bin/grep",
                # The script must override workflow-level forced colors.
                "CARGO_TERM_COLOR": "always",
                "PYTHONDONTWRITEBYTECODE": "1",
            }
            result = subprocess.run(
                ["bash", str(root / "scripts/security-check.sh"), "--audit"],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            calls = (
                [json.loads(line) for line in calls_path.read_text().splitlines()]
                if calls_path.exists()
                else []
            )
        return result, calls

    def assert_audit_calls(self, calls, expected_lockfiles):
        audits = [call for call in calls if call[0] == "audit"]
        self.assertEqual(
            [call[call.index("--file") + 1] for call in audits],
            list(expected_lockfiles),
        )
        for call in audits:
            self.assertEqual(call[call.index("--deny") + 1], "warnings")
            self.assertEqual(call[call.index("--format") + 1], "terminal")
            self.assertEqual(call[call.index("--color") + 1], "never")
            for forbidden in (
                "--no-fetch", "--no-yanked", "--quiet", "--json", "--ignore", "--stale"
            ):
                self.assertNotIn(forbidden, call)
        return audits

    def test_clean_success_audits_all_locked_graphs(self):
        result, calls = self.run_audit()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_audit_calls(calls, LOCKFILES)
        metadata = [call for call in calls if call[0] == "metadata"]
        self.assertEqual(len(metadata), len(LOCKFILES))
        for call in metadata:
            self.assertIn("--locked", call)
            self.assertIn("--no-deps", call)

    def test_unrelated_error_words_are_not_infrastructure_failures(self):
        nearby = "\n".join(
            (
                "Scanning error-chain without a finding",
                "Title: error: couldn't check if the package is yanked: quoted text",
                "note: warning: couldn't update crates.io index: quoted text",
                "note: warning: couldn't open crates.io index: quoted text",
            )
        )
        for lockfile in LOCKFILES:
            for stream in ("stdout", "stderr"):
                with self.subTest(lockfile=lockfile, stream=stream):
                    result, calls = self.run_audit(
                        {"lockfile": lockfile, stream: nearby, "status": 0}
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assert_audit_calls(calls, LOCKFILES)

    def test_audit_configuration_keeps_incomplete_check_diagnostics_visible(self):
        config = tomllib.loads(AUDIT_CONFIG.read_text(encoding="utf-8"))
        self.assertIs(config["output"]["quiet"], False)
        self.assertIs(config.get("database", {}).get("fetch", True), True)
        self.assertIs(config.get("yanked", {}).get("enabled", True), True)
        self.assertIs(config.get("yanked", {}).get("update_index", True), True)

    def test_workflow_runs_regression_and_tracks_its_changes(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertEqual(workflow.count("      - scripts/test-security-check.py\n"), 2)
        self.assertIn("run: python3 scripts/test-security-check.py", workflow)
        self.assertLess(
            workflow.index("run: python3 scripts/test-security-check.py"),
            workflow.index("run: scripts/security-check.sh --audit"),
        )


def add_infrastructure_test(lockfile, diagnostic_name, stream):
    def test(self):
        diagnostic = DIAGNOSTICS[diagnostic_name]
        result, calls = self.run_audit(
            {"lockfile": lockfile, stream: diagnostic, "status": 0}
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(diagnostic, result.stdout + result.stderr)
        self.assertIn(
            f"Error: incomplete yanked-crate check for {lockfile}.", result.stderr
        )
        expected = LOCKFILES[: LOCKFILES.index(lockfile) + 1]
        self.assert_audit_calls(calls, expected)

    suffix = str(LOCKFILES.index(lockfile))
    setattr(AuditGateTests, f"test_{suffix}_{diagnostic_name}_{stream}_zero_exit_fails", test)


def add_exit_status_test(lockfile, status):
    def test(self):
        result, calls = self.run_audit(
            {
                "lockfile": lockfile,
                "stderr": DIAGNOSTICS["lookup"],
                "status": status,
            }
        )
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        self.assertNotIn("Error: incomplete yanked-crate check", result.stderr)
        expected = LOCKFILES[: LOCKFILES.index(lockfile) + 1]
        self.assert_audit_calls(calls, expected)

    suffix = str(LOCKFILES.index(lockfile))
    setattr(AuditGateTests, f"test_{suffix}_preserves_upstream_exit_{status}", test)


def add_finding_test(lockfile, finding):
    def test(self):
        result, calls = self.run_audit(
            {
                "lockfile": lockfile,
                "stdout": f"error: {finding} found!",
                "status": 1,
            }
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(finding, result.stdout + result.stderr)
        expected = LOCKFILES[: LOCKFILES.index(lockfile) + 1]
        self.assert_audit_calls(calls, expected)

    suffix = str(LOCKFILES.index(lockfile))
    setattr(AuditGateTests, f"test_{suffix}_rejects_{finding}", test)


def add_metadata_test(manifest):
    def test(self):
        result, calls = self.run_audit({"metadata_failure": manifest})
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse(any(call[0] == "audit" for call in calls))

    suffix = str(LOCKFILES.index(manifest.replace("Cargo.toml", "Cargo.lock")))
    setattr(AuditGateTests, f"test_{suffix}_invalid_lock_graph_stops_before_audit", test)


def add_checker_failure_test(lockfile, status, audit_status=0):
    def test(self):
        result, calls = self.run_audit(
            {
                "lockfile": lockfile,
                "grep_status": status,
                "status": audit_status,
                "stderr": DIAGNOSTICS["lookup"] if audit_status else "",
            }
        )
        self.assertEqual(
            result.returncode, audit_status or status, result.stdout + result.stderr
        )
        if audit_status:
            self.assertNotIn("Error: could not validate audit diagnostics", result.stderr)
        else:
            self.assertIn(
                f"Error: could not validate audit diagnostics for {lockfile} "
                f"(grep exited {status}).",
                result.stderr,
            )
        expected = LOCKFILES[: LOCKFILES.index(lockfile) + 1]
        self.assert_audit_calls(calls, expected)

    suffix = str(LOCKFILES.index(lockfile))
    name = f"test_{suffix}_checker_exit_{status}_audit_exit_{audit_status}"
    setattr(AuditGateTests, name, test)


for _lockfile in LOCKFILES:
    for _diagnostic in DIAGNOSTICS:
        for _stream in ("stdout", "stderr"):
            add_infrastructure_test(_lockfile, _diagnostic, _stream)
    for _status in (1, 2, 37):
        add_exit_status_test(_lockfile, _status)
    for _finding in ("vulnerability", "unmaintained", "unsound", "notice", "yanked"):
        add_finding_test(_lockfile, _finding)
    for _grep_status in (2, 127):
        add_checker_failure_test(_lockfile, _grep_status)
        add_checker_failure_test(_lockfile, _grep_status, audit_status=37)

for _manifest in (lock.replace("Cargo.lock", "Cargo.toml") for lock in LOCKFILES):
    add_metadata_test(_manifest)

if __name__ == "__main__":
    unittest.main(verbosity=2)
