#!/usr/bin/env python3
"""Mutation regressions for the production source identity guard."""
import importlib.util
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("pin", ROOT / "scripts/check-shared-core-pin.py")
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

    def test_committed_sources_agree(self):
        self.assertEqual(len(pin.check(self.root)), 40)

    def test_sibling_override_is_rejected(self):
        (self.root / ".cargo/config.toml").write_text(
            '[patch."https://github.com/beamiter/jterm_core.git"]\n'
            'jterm_core = { path = "../jterm_core" }\n')
        with self.assertRaisesRegex(AssertionError, "patch overrides"):
            pin.check(self.root)

    def test_path_only_lock_is_rejected(self):
        path = self.root / "Cargo.lock"
        text = path.read_text()
        text = "\n".join(line for line in text.splitlines()
                         if not line.startswith('source = "git+https://github.com/beamiter/jterm_core.git'))
        path.write_text(text)
        with self.assertRaisesRegex(AssertionError, "locked core source"):
            pin.check(self.root)

    def test_flatpak_stale_pin_is_rejected(self):
        path = self.root / "packaging/flatpak/cargo-sources.json"
        revision = pin.check(self.root)
        path.write_text(path.read_text().replace(revision, "0" * 40))
        with self.assertRaisesRegex(AssertionError, "Flatpak"):
            pin.check(self.root)

    def test_nix_bundle_substitution_is_rejected(self):
        path = self.root / "flake.nix"
        path.write_text(path.read_text() + '\n# packaging/jterm-core-src.tar.gz\n')
        with self.assertRaisesRegex(AssertionError, "Nix substitutes"):
            pin.check(self.root)


if __name__ == "__main__":
    unittest.main()
