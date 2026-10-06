#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$ROOT/apps/macos/.build/detail-header-layout"
mkdir -p "$BUILD"
swiftc -swift-version 6 -parse-as-library \
  "$ROOT/apps/macos/Sources/HysteriaX/Views/DetailHeaderLayout.swift" \
  "$ROOT/tests/detail-header-layout.swift" \
  -o "$BUILD/detail-header-layout"
"$BUILD/detail-header-layout"
