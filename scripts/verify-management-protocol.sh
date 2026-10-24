#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$(mktemp -d "${TMPDIR:-/tmp}/hysteriax-management-protocol.XXXXXX")"
trap 'rm -rf "$BUILD"' EXIT
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(rg --files "$ROOT/apps/macos/Sources/HysteriaX" -g '*.swift' | rg -v '/App/HysteriaXApp.swift$')
swiftc -D HYSTERIAX_UI_TESTING -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/tests/management-protocol.swift" -o "$BUILD/management-protocol"
"$BUILD/management-protocol"
