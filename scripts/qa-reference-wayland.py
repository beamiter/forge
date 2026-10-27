#!/usr/bin/env python3
"""Private headless Sway/Pixman checks at real fractional output scales.

Run under a private dbus-run-session. GTK buffer scale and compositor output
scale are recorded separately. No font-DPI substitution or system install.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import tempfile
import shutil


def wait_for(probe, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = probe()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError("Native Wayland readiness timed out")


def walk(node):
    yield node
    for child in node.get("nodes", []) + node.get("floating_nodes", []):
        yield from walk(child)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--runtime", required=True)
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--focus-composer", action="store_true")
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    dirty = subprocess.check_output(["git", "diff", "--binary", "HEAD"])
    for name in subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"]).split(b"\0"):
        if name:
            dirty += b"\0UNTRACKED\0" + name + b"\0" + Path(os.fsdecode(name)).read_bytes()
    provenance = {"git_head": head, "dirty_diff_sha256": hashlib.sha256(dirty).hexdigest(), "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest()}
    provenance["core_head_at_capture"] = subprocess.check_output(["git", "-C", "../jterm_core", "rev-parse", "HEAD"], text=True).strip()
    results = []
    for scale in [1.25, 1.5]:
        folder = output / ("scale-" + str(int(scale * 100)))
        folder.mkdir(exist_ok=True)
        env = os.environ.copy()
        for key in ["DISPLAY", "WAYLAND_DISPLAY", "SWAYSOCK", "GDK_SCALE", "GDK_DPI_SCALE"]:
            env.pop(key, None)
        for name in ["runtime", "config", "cache", "data", "state"]:
            directory = Path(tempfile.mkdtemp(prefix="forge-wl-")) if name == "runtime" else folder / name
            directory.mkdir(exist_ok=True)
            directory.chmod(0o700)
            env["XDG_" + name.upper() + ("_DIR" if name == "runtime" else "_HOME")] = str(directory)
        config = folder / "sway.conf"
        config.write_text(f"xwayland disable\noutput * resolution 2400x1600 scale {scale}\nseat seat0 fallback true\nfocus_follows_mouse no\ndefault_border none\ndefault_floating_border none\n")
        env.update(WLR_BACKENDS="headless", WLR_RENDERER="pixman", WLR_LIBINPUT_NO_DEVICES="1", WLR_HEADLESS_OUTPUTS="1")
        log = (folder / "sway.log").open("w")
        server = subprocess.Popen(["sway", "--unsupported-gpu", "-d", "-c", str(config)], env=env, stdout=log, stderr=log)
        processes = []
        try:
            runtime = Path(env["XDG_RUNTIME_DIR"])
            socket = wait_for(lambda: next((p for p in runtime.glob("wayland-*") if p.is_socket()), None))
            ipc = wait_for(lambda: next(runtime.glob("sway-ipc.*.sock"), None))
            env["WAYLAND_DISPLAY"] = socket.name
            env["SWAYSOCK"] = str(ipc)
            def run(argv):
                return subprocess.run(argv, env=env, check=True, capture_output=True, text=True, timeout=30)
            def sway(value):
                result = json.loads(run(["swaymsg", "-r", value]).stdout)
                if any(not item.get("success") for item in result):
                    raise RuntimeError(result)
            for mode in ["block", "chat"]:
                private = folder / mode
                private.mkdir(exist_ok=True)
                private.chmod(0o700)
                shell_rc = private / "reference.bashrc"
                integration = Path(__file__).resolve().parent / "shell-integration/forge.bash"
                shell_rc.write_text("PS1='> '\nsource " + str(integration) + "\n")
                shell_rc.chmod(0o600)
                shell = private / "bash"
                shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --rcfile " + str(shell_rc) + " -i\n")
                shell.chmod(0o700)
                settings = Path(env["XDG_CONFIG_HOME"]) / "forge"
                settings.mkdir(exist_ok=True)
                settings.chmod(0o700)
                (settings / "config.toml").write_text(f'shell = "{shell}"\njsh_update_check = "never"\nopacity = 1.0\nfont = "DejaVu Sans Mono 11"\nchrome_font_size = "10px"\n')
                (settings / "config.toml").chmod(0o600)
                geometry = private / "gtk-geometry.json"
                child_env = env.copy()
                child_env["HISTFILE"] = str(private / "bash_history")
                child_env.update(GDK_BACKEND="wayland", GSK_RENDERER="cairo", GTK_A11Y="none", GTK_USE_PORTAL="0", FORGE_SHELL=str(shell), JTERM_REFERENCE_FIXTURE="1", FORGE_REFERENCE_GEOMETRY=str(geometry), FORGE_REFERENCE_KEY_TRACE=str(private / "key-trace.jsonl"))
                with (folder / (mode + "-native.log")).open("w") as app_log:
                    app = subprocess.Popen([args.runtime, args.binary, "--mode", mode, "--no-restore"], env=child_env, stdout=app_log, stderr=app_log)
                    processes.append(app)
                    def window():
                        if app.poll() is not None:
                            raise RuntimeError(f"Forge exited {app.returncode}")
                        tree = json.loads(run(["swaymsg", "-t", "get_tree", "-r"]).stdout)
                        return next((node for node in walk(tree) if node.get("pid") == app.pid and node.get("type") in ("con", "floating_con")), None)
                    node = wait_for(window)
                    sway(f'[con_id={node["id"]}] floating enable, resize set width 1356 px height 704 px, move position 0 0, focus')
                    time.sleep(1.0)
                    if mode == "block":
                        run(["wtype", "-s", "250", "--", "bash " + args.fixture])
                        run(["wtype", "-s", "250", "-k", "Return"])
                        time.sleep(1.0)
                    node = window()
                    rect = node["rect"]
                    crop = f'{rect["x"]},{rect["y"]} {rect["width"]}x{rect["height"]}'
                    screenshot = folder / (mode + ".png")
                    run(["grim", "-g", crop, str(screenshot)])
                    if args.focus_composer and geometry.exists():
                        composer = json.loads(geometry.read_text()).get("composer")
                        if composer:
                            sway(f'seat seat0 cursor set {int(composer["x"]+100)} {int(composer["y"]+12)}')
                            sway('seat seat0 cursor press button1')
                            sway('seat seat0 cursor release button1')
                            time.sleep(0.25)
                    proof = private / "keyboard-proof.txt"
                    proof.unlink(missing_ok=True)
                    run(["wtype", "-s", "250", "--", "printf wayland-keyboard-ok > " + str(proof)])
                    run(["wtype", "-s", "250", "-k", "Return"])
                    proof_error = None
                    try:
                        wait_for(lambda: proof.exists() and proof.read_text() == "wayland-keyboard-ok", timeout=8)
                    except RuntimeError as error:
                        proof_error = str(error)
                    outputs = json.loads(run(["swaymsg", "-t", "get_outputs", "-r"]).stdout)
                    results.append({**provenance, "mode": mode, "compositor": "private headless Sway/Pixman", "compositor_outputs": outputs, "requested_output_scale": scale, "sway_window_rect": rect, "gtk_geometry": json.loads(geometry.read_text()) if geometry.exists() else None, "keyboard_focus": proof.read_text() if proof.exists() else None, "keyboard_proof_error": proof_error, "explicit_composer_focus":args.focus_composer, "screenshot": str(screenshot)})
                    app.terminate()
                    app.wait(timeout=10)
            (output / "fractional-scale-results.json").write_text(json.dumps(results, indent=2))
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
            server.terminate()
            try:
                server.wait(timeout=8)
            except subprocess.TimeoutExpired:
                server.kill()
            log.close()
            shutil.rmtree(env["XDG_RUNTIME_DIR"], ignore_errors=True)
    print(json.dumps(results, indent=2))
    if any(result["keyboard_focus"] != "wayland-keyboard-ok" for result in results):
        raise SystemExit("Native Wayland keyboard proof failed; retained screenshots and diagnostics")


if __name__ == "__main__":
    main()
