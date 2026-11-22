#!/usr/bin/env bash
# Hosted-only GTK4 synthetic display proof. Does not launch Forge or VTE.
set -Eeuo pipefail
umask 077
test $# -eq 1
core="$(cd -- "$1" && pwd -P)"
src="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
pin=5f665fe7c56f118927d0f319bbabd05eb3cbe045
tree=b7fee217fee0e8197758d6b96bf4b1060d7b9f78
[[ "$(git -C "$core" rev-parse --verify HEAD)" == "$pin" ]]
[[ "$(git -C "$core" rev-parse --verify "$pin:crates/jterm_engine")" == "$tree" ]]

scope="$(mktemp -d)"
trap 'rm -rf -- "$scope"' EXIT
mkdir -p "$scope"/{engine,home,config,data,cache,state,runtime}
chmod 700 "$scope/runtime"
# Materialize only the authenticated Git tree, never the potentially dirty checkout.
git -C "$core" archive "$pin:crates/jterm_engine" | tar -x -C "$scope/engine"
cp -- "$src"/{Cargo.toml,Cargo.lock} "$scope/"
cp -R -- "$src/src" "$scope/"
export CARGO_TARGET_DIR="$scope/target"
cd "$scope"
cargo fetch --locked
cargo test --offline --locked
cargo run --offline --locked --quiet > frames
cc -std=c11 -Wall -Wextra -Werror -O2 -o display "$src/display.c" $(pkg-config --cflags --libs gtk4 pangocairo)

: > "$scope/Xauthority"
chmod 600 "$scope/Xauthority"
timeout 15s env -i     PATH=/usr/bin:/bin LANG=C.UTF-8 HOME="$scope/home"     XDG_CONFIG_HOME="$scope/config" XDG_DATA_HOME="$scope/data" XDG_CACHE_HOME="$scope/cache"     XDG_STATE_HOME="$scope/state" XDG_RUNTIME_DIR="$scope/runtime"     GDK_BACKEND=x11 GDK_SCALE=1 GSK_RENDERER=cairo GTK_A11Y=none GSETTINGS_BACKEND=memory     dbus-run-session -- xvfb-run --auto-servernum --auth-file="$scope/Xauthority"       --server-args='-screen 0 1280x800x24 -nolisten tcp' "$scope/display" < frames
