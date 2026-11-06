#!/usr/bin/env bash
# One private production host and four installed GUI applications. No provider.
set -Eeuo pipefail
umask 077
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
if [[ "${1:-}" != --session ]]; then
  root=$(realpath -- "${1:?acceptance root}")
  artifacts=$(realpath -- "${2:?downloaded artifact directory}")
  expected=${3:?frozen candidate digest}
  python3 "$here/provenance.py" verify "$root" "$expected" "$artifacts"
  test ! -e "$root/native-home"
  mkdir -m 0700 "$root/native-home" "$root/desktop-home"
  runtime=$(mktemp -d /tmp/native-mission-xdg.XXXXXXXX)
  state=$(mktemp -d /tmp/native-mission-state.XXXXXXXX)
  chmod 0700 "$runtime" "$state"
  # No inherited credentials, user profiles, provider PATH or app state. The
  # private bus owns its descendants; nested script cleans host/display first.
  exec env -i PATH=/usr/bin:/bin LANG=C.UTF-8 LC_ALL=C.UTF-8 \
    HOME="$root/desktop-home" USER="$(id -un)" \
    XDG_CONFIG_HOME="$root/desktop-home/.config" XDG_DATA_HOME="$root/desktop-home/.local/share" \
    XDG_STATE_HOME="$root/desktop-home/.local/state" XDG_CACHE_HOME="$root/desktop-home/.cache" \
    XDG_RUNTIME_DIR="$runtime" JTERM_WORKSPACE_STATE_DIR="$state" \
    GDK_BACKEND=x11 WINIT_UNIX_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=1 GSK_RENDERER=cairo \
    DISPLAY=:97 NO_AT_BRIDGE=0 GTK_A11Y=atspi \
    dbus-run-session -- bash "$here/run.sh" --session "$root" "$artifacts"
