#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="$ROOT/apps/macos/Sources/HysteriaX"
BUILD="$ROOT/apps/macos/.build/node-task-feedback"
mkdir -p "$BUILD"
swiftc -swift-version 6 -parse-as-library \
  "$SOURCE/Models/APIModels.swift" \
  "$SOURCE/Models/OpenAPIRequests.generated.swift" \
  "$SOURCE/Models/OpenAPIResponses.generated.swift" \
  "$SOURCE/Services/APIClient.swift" \
  "$SOURCE/Support/DateDisplayParser.swift" \
  "$SOURCE/Models/NodeTaskFeedback.swift" \
  "$ROOT/tests/node-task-feedback.swift" \
  -o "$BUILD/node-task-feedback"
"$BUILD/node-task-feedback"
