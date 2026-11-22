#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BUILD="${TMPDIR:-/tmp}/hysteriax-overview-client"
mkdir -p "$BUILD"
python3 "$ROOT/tests/support/overview-fixtures.py" "$BUILD"
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(find "$ROOT/apps/macos/Sources/HysteriaX" -type f -name '*.swift' ! -path '*/App/HysteriaXApp.swift' -print)
swiftc -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/apps/macos/Tests/overview-client.swift" -o "$BUILD/overview-client"
"$BUILD/overview-client" "$BUILD" "$BUILD"
echo "Rendered overview fixtures: $BUILD"
