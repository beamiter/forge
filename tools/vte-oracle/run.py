#!/usr/bin/env python3
"""Run the inline GTK suite with an independent, pinned comparison renderer."""

import fcntl
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
ORACLE = ROOT / "tools" / "vte-oracle"
MARKER = "\n[[package]]\n"


def once(source, old, new):
    if source.count(old) != 1:
        raise ValueError(f"unexpected fixture source shape for {old!r}")
    return source.replace(old, new, 1)


def package_name(block):
    match = re.match(r'name = "([^"]+)"\n', block.strip())
    if not match:
        raise ValueError("invalid lock package")
    return match[1]


def fixture_lock():
    header, *packages = (ROOT / "Cargo.lock").read_text().split(MARKER)
    extra = ("\n" + (ORACLE / "lock-packages.toml").read_text()).split(MARKER)[1:]
    names = {package_name(package) for package in packages}
    for package in extra:
        name = package_name(package)
        if name in names:
            raise ValueError(f"oracle dependency also present in the app lock: {name}")
        packages.append(package)
    app = next(index for index, package in enumerate(packages) if package_name(package) == "forge")
    packages[app] = once(packages[app], ' "uuid",\n', ' "uuid",\n "vte4",\n')
    packages.sort(key=package_name)
    return header.rstrip() + "\n" + "".join(
        f"{MARKER}{package.strip()}\n" for package in packages
    )


def write_if_changed(path, contents):
    if not path.exists() or path.read_text() != contents:
        path.write_text(contents)


def main():
    arguments = sys.argv[1:] or ["test", "--lib"]
    if arguments[0] not in ("test", "check", "clippy"):
        raise SystemExit("usage: run.py {test|check|clippy} [cargo arguments]")
    cache = ROOT / "target" / "vte-oracle"
    overlay = cache / "source"
    overlay.mkdir(parents=True, exist_ok=True)
    with (cache / "overlay.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for name in ("src", "tests", "examples", "vendor", "data", "docs", "qa",
                     "tools", "scripts", "packaging", ".github", "build.rs",
                     "README.md", "config.toml.example", "rust-toolchain.toml"):
            source = ROOT / name
            destination = overlay / name
            if source.exists() and not destination.is_symlink():
                destination.symlink_to(source, target_is_directory=source.is_dir())
        manifest = (ROOT / "Cargo.toml").read_text()
        manifest = once(manifest, "test = false\n\n[[bin]]", "test = true\n\n[[bin]]")
        manifest = once(manifest, "autoexamples = false", "autoexamples = true")
        manifest = once(manifest, "[features]\n", "[features]\nvte-oracle = []\n")
        manifest = once(manifest, "[dependencies]\n",
                        '[dependencies]\nvte4 = { version = "=0.10.0", features = ["v0_76"] }\n')
        write_if_changed(overlay / "Cargo.toml", manifest)
        write_if_changed(overlay / "Cargo.lock", fixture_lock())
        environment = os.environ.copy()
        environment.setdefault("CARGO_TARGET_DIR", str(ROOT / "target"))
        return subprocess.run(
            ["cargo", arguments[0], "--manifest-path", str(overlay / "Cargo.toml"),
             "--locked", "--features", "vte-oracle", *arguments[1:]],
            cwd=ROOT,
            env=environment,
        ).returncode


if __name__ == "__main__":
    sys.exit(main())
