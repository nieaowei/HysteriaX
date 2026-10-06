#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$ROOT/apps/macos/.build/vertical-split"
mkdir -p "$BUILD"
swiftc -swift-version 6 -parse-as-library \
  "$ROOT/apps/macos/Sources/HysteriaX/Views/MainVerticalSplitView.swift" \
  "$ROOT/tests/vertical-split.swift" \
  -o "$BUILD/vertical-split"
"$BUILD/vertical-split"
