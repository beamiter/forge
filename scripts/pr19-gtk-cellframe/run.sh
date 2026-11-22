#!/usr/bin/env bash
# GTK4 display proof against synthetic and real-PTY CellFrames.
set -Eeuo pipefail
umask 077
test $# -eq 1
core="$(cd -- "$1" && pwd -P)"
src="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
pin=7e30d95d304a20d7820ae2afcd740e813cf4e786
tree=b7fee217fee0e8197758d6b96bf4b1060d7b9f78
[[ "$(git -C "$core" rev-parse --verify "$pin:crates/jterm_engine")" == "$tree" ]]

scope="$(mktemp -d)"
trap 'rm -rf -- "$scope"' EXIT
mkdir -p "$scope"/{engine,home,config,data,cache,state,runtime}
chmod 700 "$scope/runtime"
export CARGO_TARGET_DIR="$scope/target"
# Export committed engine sources so local edits cannot change the candidate.
git -C "$core" archive "$pin:crates/jterm_engine" | tar -x -C "$scope/engine"
cp -- "$src"/{Cargo.toml,Cargo.lock} "$scope/"
cp -R -- "$src/src" "$scope/"
cd "$scope"
cargo fetch --locked
cargo test --offline --locked
cargo build --offline --locked --bin gtk-cellframe-display-contract
producer="$CARGO_TARGET_DIR/debug/gtk-cellframe-display-contract"
# pkg-config emits shell-escaped compiler arguments. Parse them without shell
# evaluation or glob expansion; a failed pkg-config query must stop the fixture.
python3 -I - "$src/display.c" <<'PYTHON'
import shlex
import subprocess
import sys

flags = subprocess.check_output(
    ["pkg-config", "--cflags", "--libs", "gtk4", "pangocairo"], text=True
)
subprocess.run(
    ["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-o", "display",
     sys.argv[1], *shlex.split(flags)],
    check=True,
)
PYTHON

isolated=(
    env -i PATH=/usr/bin:/bin LANG=C.UTF-8 HOME="$scope/home"
    XDG_CONFIG_HOME="$scope/config" XDG_DATA_HOME="$scope/data" XDG_CACHE_HOME="$scope/cache"
    XDG_STATE_HOME="$scope/state" XDG_RUNTIME_DIR="$scope/runtime"
)
for source in synthetic pty; do
    args=()
    if [[ "$source" == pty ]]; then
        args+=(--pty)
    fi
    timeout 25s "${isolated[@]}" "$producer" "${args[@]}" > frames
    : > "$scope/Xauthority"
    timeout 15s "${isolated[@]}" \
        GDK_BACKEND=x11 GDK_SCALE=1 GSK_RENDERER=cairo GTK_A11Y=none GSETTINGS_BACKEND=memory \
        dbus-run-session -- xvfb-run --auto-servernum --auth-file="$scope/Xauthority" \
        --server-args='-screen 0 1280x800x24 -nolisten tcp' "$scope/display" "${args[@]}" < frames
done
