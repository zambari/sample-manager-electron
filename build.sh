#!/usr/bin/env bash
# Build Sample Manager.
#
# Usage:
#   ./build.sh          release build  -> target/release/sample_manager
#   ./build.sh run      release build, then launch the app
#   ./build.sh bundle   universal (Intel + Apple Silicon) .app, zipped
#                       -> target/bundle/Sample Manager.zip
#   ./build.sh windows  64-bit Windows .exe, cross-compiled with cargo-xwin
#                       -> target/bundle/Sample Manager Windows.zip
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
  bundle)
    app_name="Sample Manager"
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
    out="target/bundle"
    app="$out/$app_name.app"

    # Apple Silicon needs macOS 11 at least; use the same floor for Intel.
    export MACOSX_DEPLOYMENT_TARGET=11.0
    rustup target add x86_64-apple-darwin aarch64-apple-darwin >/dev/null
    cargo build --release --target x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin

    # Leave any Windows zip in $out alone.
    rm -rf "${app:?}" "${out:?}/${app_name:?}.zip"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    lipo -create -output "$app/Contents/MacOS/sample_manager" \
      target/x86_64-apple-darwin/release/sample_manager \
      target/aarch64-apple-darwin/release/sample_manager
    cp assets/icon.icns "$app/Contents/Resources/AppIcon.icns"

    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$app_name</string>
  <key>CFBundleDisplayName</key><string>$app_name</string>
  <key>CFBundleIdentifier</key><string>com.zambari.sample-manager</string>
  <key>CFBundleExecutable</key><string>sample_manager</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Sample Manager uses audio devices to play samples.</string>
</dict>
</plist>
PLIST

    # Not notarised: ad-hoc sign so Apple Silicon will run it at all.
    codesign --force --deep --sign - "$app"
    ditto -c -k --keepParent "$app" "$out/$app_name.zip"
    echo
    echo "Built: $(pwd)/$out/$app_name.zip"
    ;;
  windows)
    if ! command -v cargo-xwin >/dev/null 2>&1; then
      echo "cargo-xwin not found. Install it with: cargo install cargo-xwin --locked" >&2
      exit 1
    fi
    target=x86_64-pc-windows-msvc
    rustup target add "$target" >/dev/null
    # First run downloads the Microsoft CRT and Windows SDK (~1 GB) into
    # cargo-xwin's cache; later builds reuse it.
    cargo xwin build --release --target "$target"

    out="target/bundle"
    stage="$out/Sample Manager"
    rm -rf "$stage" "$out/Sample Manager Windows.zip"
    mkdir -p "$stage"
    cp "target/$target/release/sample_manager.exe" "$stage/Sample Manager.exe"
    (cd "$out" && zip -qr "Sample Manager Windows.zip" "Sample Manager")
    rm -rf "$stage"
    echo
    echo "Built: $(pwd)/$out/Sample Manager Windows.zip"
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
    echo "Use one of: release (default), run, bundle, windows, dev, test, clean" >&2
    exit 1
    ;;
esac
