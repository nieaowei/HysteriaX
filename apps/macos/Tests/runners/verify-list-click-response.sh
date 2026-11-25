#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BUILD="$ROOT/apps/macos/.build/list-click-response"
mkdir -p "$BUILD"
python3 "$ROOT/tests/support/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(find "$ROOT/apps/macos/Sources/HysteriaX" -type f -name '*.swift' ! -path '*/App/HysteriaXApp.swift' -print)
swiftc -swift-version 6 -whole-module-optimization -O -parse-as-library "${SOURCES[@]}" "$ROOT/apps/macos/Tests/list-click-response.swift" -o "$BUILD/list-click-response"
OUTPUT="${1:-$BUILD/results.json}"
"$BUILD/list-click-response" "$BUILD" "$OUTPUT"
