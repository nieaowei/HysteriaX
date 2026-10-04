#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${TMPDIR:-/tmp}/hysteriax-overview-client"
mkdir -p "$BUILD"
python3 "$ROOT/scripts/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(rg --files "$ROOT/apps/macos/Sources/HysteriaX" -g '*.swift' | rg -v '/App/HysteriaXApp.swift$')
swiftc -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/tests/overview-client.swift" -o "$BUILD/overview-client"
"$BUILD/overview-client" "$BUILD" "$BUILD"
echo "Rendered overview fixtures: $BUILD"
