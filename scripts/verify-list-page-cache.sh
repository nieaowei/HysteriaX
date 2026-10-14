#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${TMPDIR:-/tmp}/hysteriax-list-page-cache"
mkdir -p "$BUILD"
swiftc -swift-version 6 -parse-as-library "$ROOT/apps/macos/Sources/HysteriaX/Stores/ListPageCache.swift" "$ROOT/tests/list-page-cache.swift" -o "$BUILD/list-page-cache"
"$BUILD/list-page-cache"
