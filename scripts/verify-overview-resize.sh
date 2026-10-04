#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$ROOT/apps/macos/.build/overview-resize"
mkdir -p "$BUILD"
python3 "$ROOT/scripts/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(rg --files "$ROOT/apps/macos/Sources/HysteriaX" -g '*.swift' | rg -v '/App/HysteriaXApp.swift$')
swiftc -swift-version 6 -O -parse-as-library "${SOURCES[@]}" "$ROOT/tests/overview-resize.swift" -o "$BUILD/overview-resize"
"$BUILD/overview-resize" "$BUILD" "$BUILD"
echo "Resize measurements: $BUILD/results.json"