fi
shift
root=$(realpath -- "${1:?acceptance root}")
artifacts=$(realpath -- "${2:?downloaded artifacts}")
export HOME="$root/native-home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state" XDG_CACHE_HOME="$HOME/.cache"
evidence="$root/evidence"
state=$JTERM_WORKSPACE_STATE_DIR
host=0; display=0; wm=0
# Invoked indirectly by the EXIT trap, including failures before host startup.
# shellcheck disable=SC2317
cleanup() {
  # No broad pkill: terminate only the exact children this shell launched.
  for pid in "$host" "$wm" "$display"; do
    if ((pid > 0)); then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
mkdir "$root/installed"
# Each directory was validated against the exact same-run receipt before this
# environment was cleared. No wildcard may silently choose between two builds.
for app in anvil forge ember frost jterm_core; do
  shopt -s nullglob
  matches=("$artifacts/native-build-$app-"*)
  shopt -u nullglob
  ((${#matches[@]} == 1))
  source=${matches[0]}
  chmod 0755 "$source/bin/"*
  if [[ "$app" == jterm_core ]]; then
    mkdir -p "$root/installed/core/bin"
    for binary in jterm-sessiond mission-native-state; do
      install -m 0755 "$source/bin/$binary" "$root/installed/core/bin/$binary"
      cmp "$source/bin/$binary" "$root/installed/core/bin/$binary"
    done
  else
    install_home="$root/installed/$app"
    mkdir -m 0700 "$install_home"
    if [[ "$app" == anvil || "$app" == forge ]]; then
      bundle=$(python3 "$here/provenance.py" extract "$source" "$root/unpacked-$app" "$app")
      chmod 0755 "$bundle/bin/$app" "$bundle/bin/$app-support-bundle" "$bundle/libexec/"*.sh
      cmp "$source/bin/$app" "$bundle/bin/$app"
      env HOME="$install_home" XDG_CONFIG_HOME="$install_home/.config" \
        XDG_DATA_HOME="$install_home/.local/share" bash "$bundle/install.sh" --no-config \
        > "$evidence/install-$app.log" 2>&1
    else
      env HOME="$install_home" XDG_CONFIG_HOME="$install_home/.config" \
        XDG_DATA_HOME="$install_home/.local/share" \
        bash "$root/repositories/$app/scripts/install.sh" --binary "$source/bin/$app" \
          --prefix "$install_home/.local" --no-config > "$evidence/install-$app.log" 2>&1
    fi
    cmp "$source/bin/$app" "$install_home/.local/bin/$app"
    sha256sum "$source/bin/$app" "$install_home/.local/bin/$app" >> "$evidence/native-installed.sha256"
  fi
done
host_bin="$root/installed/core/bin/jterm-sessiond"
helper="$root/installed/core/bin/mission-native-state"
sha256sum "$host_bin" "$helper" >> "$evidence/native-installed.sha256"
{ uname -a; dpkg-query -W; env HOME="$root/desktop-home" XDG_CACHE_HOME="$root/desktop-home/.cache" fc-match monospace; } > "$evidence/native-environment.txt"
Xvfb "$DISPLAY" -screen 0 1600x1200x24 -nolisten tcp > "$evidence/native-xvfb.log" 2>&1 & display=$!
for _ in $(seq 1 100); do
  kill -0 "$display"
  if xdpyinfo >/dev/null 2>&1; then break; fi
  sleep .1
done
xdpyinfo > "$evidence/native-display.txt"
env HOME="$root/desktop-home" XDG_CONFIG_HOME="$root/desktop-home/.config" \
  XDG_CACHE_HOME="$root/desktop-home/.cache" openbox > "$evidence/native-openbox.log" 2>&1 & wm=$!
for _ in $(seq 1 100); do
  kill -0 "$wm"
  if xprop -root _NET_SUPPORTING_WM_CHECK | grep -q 'window id'; then break; fi
  sleep .1
done
xprop -root _NET_SUPPORTING_WM_CHECK | grep -q 'window id'
# Accessibility registration is private desktop setup, not application input.
# The bus was started with desktop-home, so activated services cannot dirty the
# pristine config-only application HOME before reader preflight.
python3 - <<'PY_A11Y'
import dbus
session = dbus.SessionBus()
props = dbus.Interface(session.get_object('org.a11y.Bus', '/org/a11y/bus'),
                       'org.freedesktop.DBus.Properties')
props.Set('org.a11y.Status', 'ScreenReaderEnabled', dbus.Boolean(True), timeout=3)
if not bool(props.Get('org.a11y.Status', 'ScreenReaderEnabled', timeout=3)):
    raise RuntimeError('Private accessibility activation failed')
PY_A11Y
"$host_bin" --jterm-session-host > "$evidence/native-production-host.log" 2>&1 & host=$!
for _ in $(seq 1 100); do
  kill -0 "$host"
  [[ -S "$state/run/host.sock" ]] && break
  sleep .1
done
test -S "$state/run/host.sock"
kill -0 "$host"
timeout 600 "$helper" seed "$root" "$state" "$host" "$host_bin" "$evidence/native-seed.json" \
  > "$evidence/native-seed.log" 2>&1
python3 "$here/provenance.py" reader-manifest "$root" "$state" "$host_bin"
# Record invariant checks even if any visible observation fails. Neither a
# successful helper check nor green CI may substitute for four visible passes.
reader_status=0
python3 "$here/reader.py" --manifest "$evidence/native-reader-input.json" \
  --plan "$evidence/native-frozen-plan.json" --receipt "$evidence/native-seed.json" \
  --state-dir "$state" --host-pid "$host" --output "$evidence/native-reader" \
  > "$evidence/native-reader.log" 2>&1 || reader_status=$?
kill -0 "$host"
timeout 180 "$helper" check "$root" "$state" "$host" "$host_bin" "$evidence/native-seed.json" \
  "$evidence/native-state-check.json" > "$evidence/native-state-check.log" 2>&1
exit "$reader_status"
