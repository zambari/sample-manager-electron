#!/usr/bin/env bash
# Build Sample Manager.
#
# Usage:
#   ./build.sh          release build  -> target/release/sample_manager
#   ./build.sh run      release build, then launch the app
#   ./build.sh dev      debug build (faster to compile, slower to run)
#   ./build.sh test     run the test suite
#   ./build.sh clean    delete build output
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install Rust from https://rustup.rs, then re-run this script." >&2
  exit 1
fi

mode="${1:-release}"

case "$mode" in
  release)
    cargo build --release
    echo
    echo "Built: $(pwd)/target/release/sample_manager"
    ;;
  run)
    cargo run --release
    ;;
  dev)
    cargo build
    echo
    echo "Built: $(pwd)/target/debug/sample_manager"
    ;;
  test)
    cargo test
    ;;
  clean)
    cargo clean
    ;;
  *)
    echo "Unknown option: $mode" >&2
    echo "Use one of: release (default), run, dev, test, clean" >&2
    exit 1
    ;;
esac
