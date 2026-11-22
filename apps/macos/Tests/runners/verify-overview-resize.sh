#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BUILD="$ROOT/apps/macos/.build/overview-resize"
mkdir -p "$BUILD"
python3 "$ROOT/tests/support/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(find "$ROOT/apps/macos/Sources/HysteriaX" -type f -name '*.swift' ! -path '*/App/HysteriaXApp.swift' -print)
swiftc -swift-version 6 -O -parse-as-library "${SOURCES[@]}" "$ROOT/apps/macos/Tests/overview-resize.swift" -o "$BUILD/overview-resize"
"$BUILD/overview-resize" "$BUILD" "$BUILD"
echo "Resize measurements: $BUILD/results.json"
