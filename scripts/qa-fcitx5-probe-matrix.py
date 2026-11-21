#!/usr/bin/env python3
"""Same-runner module probe diagnostics; does not retry or pass the Pinyin gate."""

import importlib.util
from pathlib import Path
import re
import subprocess
import tempfile


def load_pinyin():
    path = Path(__file__).with_name("qa-fcitx5-pinyin.py")
    spec = importlib.util.spec_from_file_location("fixture", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


pinyin = load_pinyin()
hosted = pinyin.hosted
FACTORS = ((False, False), (True, False), (False, True), (True, True))
# Accept only the preflight's fixed numeric/category diagnostics for reporting.
# Never log exception strings, environment values, or native subprocess output.
SAFE_PROBE = re.compile(
    r"GTK4 Fcitx probe (?:"
    r"reported diagnostics \(stderr_bytes=[0-9]{1,12}; gtk_im_module_missing=[0-9]{1,2}, "
    r"gtk_session_bus=[0-9]{1,2}, gtk_accessibility_bus=[0-9]{1,2}, unknown=[0-9]{1,2}; "
    r"sample_truncated=[01]\)|"
    r"timed out after 5s \(stdout=(?:no-output|expected-line|unexpected-output); stderr_bytes=[0-9]{1,12}\))"
)


def classify(error):
    detail = str(error)
    return detail if SAFE_PROBE.fullmatch(detail) else "probe-rejected-without-safe-detail"


def run_matrix(run=hosted.run, emit=print):
    for profile, policy in FACTORS:
        row = f"module-matrix profile={int(profile)} pinyin_bus_policy={int(policy)}"
        emit(f"{row} begin", flush=True)
        try:
            with tempfile.TemporaryDirectory(prefix="forge-fcitx-factor-", dir="/tmp") as scope:
                run(Path(scope), probe_profile=pinyin.install_profile if profile else None,
                    probe_pinyin_bus=policy)
        except hosted.preflight.Unavailable as error:
            emit(f"{row} probe-rejected: {classify(error)}", flush=True)
        except (hosted.owned.Unavailable, OSError, RuntimeError, subprocess.SubprocessError):
            emit(f"{row} setup-unavailable", flush=True)
        else:
            emit(f"{row} module-selected", flush=True)
    emit("Module diagnostics only; original native Pinyin gate outcome is unchanged.", flush=True)


if __name__ == "__main__":
    run_matrix()
