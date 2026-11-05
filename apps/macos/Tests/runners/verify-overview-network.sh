#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BUILD="${TMPDIR:-/tmp}/hysteriax-overview-network"
mkdir -p "$BUILD"
python3 "$ROOT/tests/support/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(rg --files "$ROOT/apps/macos/Sources/HysteriaX" -g '*.swift' | rg -v '/App/HysteriaXApp.swift$')
swiftc -D HYSTERIAX_UI_TESTING -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/apps/macos/Tests/overview-network.swift" -o "$BUILD/overview-network"
"$BUILD/overview-network" "$BUILD"
