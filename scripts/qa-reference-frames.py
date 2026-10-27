#!/usr/bin/env python3
"""Retain actual native window pixels around short interaction sequences."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def run(*argv):
    return subprocess.run(argv, check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--runtime", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    private = out / "private"
    private.mkdir(exist_ok=True)
    private.chmod(0o700)
    env = os.environ.copy()
    for key in ["config", "data", "state", "cache", "runtime"]:
        folder = private / key
        folder.mkdir(exist_ok=True)
        folder.chmod(0o700)
        env["XDG_" + key.upper() + ("_DIR" if key == "runtime" else "_HOME")] = str(folder)
    env.update(GDK_BACKEND="x11", GSK_RENDERER="cairo", GTK_A11Y="none", GTK_USE_PORTAL="0", JTERM_REFERENCE_FIXTURE="0", GDK_SCALE="1", GDK_DPI_SCALE="1", HISTFILE=str(private / "bash_history"))
    rc = private / "reference.bashrc"
    rc.write_text("PS1='> '\nsource " + str(Path(__file__).resolve().parent / "shell-integration/forge.bash") + "\n")
    shell = private / "bash"
    shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --rcfile " + str(rc) + " -i\n")
    shell.chmod(0o700)
    config = private / "config/forge"
    config.mkdir(exist_ok=True)
    config.chmod(0o700)
    (config / "config.toml").write_text(f'shell="{shell}"\njsh_update_check="never"\nfont="DejaVu Sans Mono 11"\nopacity=1.0\nchrome_font_size="10px"\n')
    (config / "config.toml").chmod(0o600)
    env["FORGE_SHELL"] = str(shell)
    geometry = out / "gtk-geometry.json"
    env["FORGE_REFERENCE_GEOMETRY"] = str(geometry)
    provenance = {"git_head":run("git", "rev-parse", "HEAD").stdout.strip(), "core_head":run("git", "-C", "../jterm_core", "rev-parse", "HEAD").stdout.strip(), "binary_sha256":hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(), "kind":"actual native GTK window frame pixels; visual acceptance pending"}
    manifest = {**provenance, "sequences":[]}
    log = (out / "native.log").open("w")
    app = subprocess.Popen([args.runtime, args.binary, "--mode", "chat", "--no-restore"], env=env, stdout=log, stderr=log)
    try:
        window = None
        for _ in range(100):
            search = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(app.pid)], capture_output=True, text=True)
            if search.returncode == 0 and search.stdout.strip():
                window = search.stdout.splitlines()[0]
                break
            time.sleep(0.1)
        if window is None:
            raise RuntimeError("native Forge window missing")
        run("xdotool", "windowsize", "--sync", window, "1366", "714")
        run("xdotool", "windowfocus", "--sync", window)
        time.sleep(1.0)
        crop = [5, 5]
        def type_command(value):
            run("xdotool", "type", "--clearmodifiers", "--delay", "1", value)
        def key(value):
            run("xdotool", "key", value)
        def sequence(name, triggers):
            folder = out / name
            folder.mkdir(exist_ok=True)
            start = time.monotonic()
            result = {"name":name,"start_monotonic":start,"triggers":[],"frames":[],"requested_logical_window":[1356,704],"DPR":1}
            for frame in range(12):
                if frame in triggers:
                    description, trigger = triggers[frame]
                    before = time.monotonic()
                    trigger()
                    result["triggers"].append({"frame":frame,"relative_seconds":before-start,"action":description})
                path = folder / f"{frame:02d}.png"
                run("import", "-window", window, "-crop", f"1356x704+{crop[0]}+{crop[1]}", str(path))
                result["frames"].append({"frame":frame,"relative_seconds":time.monotonic()-start,"path":str(path),"sha256":hashlib.sha256(path.read_bytes()).hexdigest(),"gtk_geometry":json.loads(geometry.read_text()) if geometry.exists() else None})
                wait = start + (frame+1)*0.065 - time.monotonic()
                if wait > 0:
                    time.sleep(wait)
            result["duration_seconds"] = time.monotonic()-start
            manifest["sequences"].append(result)
            (out / "manifest.json").write_text(json.dumps(manifest,indent=2))
        proof = out / "enter-proof.txt"
        type_command("printf enter-frame-ok > " + str(proof) + "; printf 'enter frame\\n'")
        sequence("enter", {2:("real Return on prepared shell command",lambda:key("Return"))})
        time.sleep(0.2)
        manifest["enter_proof"] = proof.read_text() if proof.exists() else None
        type_command("for n in {1..32}; do printf 'burst %s\\n' \"$n\"; sleep .02; done")
        sequence("output-burst", {2:("real Return starts paced PTY output",lambda:key("Return"))})
        time.sleep(0.3)
        type_command("for n in {1..100}; do printf 'stream %s\\n' \"$n\"; sleep .02; done")
        key("Return")
        time.sleep(0.3)
        run("xdotool", "mousemove", "700", "360")
        sequence("wheel", {2:("wheel upward while real PTY output continues",lambda:run("xdotool","click","--repeat","8","--delay","5","4"))})
        def tab_at(x):
            run("xdotool","mousemove",str(x),"65")
            run("xdotool","click","1")
        sequence("inspector-tabs", {2:("open actual Context inspector",lambda:tab_at(1155)),8:("return to Agent inspector",lambda:tab_at(1085))})
        (out / "manifest.json").write_text(json.dumps(manifest,indent=2))
        if manifest["enter_proof"] != "enter-frame-ok":
            raise RuntimeError("native Enter proof failed")
        print(json.dumps({"manifest":str(out/"manifest.json"),"sequences":len(manifest["sequences"]),"frames":48,"enter_proof":manifest["enter_proof"]}))
    finally:
        app.terminate()
        try:
            app.wait(timeout=8)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait()
        log.close()


if __name__ == "__main__":
    main()
