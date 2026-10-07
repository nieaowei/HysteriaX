#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$(mktemp -d)"
trap 'rm -rf "$BUILD"' EXIT
swiftc -swift-version 6 -parse-as-library \
  "$ROOT/apps/macos/Sources/HysteriaX/Support/Localization.swift" \
  "$ROOT/apps/macos/Sources/HysteriaX/Support/ManagementDisplayText.swift" \
  "$ROOT/tests/management-display-text.swift" \
  -o "$BUILD/management-display-text"
"$BUILD/management-display-text"
