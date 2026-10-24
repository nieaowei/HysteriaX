#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT INT TERM
swiftc -swift-version 6 -parse-as-library \
  "$root/apps/macos/Sources/HysteriaX/Support/Localization.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/APIModels.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/OpenAPIRequests.generated.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/OpenAPIResponses.generated.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/SubscriptionTemplateDocument.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateEditorGuidance.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateDocumentationOrder.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateProtocolOptions.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateDefaultCatalog.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateHTTPClientLayout.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateRequiredFields.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/MihomoTemplateRule.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateReferenceIndex.swift" \
  "$root/apps/macos/Sources/HysteriaX/Stores/SubscriptionTemplateStore.swift" \
  "$root/apps/macos/Sources/HysteriaX/Services/APIClient.swift" \
  "$root/apps/macos/Sources/HysteriaX/Services/ManagementEvent.swift" \
  "$root/apps/macos/Sources/HysteriaX/Services/ManagementWriteJournal.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/TemplateCollectionFields.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/TemplateHTTPClientFields.swift" \
  "$root/apps/macos/Sources/HysteriaX/Models/TemplateFieldLayout.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/SubscriptionTemplateFields.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/TemplateStringListEditor.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/TemplateStructuredFieldEditor.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/MihomoTemplateRules.swift" \
  "$root/apps/macos/Sources/HysteriaX/Views/SubscriptionTemplateInjections.swift" \
  "$root/tests/template-picker-transitions.swift" -o "$tmp_dir/template-picker-tests"
"$tmp_dir/template-picker-tests" "$root/crates/hysteriax-server/src/api/subscriptions/templates/singbox.schema.json"
