#!/usr/bin/env python3
"""Check the real oracle wrapper's Cargo argv, without building or running GTK."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class OracleWrapper(unittest.TestCase):
    def test_locked_is_added_once_before_harness_arguments(self):
        with tempfile.TemporaryDirectory(prefix="forge-oracle-contract-") as tmp:
            repo = Path(tmp)
            oracle = repo / "tools" / "vte-oracle"
            oracle.mkdir(parents=True)
            for name in ("run.py", "lock-packages.toml"):
                shutil.copyfile(ROOT / "tools" / "vte-oracle" / name, oracle / name)
            for name in ("Cargo.toml", "Cargo.lock"):
                shutil.copyfile(ROOT / name, repo / name)

            cargo = repo / "cargo"
            cargo.write_text(
                f"#!{sys.executable}\n"
                "import json, sys\n"
                "print(json.dumps(sys.argv[1:]))\n"
                "sys.exit(23)\n"
            )
            cargo.chmod(0o700)
            env = {**os.environ, "PATH": str(repo)}
            manifest = str(repo / "target/vte-oracle/source/Cargo.toml")
            for subcommand, args, cargo_args, harness in (
                ("test", ["--lib"], ["--lib"], []),
                ("test", ["--lib", "--all-features", "--locked", "--", "--ignored", "--list"],
                 ["--lib", "--all-features"], ["--", "--ignored", "--list"]),
                ("test", ["--lib", "--", "--locked"], ["--lib"], ["--", "--locked"]),
                ("test", ["--locked", "--", "--locked"], [], ["--", "--locked"]),
                ("check", ["--locked"], [], []),
                ("clippy", ["--", "-D", "warnings"], [], ["--", "-D", "warnings"]),
            ):
                with self.subTest(subcommand=subcommand, args=args):
                    result = subprocess.run(
                        [sys.executable, str(oracle / "run.py"), subcommand, *args],
                        env=env, capture_output=True, text=True, timeout=10,
                    )
                    self.assertEqual(result.returncode, 23, result.stderr)
                    self.assertEqual(json.loads(result.stdout), [
                        subcommand, "--manifest-path", manifest, "--locked", "--features",
                        "vte-oracle", *cargo_args, *harness,
                    ])


if __name__ == "__main__":
    unittest.main()
