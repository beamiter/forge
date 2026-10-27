#!/usr/bin/env python3
"""Run actual Files follow callback regressions with Python 3 and rustc only.

Production functions are extracted from the current checkout on every invocation.
The companion Rust template supplies deterministic UI/transport boundaries. No
Cargo build, cached dependencies, GTK display, network, or SSH server is needed.
"""

from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile
import textwrap


ROOT = Path(__file__).resolve().parents[1]


def item(source, signature):
    """Extract a rustfmt-formatted braced item, preserving its body verbatim."""
    if source.count(signature) != 1:
        raise ValueError(f"expected one production item: {signature}")
    start = source.index(signature)
    line_start = source.rfind("\n", 0, start) + 1
    indentation = source[line_start:start]
    if indentation.strip():
        raise ValueError(f"production item must begin a source line: {signature}")
    opening = source.index("{", start)
    # An item's closing brace occupies its own line at its declaration's indent.
    # This avoids treating braces inside Rust strings or comments as delimiters.
    closing = re.search(r"^" + re.escape(indentation) + r"}[ \t]*$", source[opening:], re.M)
    if closing is None:
        raise ValueError(f"cannot find production item end: {signature}")
    return textwrap.dedent(source[line_start:opening + closing.end()])


def between(source, start_marker, end_marker):
    if source.count(start_marker) != 1 or source.count(end_marker) != 1:
        raise ValueError(f"production boundary changed: {start_marker}")
    start = source.index(start_marker)
    end = source.index(end_marker, start)
    return source[start:end].rstrip()


def build_harness():
    source = (ROOT / "src/ui/file_tree.rs").read_text(encoding="utf-8")
    remote = (ROOT / "src/ui/remote_fs.rs").read_text(encoding="utf-8")
    template = (ROOT / "scripts/files_follow_harness.rs").read_text(encoding="utf-8")
    helpers = [
        "fn scan_entries(",
        "fn remap_remote_location(",
        "fn unique_observed_profile_index(",
        "fn observed_target_location(",
        "fn location_matches_observed_target(",
    ]
    methods = [
        "fn next_file_tree_remote_follow_intent(",
        "pub(crate) fn invalidate_file_tree_remote_follow(",
        "fn current_observed_ssh_command(",
        "fn observed_ssh_identity_is_current(",
        "fn stage_observed_remote_files(",
        "fn commit_file_tree_point_listing(",
        "fn navigate_file_tree_point(",
    ]
    state_tests = [
        "automatic_probe_replacement_and_late_retirement_keep_latest_cancellable",
        "completed_automatic_probe_is_retired_without_cancelling_its_result",
        "automatic_probe_cancellation_preserves_user_navigation_authority",
    ]
    replacements = {
        "navigation": between(
            source,
            "#[derive(Clone, Debug, PartialEq, Eq)]\nstruct FileTreeNavigationPoint",
            "fn validate_absolute_navigation_path",
        ),
        "context": "#[derive(Clone, Debug, PartialEq, Eq)]\n"
        + item(source, "struct RemoteFollowContext")
        + "\n"
        + item(source, "impl RemoteFollowContext"),
        "helpers": "\n\n".join(item(source, name) for name in helpers),
        "methods": "\n\n".join(item(source, name) for name in methods),
        "cancel-token": between(
            remote,
            "#[derive(Clone, Default)]\npub(crate) struct CancelToken",
            "/// The cancellation outcome:",
        ),
    }
    for index, name in enumerate(state_tests):
        replacements[f"state-test-{index}"] = "#[test]\n" + item(source, f"fn {name}(")
    for name, body in replacements.items():
        marker = f"// @files-follow:{name}"
        if template.count(marker) != 1:
            raise ValueError(f"expected one fixture marker: {marker}")
        template = template.replace(marker, body, 1)
    if "// @files-follow:" in template:
        raise ValueError("unexpanded production marker in Files follow fixture")
    return template


def build_path_dialog_harness():
    source = (ROOT / "src/ui/file_tree.rs").read_text(encoding="utf-8")
    template = (ROOT / "scripts/files_path_dialog_harness.rs").read_text(encoding="utf-8")
    signatures = [
        "pub(crate) fn present_file_tree_path_dialog(",
        "pub(crate) fn set_file_tree_root(",
        "fn file_tree_context_is_current(",
        "fn require_current_file_tree_context(",
        "fn validate_absolute_navigation_path(",
        "fn navigation_breadcrumbs(",
        "fn file_tree_context_matches(",
    ]
    for index, signature in enumerate(signatures):
        marker = f"// @files-path:item-{index}"
        if template.count(marker) != 1:
            raise ValueError(f"expected one fixture marker: {marker}")
        template = template.replace(marker, item(source, signature), 1)
    if "// @files-path:" in template:
        raise ValueError("unexpanded production marker in Files path-dialog fixture")
    return template


def main():
    harnesses = [
        ("files_follow", build_harness()),
        ("files_path_dialog", build_path_dialog_harness()),
    ]
    env = os.environ.copy()
    # These isolated callbacks intentionally exercise normal automatic follow.
    env.pop("FORGE_SAFE_MODE", None)
    for name, harness in harnesses:
        with tempfile.TemporaryDirectory(prefix=f"forge-{name}-") as directory:
            directory = Path(directory)
            source = directory / f"{name}_harness.rs"
            binary = directory / f"{name}_tests"
            source.write_text(harness, encoding="utf-8")
            subprocess.run(
                [
                    "rustc", "--edition=2021", "--test", "-D", "warnings",
                    "-C", "debuginfo=0", str(source), "-o", str(binary),
                ],
                check=True,
                env=env,
            )
            subprocess.run(
                [str(binary), "--test-threads=1", *sys.argv[1:]],
                check=True,
                env=env,
            )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"Files follow regression setup failed: {error}", file=sys.stderr)
        sys.exit(1)
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode or 1)
