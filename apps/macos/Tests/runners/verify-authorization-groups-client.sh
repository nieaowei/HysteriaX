#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BUILD=$(mktemp -d)
trap 'rm -rf "$BUILD"' EXIT
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(find "$ROOT/apps/macos/Sources/HysteriaX" -type f -name '*.swift' ! -path '*/App/HysteriaXApp.swift' -print)
swiftc -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/apps/macos/Tests/authorization-groups-client.swift" -o "$BUILD/checks"
"$BUILD/checks"
