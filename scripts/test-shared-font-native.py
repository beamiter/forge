#!/usr/bin/env python3
"""Exercise Forge zoom, font reload and PTY geometry on its shared GTK terminal."""
import argparse
import json
import os
from pathlib import Path
import re
import tempfile

from native import Atspi, accessible_text, application_nodes, sample_pixels, start_accessibility, wait_for
from native_jsh import application, xdotool


WITNESS = "shared-font-witness"
CELL_BACKGROUND = (18, 227, 113)
SHELL = r"""#!/usr/bin/python3
import json
import os
from pathlib import Path
import select
import tty

root = Path(__file__).parent
(root / "leader").write_text(str(os.getpid()))
tty.setraw(0)
os.write(1, b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;C;cmdline_url=font-probe\x07"
            b"\x1b[38;2;18;227;113;48;2;18;227;113mM\x1b[0m shared-font-witness\r\n")
previous = None
while True:
    size = os.get_terminal_size(0)
    if size != previous:
        temporary = root / "winsize.tmp"
        temporary.write_text(json.dumps(size))
        temporary.rename(root / "winsize.json")
        previous = size
    if select.select([0], [], [], 0.025)[0] and not os.read(0, 32):
        break
"""


def run(binary, mode, root):
    root.mkdir(mode=0o700)
    shell = root / "shell"
    shell.write_text(SHELL)
    shell.chmod(0o700)
    config = root / "forge.toml"

    def write_config(font):
        config.write_text(
            f'shell = {json.dumps(str(shell))}\nterminal_mode = "{mode}"\n'
            f'font = "{font}"\nfont_scale = 1.0\n'
        )
        config.chmod(0o600)

    write_config("Monospace 11")
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("JSH_", "JTERM_SHARED_"))
           and key not in ("FORGE_SHELL", "FORGE_HISTORY_PATH", "BASH_ENV")}
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                "XDG_RUNTIME_DIR"):
        directory = root / key
        directory.mkdir(mode=0o700)
        env[key] = str(directory)
    env.update(GDK_BACKEND="x11", GSK_RENDERER="cairo", GTK_A11Y="atspi", GTK_IM_MODULE="simple",
               JTERM_SHARED_ACTIVE_TERMINAL="1", JTERM_SHARED_TRANSCRIPT="1")
    with application(binary, config, env, root / "forge.log", restore=False) as process:
        winsize = root / "winsize.json"
        window = xdotool("search", "--onlyvisible", "--pid", str(process.pid)).split()[0]

        def geometry():
            screen = next((node for node in application_nodes(process.pid)
                           if node.get_description() == "Shared active terminal"
                           and node.get_state_set().contains(Atspi.StateType.SHOWING)), None)
            text = accessible_text(screen)
            if WITNESS not in text or not winsize.is_file():
                return None
            lines = text.split("\n")
            grid = (max(map(len, lines)), len(lines))
            if tuple(json.loads(winsize.read_text())) != grid:
                return None
            # GTK 4.14 supplies the text and component but lacks character
            # extents. Measure one painted cell in the actual GTK window.
            bounds = screen.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            width, height = min(bounds.width, 520), min(bounds.height, 160)
            if width < 1 or height < 1:
                return None
            pixels = sample_pixels(window, bounds, width, height)
            offsets = [index for index, pixel in enumerate(pixels) if pixel == CELL_BACKGROUND]
            if not offsets:
                return None
            xs = [index % width for index in offsets]
            ys = [index // width for index in offsets]
            return max(xs) - min(xs) + 1, max(ys) - min(ys) + 1, *grid

        def enlarged(before):
            current = geometry()
            if current and current[0] > before[0] and current[1] > before[1] and current[2] < before[2]:
                return current
            return None

        def saved_scale():
            match = re.search(r"(?m)^font_scale = ([0-9.]+)$", config.read_text())
            return float(match[1]) if match else 0.0

        before = wait_for("initial shared font and matching PTY grid", geometry)
        leader = (root / "leader").read_text()
        xdotool("key", "--clearmodifiers", "--delay", "60", *(["ctrl+equal"] * 12))

        # Observe the actual shortcut taking effect before checking the view.
        wait_for("terminal zoom preference saved", lambda: abs(saved_scale() - 1.3) < 0.005)
        zoomed = wait_for(f"{mode} larger live cells and resized PTY after Ctrl+=", lambda: enlarged(before))
        xdotool("key", "--clearmodifiers", "ctrl+0")
        wait_for("reset font and original PTY grid", lambda: geometry() == before)
        wait_for("reset zoom preference saved", lambda: saved_scale() == 1.0)
        write_config("Monospace 17")
        xdotool("key", "--clearmodifiers", "ctrl+shift+r")
        reloaded = wait_for(f"{mode} changed live font and matching PTY after reload", lambda: enlarged(before))
        assert (root / "leader").read_text() == leader, "font changes replaced the shell"
    print(f"Forge {mode} live font and PTY passed (initial={before}, zoom={zoomed}, reload={reloaded})", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--mode", choices=("block", "chat", "unified"), action="append")
    args = parser.parse_args()
    start_accessibility()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="forge-shared-font-") as directory:
        for mode in args.mode or ("block", "chat", "unified"):
            run(binary, mode, Path(directory) / mode)


if __name__ == "__main__":
    main()
