#!/usr/bin/env python3
"""Check the committed production source identity without resolving dependencies.

This is a source-consistency gate, not a build or runtime acceptance test.
The runtime helper shared-core-path.py additionally checks Cargo metadata.
"""
import argparse
import json
from pathlib import Path
import re
import tomllib


def check(root: Path) -> str:
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    dep = manifest["dependencies"]["jterm_core"]
    rev = dep.get("rev", "")
    assert re.fullmatch(r"[0-9a-f]{40}", rev), "core requires a full commit SHA"
    assert dep.get("git") == "https://github.com/beamiter/jterm_core.git", "unexpected core repository"
    assert "path" not in dep, "production core cannot use a path dependency"
    dev = manifest["dev-dependencies"]["jterm_core"]
    assert all(dev.get(key) == dep.get(key) for key in ("git", "rev", "version")), "test and runtime core pins differ"
    configs = [manifest]
    config = root / ".cargo/config.toml"
    if config.exists():
        configs.append(tomllib.loads(config.read_text()))
    for config in configs:
        assert not any("jterm_core" in entries for entries in config.get("patch", {}).values()), "committed core patch overrides the production pin"
    packages = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    cores = [package for package in packages if package["name"] == "jterm_core"]
    assert len(cores) == 1, "lockfile must contain exactly one core"
    source = f"git+{dep['git']}?rev={rev}#{rev}"
    assert cores[0].get("source") == source, "locked core source differs from manifest"
    policy = tomllib.loads((root / "deny.toml").read_text())
    assert f"{dep['git']}?rev={rev}" in policy["sources"]["allow-git"], "dependency policy does not allow the exact core pin"
    flatpak = json.loads((root / "packaging/flatpak/cargo-sources.json").read_text())
    sources = [entry for entry in flatpak if entry.get("url", "").removesuffix(".git") == dep["git"].removesuffix(".git")]
    assert len(sources) == 1 and sources[0].get("commit") == rev, "Flatpak does not fetch the exact locked core"
    nix = (root / "flake.nix").read_text()
    assert source in nix and '"jterm_core-0.2.0"' in nix, "Nix lacks the pinned core source hash"
    assert "jterm-core-src.tar.gz" not in nix, "Nix substitutes a bundled core for the production pin"
    return rev


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        revision = check(args.root)
    except (AssertionError, KeyError, ValueError, OSError) as error:
        raise SystemExit(f"Shared-core pin mismatch: {error}") from error
    print(f"Production core source is consistent: {revision}")
