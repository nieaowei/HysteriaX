#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$ROOT/apps/macos/.build/main-split-resize"
mkdir -p "$BUILD"
python3 "$ROOT/scripts/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(rg --files "$ROOT/apps/macos/Sources/HysteriaX" -g '*.swift' | rg -v '/App/HysteriaXApp.swift$')
swiftc -swift-version 6 -whole-module-optimization -O -parse-as-library "${SOURCES[@]}" "$ROOT/tests/main-split-resize.swift" -o "$BUILD/main-split-resize"
"$BUILD/main-split-resize" "$BUILD" "${1:-$BUILD/results.json}"
