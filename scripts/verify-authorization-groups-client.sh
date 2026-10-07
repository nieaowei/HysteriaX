#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD=$(mktemp -d)
trap 'rm -rf "$BUILD"' EXIT
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(rg --files "$ROOT/apps/macos/Sources/HysteriaX" -g '*.swift' | rg -v '/App/HysteriaXApp.swift$')
swiftc -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/tests/authorization-groups-client.swift" -o "$BUILD/checks"
"$BUILD/checks"
