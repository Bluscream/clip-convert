#!/usr/bin/env bash
# The build gate for clip-convert.
#
# GTK development headers are not available on the immutable host, so the build
# runs inside the `build-box` distrobox. The core crate has no UI dependencies
# and is checked on the host first, because that is seconds rather than minutes.
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTAINER="${LCC_CONTAINER:-build-box}"
RELEASE=0
PROBE=0

usage() {
    cat <<'USAGE'
Usage: build.sh [options]

  --release   Build optimised, not debug.

Every cargo step runs at nice 15 on half the cores, so a build never makes the
workstation unusable. Override with LCC_JOBS and LCC_NICE.
  --probe     After building, run the binary hidden and measure its idle CPU
              and file-descriptor count. This is the only check that catches a
              runtime regression such as a busy-wait loop. Needs a Wayland
              session and about 30 seconds.
  -h, --help  Show this message.

Steps: size limits -> format check -> clippy (pedantic, warnings are errors)
-> tests -> docs -> build.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) RELEASE=1 ;;
        --probe) PROBE=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

# Half the cores, at low priority. A full-speed cargo build makes the whole
# workstation unusable, and none of these steps are urgent.
JOBS="${LCC_JOBS:-$(( $(nproc) / 2 ))}"
(( JOBS < 1 )) && JOBS=1
NICE="${LCC_NICE:-15}"

in_container() {
    distrobox enter "$CONTAINER" -- bash -lc \
        "cd '$PROJECT_DIR' && CARGO_BUILD_JOBS=$JOBS nice -n $NICE $1"
}

echo "==> core crate on the host (no UI dependencies)"
cd "$PROJECT_DIR"
# --no-default-features drops the native input backend, which is the only part
# of the core needing system headers. Everything else is checked here, in
# seconds, rather than paying for a container round-trip.
CARGO_BUILD_JOBS="$JOBS" nice -n "$NICE" \
    cargo test -p clip-convert-core --no-default-features

echo "==> size limits"
"$PROJECT_DIR/scripts/limits.sh"

echo "==> format"
in_container "cargo fmt --all --check"

echo "==> clippy"
# Warnings are errors here, which is what makes the pedantic tier meaningful.
in_container "cargo clippy --workspace --all-targets --all-features -- -D warnings"

echo "==> tests"
in_container "cargo test --workspace"

echo "==> docs"
in_container "cargo doc --workspace --no-deps"

if [[ $RELEASE -eq 1 ]]; then
    echo "==> release build"
    in_container "cargo build --workspace --release"
    BINARY="$PROJECT_DIR/target/release/clip-convert"
else
    in_container "cargo build --workspace"
    BINARY="$PROJECT_DIR/target/debug/clip-convert"
fi

echo "==> built $BINARY"
ls -lh "$BINARY"

if [[ $PROBE -eq 1 ]]; then
    echo "==> idle probe"
    "$PROJECT_DIR/scripts/probe.sh" "$BINARY"
fi
