#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT INT TERM
swiftc -swift-version 6 -parse-as-library \
  "$root/apps/macos/Sources/HysteriaX/Models/CredentialDisplay.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/APIModels.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/OpenAPIRequests.generated.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/OpenAPIResponses.generated.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/NodePackageDraft.swift" \
  "$root/apps/macos/Sources/HysteriaX/Support/DateDisplayParser.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/ServerConfigurationDraft.swift" \
  "$root/apps/macos/Sources/HysteriaX/Services/APIClient.swift" \
  "$root/apps/macos/Sources/HysteriaX/Services/NodeAlertNotifications.swift" \
  "$root/tests/node-package-client.swift" -o "$tmp_dir/node-package-client-tests"
"$tmp_dir/node-package-client-tests"
