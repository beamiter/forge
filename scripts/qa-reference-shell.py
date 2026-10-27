#!/usr/bin/env python3
"""Capture the real native workspace on a private X display and XDG tree.

Run under xvfb-run and dbus-run-session. Fixture text is executed by the
actual shell and is explicitly named as test data, never agent state.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def command(*args, check=True):
    return subprocess.run(args, text=True, capture_output=True, check=check)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--runtime", required=True)
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--rich-fixture", action="store_true")
    parser.add_argument("--capture-mode", choices=["block", "chat"])
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    head = command("git", "rev-parse", "HEAD").stdout.strip()
    dirty = subprocess.check_output(["git", "diff", "--binary", "HEAD"])
    for raw in subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"]).split(b"\0"):
        if raw:
            dirty += b"\0UNTRACKED\0" + raw + b"\0" + Path(os.fsdecode(raw)).read_bytes()
    provenance = {"git_head": head, "dirty_diff_sha256": hashlib.sha256(dirty).hexdigest(), "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(), "fixture_sha256": hashlib.sha256(Path(args.fixture).read_bytes()).hexdigest()}
    provenance["core_head_at_capture"] = subprocess.check_output(["git", "-C", "../jterm_core", "rev-parse", "HEAD"], text=True).strip()
    results = []
    for mode in ([args.capture_mode] if args.capture_mode else ["block", "chat"]):
        private = output / ("xdg-" + mode)
        private.mkdir(parents=True, exist_ok=True)
        private.chmod(0o700)
        environment = os.environ.copy()
        environment["HISTFILE"] = str(private / "bash_history")
        for name in ["config", "data", "state", "cache", "runtime"]:
            directory = private / name
            directory.mkdir(parents=True, exist_ok=True)
            directory.chmod(0o700)
            environment["XDG_" + name.upper() + ("_DIR" if name == "runtime" else "_HOME")] = str(directory)
        config = private / "config" / "forge"
        config.mkdir(exist_ok=True)
        config.chmod(0o700)
        shell_bin = private / "bin"
        shell_bin.mkdir(exist_ok=True)
        shell_bin.chmod(0o700)
        shell_rc = private / "reference.bashrc"
        integration = Path(__file__).resolve().parent / "shell-integration" / "forge.bash"
        shell_rc.write_text("PS1='> '\nsource " + str(integration) + "\n")
        shell_rc.chmod(0o600)
        shell_wrapper = shell_bin / "bash"
        shell_wrapper.write_text("#!/bin/sh\nexec /bin/bash --noprofile --rcfile " + str(shell_rc) + " -i\n")
        shell_wrapper.chmod(0o700)
        (config / "config.toml").write_text('shell = "' + str(shell_wrapper) + '"\njsh_update_check = "never"\nopacity = 1.0\nfont = "DejaVu Sans Mono 11"\nchrome_font_size = "10px"\n')
        (config / "config.toml").chmod(0o600)
        geometry_path = output / (mode + "-geometry-live.json")
        geometry_path.unlink(missing_ok=True)
        environment.update(GDK_BACKEND="x11", GSK_RENDERER="cairo", GTK_USE_PORTAL="0", FORGE_SHELL=str(shell_wrapper), GDK_SCALE="1", GDK_DPI_SCALE="1", FORGE_REFERENCE_GEOMETRY=str(geometry_path), GTK_A11Y="none")
        environment["JTERM_REFERENCE_FIXTURE"] = "1" if args.rich_fixture else "0"
        with (output / (mode + "-native.log")).open("w") as log:
            process = subprocess.Popen([args.runtime, args.binary, "--mode", mode, "--no-restore"], env=environment, stdout=log, stderr=log)
            try:
                window = None
                for _ in range(80):
                    if process.poll() is not None:
                        raise RuntimeError(f"{mode} exited: {process.returncode}")
                    found = command("xdotool", "search", "--onlyvisible", "--pid", str(process.pid), check=False)
                    if found.returncode == 0 and found.stdout.strip():
                        window = found.stdout.splitlines()[0]
                        break
                    time.sleep(0.1)
                if window is None:
                    raise RuntimeError(f"No native {mode} window")
                command("xdotool", "windowsize", "--sync", window, "1356", "704")
                command("xdotool", "windowfocus", "--sync", window)
                command("xdotool", "mousemove", "1550", "1050")
                time.sleep(1.0)
                # X11's outer CSD surface includes GTK's resize shadow. Match
                # the actual GTK client size, then crop only that outer shadow.
                def resize_client(width, height):
                    command("xdotool", "windowsize", "--sync", window, str(width), str(height))
                    time.sleep(0.4)
                    native = json.loads(geometry_path.read_text()) if geometry_path.exists() else None
                    extra_x = width - native["logical_window"]["width"] if native else 0
                    extra_y = height - native["logical_window"]["height"] if native else 0
                    if extra_x or extra_y:
                        command("xdotool", "windowsize", "--sync", window, str(width + extra_x), str(height + extra_y))
                        time.sleep(0.5)
                    return (extra_x // 2, extra_y // 2)
                crop = resize_client(1356, 704)
                if not args.rich_fixture or mode == "block":
                    command("xdotool", "type", "--clearmodifiers", "--delay", "1", "bash " + args.fixture)
                    command("xdotool", "key", "Return")
                else:
                    command("xdotool", "mousemove", "650", "230")
                    command("xdotool", "click", "--repeat", "12", "--delay", "40", "4")
                    command("xdotool", "mousemove", "1550", "1050")
                time.sleep(1.8)
                command("import", "-window", window, "-crop", f"1356x704+{crop[0]}+{crop[1]}", str(output / (mode + "-1356x704.png")))
                proof = output / (mode + "-keyboard-proof.txt")
                proof.unlink(missing_ok=True)
                command("xdotool", "type", "--clearmodifiers", "--delay", "1", "printf keyboard-focus-ok > " + str(proof))
                command("xdotool", "key", "Return")
                for _ in range(30):
                    if proof.exists() and proof.read_text() == "keyboard-focus-ok":
                        break
                    time.sleep(0.1)
                results.append({**provenance, "mode": mode, "fixture_kind": "native rich components" if args.rich_fixture else "real PTY command", "window": window, "keyboard_focus": proof.read_text() if proof.exists() else "FAILED", "outer_x11_geometry": command("xdotool", "getwindowgeometry", "--shell", window).stdout, "native_geometry": json.loads(geometry_path.read_text()) if geometry_path.exists() else None, "requested_client": [1356, 704], "csd_shadow_crop": crop, "requested_dpr": 1, "screenshot": str(output / (mode + "-1356x704.png"))})
                if mode == "chat" and geometry_path.exists():
                    # The proof file precedes the shell's final OSC prompt.
                    # Let the real prompt settle, then take a fresh allocation
                    # snapshot instead of clicking the preceding Running row.
                    resize_client(1355, 704)
                    crop = resize_client(1356, 704)
                    geometry = json.loads(geometry_path.read_text())
                    tools = geometry.get("tools")
                    if tools:
                        command("xdotool", "mousemove", str(int(tools["x"] + tools["width"] / 2) + crop[0]), str(int(tools["y"] + tools["height"] / 2) + crop[1]))
                        command("xdotool", "click", "1")
                        time.sleep(0.4)
                        visible = command("xdotool", "search", "--onlyvisible", "--pid", str(process.pid), check=False)
                        results[-1]["tools_open_probe"] = {"geometry":geometry,"client_point":[int(tools["x"]+tools["width"]/2),int(tools["y"]+tools["height"]/2)],"root_point":[int(tools["x"]+tools["width"]/2)+crop[0],int(tools["y"]+tools["height"]/2)+crop[1]],"visible_x11_windows":visible.stdout.splitlines()}
                        command("import", "-window", "root", "-crop", f"1356x704+{crop[0]}+{crop[1]}", str(output / "chat-tools-popover.png"))
                        command("xdotool", "key", "Escape")
                        command("xdotool", "mousemove", "1550", "1050")
                if mode == "block":
                    command("xdotool", "key", "ctrl+l")
                    time.sleep(1.8)
                    command("import", "-window", window, "-crop", f"1356x704+{crop[0]}+{crop[1]}", str(output / "block-idle-organism.png"))
                results[-1]["responsive_captures"] = []
                for width, height, label in [(900, 704, "narrow"), (650, 704, "compact"), (1600, 900, "wide")]:
                    crop = resize_client(width, height)
                    time.sleep(0.6)
                    actual = json.loads(geometry_path.read_text())
                    if actual["logical_window"] != {"width": width, "height": height}:
                        raise RuntimeError(f"Native {mode} {label} minimum prevented requested size: {actual['logical_window']}")
                    screenshot = output / (mode + "-" + label + ".png")
                    command("import", "-window", window, "-crop", f"{width}x{height}+{crop[0]}+{crop[1]}", str(screenshot))
                    results[-1]["responsive_captures"].append({"label":label,"native_geometry":actual,"screenshot":str(screenshot)})
            finally:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    (output / "native-run.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))
    if any(result["keyboard_focus"] != "keyboard-focus-ok" for result in results):
        raise SystemExit("Native keyboard focus proof failed")


if __name__ == "__main__":
    main()
