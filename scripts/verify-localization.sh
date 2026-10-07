#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$(mktemp -d)"
trap 'rm -rf "$BUILD"' EXIT
SOURCE="$ROOT/apps/macos/Sources/HysteriaX"
python3 "$ROOT/tests/verify-localization-resources.py"
RESOURCE="${1:-$SOURCE/Resources}"
cp -R "$RESOURCE/en.lproj" "$RESOURCE/zh-Hans.lproj" "$BUILD/"
swiftc -swift-version 6 -parse-as-library \
  "$SOURCE/Support/Localization.swift" \
  "$SOURCE/Support/ManagementDisplayText.swift" \
  "$ROOT/tests/client-localization.swift" -o "$BUILD/localization-tests"
"$BUILD/localization-tests"
