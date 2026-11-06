#!/usr/bin/env bash
# Full production builds only. This script is not a provider/native runtime test.
set -Eeuo pipefail
umask 077
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
command=${1:?Expected packages or build}; shift
case "$command" in
  packages)
    lane=${1:?Expected build or runtime component}
    case "$lane" in jterm_core|anvil|forge|ember|frost|runtime) ;; *) exit 2 ;; esac
    # Ignore every image-selected mirror/source. Use Ubuntu's signed official
    # HTTPS archives only; network retries/time and whole step are bounded.
    source_list=$(mktemp)
    trap 'rm -f -- "$source_list"' EXIT
    printf '%s\n' \
      'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu noble main universe' \
      'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu noble-updates main universe' \
      'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://security.ubuntu.com/ubuntu noble-security main universe' > "$source_list"
    chmod 644 "$source_list"
    apt_network=(-o "Dir::Etc::sourcelist=$source_list" -o Dir::Etc::sourceparts=- \
      -o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
    packages=(build-essential pkg-config ca-certificates)
    case "$lane" in
      anvil|forge|runtime)
        packages+=(libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev libpcre2-dev) ;;
    esac
    case "$lane" in
      ember|frost|runtime)
        packages+=(libfontconfig1-dev libwayland-dev libx11-dev libx11-xcb-dev libxcb1-dev \
          libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxcursor-dev libxi-dev \
          libxrandr-dev libxkbcommon-dev libxkbcommon-x11-0 libegl1-mesa-dev libgl1-mesa-dev) ;;
    esac
    if [[ "$lane" == runtime ]]; then
      packages+=(xvfb openbox xdotool xauth xclip x11-utils dbus dbus-x11 python3 python3-dbus \
        python3-gi python3-pil python3-xlib gir1.2-atspi-2.0 at-spi2-core imagemagick \
        tesseract-ocr fonts-dejavu-core fonts-noto-cjk mesa-vulkan-drivers mesa-utils)
    fi
    sudo apt-get "${apt_network[@]}" update
    sudo apt-get "${apt_network[@]}" install --no-install-recommends -y "${packages[@]}"
    ;;
  build)
    root=$(realpath -- "${1:?acceptance root}")
    name=${2:?matrix component}
    expected=${3:?frozen digest}
    case "$name" in jterm_core|anvil|forge|ember|frost) ;; *) exit 2 ;; esac
    toolchain=$(python3 "$here/provenance.py" toolchain "$name")
    export RUSTUP_TOOLCHAIN="$toolchain" CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=never
    # Every Cargo invocation chooses the exact toolchain. Never change default.
    test "$(rustc "+$toolchain" --version | awk '{print $2}')" = "$toolchain"
    test "$(cargo "+$toolchain" --version | awk '{print $2}')" = "$toolchain"
    unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_HOME CARGO_TARGET_DIR
    output="$root/build-artifact"
    mkdir "$output"
    mkdir "$output/bin"
    export CARGO_TARGET_DIR="$root/target-$name"
    { rustc "+$toolchain" -vV; cargo "+$toolchain" --version; uname -a; df -h "$root"; } > "$output/toolchain.txt"
    repo="$root/repositories/$name"
    cd -- "$repo"
    metadata_features=()
    if [[ "$name" == forge || "$name" == ember ]]; then metadata_features+=(--all-features); fi
    cargo "+$toolchain" metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu \
      "${metadata_features[@]}" > "$output/cargo-metadata.json"
    python3 "$here/provenance.py" metadata "$output/cargo-metadata.json" "$output/dependency-msrv.json" "$name"
    case "$name" in
      jterm_core)
        cargo +1.86.0 build --release --locked --bin jterm-sessiond
        install -m 0755 "$CARGO_TARGET_DIR/release/jterm-sessiond" "$output/bin/jterm-sessiond"
        export CARGO_TARGET_DIR="$root/target-helper"
        rustup run 1.86.0 rustfmt --version >> "$output/toolchain.txt"
        rustup run 1.86.0 clippy-driver --version >> "$output/toolchain.txt"
        cargo +1.86.0 fmt --manifest-path "$here/../harness/Cargo.toml" -- --check
        cargo +1.86.0 clippy --locked --manifest-path "$here/../harness/Cargo.toml" \
          --bin mission-native-state -- -D warnings
        cargo +1.86.0 test --locked --manifest-path "$here/../harness/Cargo.toml" --bin mission-native-state
        cargo +1.86.0 metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu \
          --manifest-path "$here/../harness/Cargo.toml" > "$output/helper-cargo-metadata.json"
        python3 "$here/provenance.py" metadata "$output/helper-cargo-metadata.json" "$output/helper-dependency-msrv.json" "$name" helper
        cargo +1.86.0 build --release --locked --manifest-path "$here/../harness/Cargo.toml" --bin mission-native-state
        install -m 0755 "$CARGO_TARGET_DIR/release/mission-native-state" "$output/bin/mission-native-state"
        sha256sum "$here/../harness/Cargo.toml" "$here/../harness/Cargo.lock" > "$output/helper-source.sha256"
        ;;
      anvil)
        cargo +1.99.1 build --release --locked
        ;;
      forge)
        cargo +1.99.1 build --release --all-features --locked
        ;;
      ember)
        cargo +1.99.1 build -p ember --release --all-features --locked
        ;;
      frost)
        cargo +1.99.1 build --release --locked --bin frost
        ;;
    esac
    if [[ "$name" != jterm_core ]]; then
      install -m 0755 "$CARGO_TARGET_DIR/release/$name" "$output/bin/$name"
      if [[ "$name" == anvil || "$name" == forge ]]; then
        DIST_DIR="$output/dist" bash scripts/package-release.sh "$CARGO_TARGET_DIR/release/$name"
        (cd "$output/dist" && sha256sum --check ./*.sha256)
      fi
    fi
    python3 "$here/provenance.py" record "$root" "$expected" "$name" "$output"
    ;;
  *) printf 'Unknown build operation\n' >&2; exit 2 ;;
esac
