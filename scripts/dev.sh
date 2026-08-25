#!/usr/bin/env bash
# Development convenience script. All native build commands run inside the
# pinned flake so `make verify` uses the same GTK/VTE toolchain as CI.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CMD="${1:-run}"

usage() {
    echo "Usage: $0 {run|build|test|test-display|check|fmt|clippy|security|verify|package|clean|watch}"
    echo
    echo "Commands:"
    echo "  run          - Run forge in development mode"
    echo "  build        - Build the optimized release binary"
    echo "  test         - Run all Rust targets and features"
    echo "  test-display - Run explicit GTK/VTE regressions under Xvfb"
    echo "  check        - Check all Rust targets and features"
    echo "  fmt          - Format the Rust source"
    echo "  clippy       - Run the repository lint policy"
    echo "  security     - Audit dependencies and shell scripts"
    echo "  verify       - Run the complete local quality gate"
    echo "  package      - Build a system-linked relocatable release bundle"
    echo "  clean        - Remove Cargo build artifacts"
    echo "  watch        - Watch the source and rerun forge"
}

case "${CMD}" in
    run|build|test|test-display|check|fmt|clippy|security|verify|package|clean|watch) ;;
    *)
        usage
        exit 1
        ;;
esac

if [[ "${CMD}" != package ]] && ! command -v nix >/dev/null 2>&1; then
    echo "Error: Nix with flakes support is required." >&2
    echo "Install Nix, then retry. CI uses the same flake environment." >&2
    exit 1
fi
if [[ "${CMD}" == package ]] && ! command -v cargo >/dev/null 2>&1; then
    echo "Error: Cargo and the system GTK/VTE development libraries are required for portable packaging." >&2
    exit 1
fi
if [[ "${CMD}" == package && -n "${IN_NIX_SHELL:-}" ]]; then
    echo "Error: portable packaging must run outside a Nix development shell." >&2
    echo "Exit the shell and run 'make package' with the system GTK/VTE toolchain." >&2
    exit 1
fi

cd "${PROJECT_ROOT}"

run_in_nix() {
    nix develop --command "$@"
}

case "${CMD}" in
    run)
        echo "Running forge in development mode..."
        run_in_nix cargo run --all-features --locked
        ;;

    build)
        echo "Building forge..."
        run_in_nix cargo build --release --all-features --locked
        ;;

    test)
        echo "Running tests..."
        run_in_nix cargo test --all-targets --all-features --locked --no-fail-fast
        ;;

    test-display)
        echo "Running display-backed GTK/VTE regressions..."
        run_in_nix bash scripts/test-gtk-display.sh
        ;;

    check)
        echo "Checking code..."
        run_in_nix cargo check --all-targets --all-features --locked
        ;;

    fmt)
        echo "Formatting Rust source..."
        run_in_nix cargo fmt --all
        ;;

    clippy)
        echo "Running Clippy..."
        run_in_nix cargo clippy --all-targets --all-features --locked -- -D warnings
        ;;

    security)
        echo "Running dependency and shell-script security checks..."
        run_in_nix bash scripts/security-check.sh
        ;;

    verify)
        echo "Running the complete quality gate..."
        # Expansion belongs to the inner Nix shell.
        # shellcheck disable=SC2016
        run_in_nix bash -c '
            set -Eeuo pipefail
            bash scripts/privacy-check.sh
            cargo fmt --all -- --check
            cargo check --all-targets --all-features --locked
            cargo test --all-targets --all-features --locked --no-fail-fast
            bash scripts/test-gtk-display.sh
            cargo clippy --all-targets --all-features --locked -- -D warnings
            RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked
            cargo build --release --all-features --locked
            mapfile -t shell_files < <(find scripts packaging -type f -name "*.sh" -print | sort)
            bash -n "${shell_files[@]}"
            bash scripts/test-install-paths.sh
        '
        ;;

    package)
        echo "Building a system-linked relocatable release bundle..."
        package_target_dir="${PACKAGE_TARGET_DIR:-target/system-package}"
        CARGO_TARGET_DIR="${package_target_dir}" cargo build --release --all-features --locked
        bash scripts/package-release.sh "${package_target_dir}/release/forge"
        ;;

    clean)
        echo "Cleaning build artifacts..."
        run_in_nix cargo clean
        ;;

    watch)
        echo "Watching source and rerunning forge..."
        run_in_nix cargo watch -x "run --all-features --locked"
        ;;
esac
