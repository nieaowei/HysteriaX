#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
BUILD="$(mktemp -d "${TMPDIR:-/tmp}/hysteriax-fingerprint.XXXXXX")"
trap 'rm -rf "$BUILD"' EXIT
SOURCES=()
while IFS= read -r source; do SOURCES+=("$source"); done < <(find "$ROOT/apps/macos/Sources/HysteriaX" -type f -name '*.swift' ! -path '*/App/HysteriaXApp.swift' -print)
swiftc -D HYSTERIAX_UI_TESTING -swift-version 6 -parse-as-library "${SOURCES[@]}" "$ROOT/apps/macos/Tests/ssh-fingerprint-client.swift" -o "$BUILD/ssh-fingerprint-client"
"$BUILD/ssh-fingerprint-client"
