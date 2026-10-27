#!/usr/bin/env python3
"""Run the affected existing native GTK/PTY assertions on private displays."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

TESTS = [
    "block_view::fold_geometry_tests::fold_actions_update_visible_and_parked_geometry",
    "block_view::blocks::tests::chat_pair_keeps_native_surfaces_and_gap_is_not_accumulated",
    "block_view::chat_layout_tests::chat_bubbles_keep_alignment_through_resize_and_font_changes",
    "block_view::tests::chat_completion_never_expands_between_shell_markers",
    "block_view::scroll::canvas_native_tests::capped_output_hands_remaining_pixels_to_retained_canvas",
    "block_view::scroll::canvas_native_tests::retained_canvas_scrolls_through_frames_and_yields_to_reading",
    "ui::organism::tests::one_companion_crosses_splits_without_cloning_its_body_or_mind",
    "ui::organism::tests::companion_footprint_rejects_real_terminal_ink",
    "block_view::tests::native_ime_commit_cancel_and_independent_ptys",
    "block_view::tests::native_ime_send_button_preserves_composition",
    "block_view::readline_preservation_tests::search_and_native_split_preserve_readline_draft_cursor_and_undo",
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--runtime", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--bus-config", required=True)
    parser.add_argument("--only", action="append", choices=TESTS)
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    provenance = {"source":subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip(),"core":subprocess.check_output(["git","-C","../jterm_core","rev-parse","HEAD"],text=True).strip(),"dirty_diff_sha256":hashlib.sha256(subprocess.check_output(["git","diff","--binary","HEAD"])).hexdigest(),"test_binary":args.binary,"test_binary_sha256":hashlib.sha256(Path(args.binary).read_bytes()).hexdigest()}
    (output/"provenance.json").write_text(json.dumps(provenance,indent=2))
    results = []
    for number, name in enumerate(args.only or TESTS):
        private = output / ("xdg-" + str(number))
        private.mkdir(exist_ok=True)
        private.chmod(0o700)
        env = os.environ.copy()
        env["HISTFILE"] = str(private / "bash_history")
        for key in ["config", "state", "data", "cache", "runtime"]:
            directory = private / key
            directory.mkdir(exist_ok=True)
            directory.chmod(0o700)
            env["XDG_" + key.upper() + ("_DIR" if key == "runtime" else "_HOME")] = str(directory)
        env.update(GDK_BACKEND="x11", GSK_RENDERER="cairo", GTK_IM_MODULE="simple", GTK_A11Y="none", GTK_USE_PORTAL="0", JTERM_REFERENCE_FIXTURE="0")
        argv = ["xvfb-run", "-a", "-s", "-screen 0 1600x1000x24 -nolisten tcp", "dbus-run-session", "--config-file=" + args.bus_config, "--", args.runtime, args.binary, "--exact", name, "--ignored", "--test-threads=1", "--nocapture"]
        try:
            run = subprocess.run(argv, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180)
            text = run.stdout
            passed = run.returncode == 0 and "1 passed; 0 failed" in text
            result = {"test": name, "exit_code": run.returncode, "passed": passed}
        except subprocess.TimeoutExpired as error:
            text = (error.stdout or b"").decode() if isinstance(error.stdout, bytes) else (error.stdout or "")
            result = {"test": name, "passed": False, "timeout": True}
        (output / (str(number) + "-" + name.rsplit("::", 1)[-1] + ".log")).write_text(text)
        results.append(result)
        print(json.dumps(result), flush=True)
        (output / "results.json").write_text(json.dumps(results, indent=2))
    if any(not result["passed"] for result in results):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
