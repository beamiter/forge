#!/usr/bin/env python3
"""Mutation regressions for the production source identity guard."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
GUARD = ROOT / "scripts/check-shared-core-pin.py"
spec = importlib.util.spec_from_file_location("pin", GUARD)
pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pin)


class PinTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="forge-pin-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ("Cargo.toml", "Cargo.lock", "deny.toml", ".cargo/config.toml",
                     "flake.nix", "packaging/flatpak/cargo-sources.json"):
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        manifest = tomllib.loads((self.root / "Cargo.toml").read_text())
        self.revision = manifest["dependencies"]["jterm_core"]["rev"]
        self.stale_revision = "0" * 40 if self.revision != "0" * 40 else "1" * 40

    def replace(self, name, old, new, count=-1):
        path = self.root / name
        text = path.read_text()
        self.assertIn(old, text, f"mutation target absent in {name}")
        path.write_text(text.replace(old, new, count))

    def cli(self, optimize):
        env = dict(os.environ, PYTHONOPTIMIZE=str(optimize), PYTHONDONTWRITEBYTECODE="1")
        return subprocess.run([sys.executable, str(GUARD), "--root", str(self.root)],
                              env=env, capture_output=True, text=True, check=False)

    def assert_rejected(self, message):
        with self.assertRaisesRegex(ValueError, message):
            pin.check(self.root)
        self.assert_cli_rejected(message)

    def assert_cli_rejected(self, message):
        for optimize in (0, 1, 2):
            with self.subTest(optimize=optimize):
                result = self.cli(optimize)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Shared-core pin mismatch:", result.stderr)
                self.assertRegex(result.stderr, message)
                self.assertNotIn("Traceback", result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertNotIn("Production core source is consistent", result.stderr)

    def test_committed_sources_agree(self):
        self.assertEqual(pin.check(self.root), self.revision)
        for optimize in (0, 1, 2):
            with self.subTest(optimize=optimize):
                result = self.cli(optimize)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                self.assertEqual(result.stdout,
                                 f"Production core source is consistent: {self.revision}\n")

    def test_short_manifest_pin_is_rejected(self):
        self.replace("Cargo.toml", self.revision, self.revision[:12], 1)
        self.assert_rejected("full commit SHA")

    def test_wrong_repository_is_rejected(self):
        self.replace("Cargo.toml", "beamiter/jterm_core.git", "other/jterm_core.git", 1)
        self.assert_rejected("unexpected core repository")

    def test_production_path_is_rejected(self):
        self.replace("Cargo.toml", 'jterm_core = {', 'jterm_core = { path = "../jterm_core",', 1)
        self.assert_rejected("cannot use a path dependency")

    def test_dev_pin_mismatch_is_rejected(self):
        path = self.root / "Cargo.toml"
        production, dev = path.read_text().split('[dev-dependencies]', 1)
        path.write_text(production + '[dev-dependencies]' +
                        dev.replace(self.revision, self.stale_revision, 1))
        self.assert_rejected("test and runtime core pins differ")

    def test_sibling_override_is_rejected(self):
        (self.root / ".cargo/config.toml").write_text(
            '[patch."https://github.com/beamiter/jterm_core.git"]\n'
            'jterm_core = { path = "../jterm_core" }\n')
        self.assert_rejected("patch overrides")

    def test_root_manifest_patch_is_rejected(self):
        path = self.root / "Cargo.toml"
        path.write_text(path.read_text() +
                        '\n[patch."https://github.com/beamiter/jterm_core.git"]\n'
                        'jterm_core = { path = "../jterm_core" }\n')
        self.assert_rejected("patch overrides")

    def test_duplicate_lock_core_is_rejected(self):
        path = self.root / "Cargo.lock"
        path.write_text(path.read_text() + '\n[[package]]\nname = "jterm_core"\nversion = "0.2.0"\n')
        self.assert_rejected("exactly one core")

    def test_path_only_lock_is_rejected(self):
        path = self.root / "Cargo.lock"
        text = path.read_text()
        text = "\n".join(line for line in text.splitlines()
                         if not line.startswith('source = "git+https://github.com/beamiter/jterm_core.git'))
        path.write_text(text)
        self.assert_rejected("locked core source")

    def test_stale_deny_policy_pin_is_rejected(self):
        self.replace("deny.toml", self.revision, self.stale_revision)
        self.assert_rejected("policy does not allow the exact core pin")

    def test_flatpak_stale_pin_is_rejected(self):
        self.replace("packaging/flatpak/cargo-sources.json", self.revision, self.stale_revision)
        self.assert_rejected("Flatpak")

    def test_duplicate_flatpak_core_is_rejected(self):
        path = self.root / "packaging/flatpak/cargo-sources.json"
        sources = json.loads(path.read_text())
        core = next(entry for entry in sources if entry.get("url", "").removesuffix(".git") ==
                    "https://github.com/beamiter/jterm_core")
        sources.append(core.copy())
        path.write_text(json.dumps(sources))
        self.assert_rejected("Flatpak")

    def test_stale_nix_pin_is_rejected(self):
        self.replace("flake.nix", self.revision, self.stale_revision)
        self.assert_rejected("Nix lacks")

    def test_nix_bundle_substitution_is_rejected(self):
        path = self.root / "flake.nix"
        path.write_text(path.read_text() + '\n# packaging/jterm-core-src.tar.gz\n')
        self.assert_rejected("Nix substitutes")

    def test_malformed_manifest_is_rejected(self):
        path = self.root / "Cargo.toml"
        original = path.read_text()
        cases = (
            ('[dependencies', 'Shared-core pin mismatch:'),
            ('[dependencies]\njterm_core = 42\n', 'Shared-core pin mismatch:'),
            ('[dependencies]\n', 'Shared-core pin mismatch:'),
            (original.replace(f'rev = "{self.revision}"', 'rev = 42', 1), 'full commit SHA'),
        )
        for content, message in cases:
            with self.subTest(manifest=content[:80]):
                path.write_text(content)
                self.assert_cli_rejected(message)


if __name__ == "__main__":
    unittest.main()
