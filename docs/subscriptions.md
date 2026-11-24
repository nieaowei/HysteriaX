# Subscription templates and formats

## Initialization and compatibility

Use matching server and Mac client versions. Database maintenance initializes one published default template per native format. The master key encrypts both drafts and immutable published versions.

The public subscription route is `GET /sub/{token}`. Copy the canonical address from the Mac app or request it from `GET /api/v2/users/{id}/subscription`. For a saved address ending in `/clash.yaml`, copy a fresh Mihomo link with the intended template and platform.

New users receive separate connection passwords and subscription tokens. Rotating through `POST /api/v2/users/{id}/credentials/rotate` revokes both old values and queues session disconnection. Copy the new subscription address after rotation.

## Addresses and qualification

Example: `/sub/{token}?template=default-mihomo&platform=windows`.

- `template` selects a global published template. Every new request reads its current published version. A draft save never changes subscribers. Archived or unpublished explicit templates return 404.
- Explicit `format=mihomo|singbox` must match a selected template. Without a template, explicit format or recognized UA selects that format's current default.
- `platform=linux|openwrt|macos|windows|android|ios` selects a platform projection. OpenWrt aliases Linux. An explicit platform takes precedence over the user agent. For a selected native template or format, an unresolved platform opens a selection page scoped to that template.
- Opening the canonical address in a browser shows all active published templates. The Linux/OpenWrt, macOS, Windows, Android and iOS tabs switch the displayed addresses. The detected platform is selected and marked as recommended. An explicit platform filter limits the page to that target.
- Each template card provides copy and open actions for an address with explicit template, format and platform, plus collapsible compatibility notes. The unscoped page also has separate Base64 and URI cards.
- `format=uri|base64` produces authorized node links directly and needs no platform. Combining these formats with `template` returns 400.
- Invalid/repeated template, format, or platform parameters return 400.

Token revocation, user enablement/expiry/quota, effective authorization groups, deployed-node state, node package eligibility, and deployed mTLS trust continue to apply on every request. Templates cannot grant node access. Missing/revoked tokens return 404; inactive users return 403. Cache keys include user/node/credential state, template ID, published version, platform, and client capability. Default switches, publication, rollback, and archive are visible to new requests immediately.

Public responses use `Cache-Control: private, no-store`, `Vary: User-Agent`, `Referrer-Policy: no-referrer`, and `X-Robots-Tag: noindex, nofollow`. The selection page loads no external assets. Keep token-bearing addresses private.

## Configuration document

Use schema 2. A document contains `schema_version: 2`, `common` (one native configuration object), and independent `injections`. A complete template package carries that same document together with the format.

The editor changes the single configuration directly. `false`, `0`, `null`, and empty arrays remain ordinary values. Generation projects this configuration onto the selected platform, excluding unsupported fields and reporting the omissions. Validation covers the common configuration and all five platform projections. Schema 1 input is rejected; no automatic data migration is performed.

The versioned field catalogs derive from Mihomo **v1.19.31** and sing-box **v1.14.2**. Rust validates types, enumerations, ranges, required properties, applicable baseline platform constraints, policy references, and outbound cycles. The same catalogs drive typed Mac controls for scalars, enums, nested objects, maps, unions, and ordered arrays. Connection fields are injected read-only. Unknown native fields survive roundtrips and generate warnings; their platform compatibility is unverified. YAML anchors/merge keys and JSON comments/trailing commas are normalized on the server. Original comments and whitespace are not retained. Native documents require finite JSON-compatible numeric values and string mapping keys; unsupported custom YAML tags are diagnosed rather than silently converted.

The shared `platform-capabilities.json` records explicit upstream documentation and the project's client-owned field profile. It includes process/interface/user matching, Linux socket and namespace options, Android package matching, graphical-client requirements, TUN routing dependencies, and the established adaptation: Mihomo redirect on Linux/macOS and TProxy on Linux; sing-box TProxy on Linux, redirect on Linux/macOS, TUN auto-redirect on Linux, fixed TUN interface names on Linux/Windows, and client-owned TUN routing on Android/iOS. This is a structural and baseline capability profile, not proof of runtime behavior on every OS or wrapper client. Unknown fields do not acquire compatibility guarantees from the catalog.

## Draft, publication, and rollback

Open a template in the Mac workbench. Use the sidebar to switch modules or search for a field and jump directly to it. The form marks explicitly configured values and shows field diagnostics. The collapsible diagnostics pane opens on errors. Collections, Mihomo rules and node injections support filtered lists and item editing.

Use the source editor for native YAML or JSON. It provides line numbers, Find and undo/redo. The server parses and serializes the source. Invalid source remains in the local buffer until corrected. sing-box fields with contextual guidance provide the documented default where available and a link to the upstream configuration reference. The form follows the pinned 1.14.2 catalog. Navigation retains each service's editor in memory. Quitting prompts to save or discard unsaved drafts.

Every persistent write uses `expected_revision`. On a 409 conflict the editor retains local changes and shows a comparison with the latest revision. Diff cards are also available when reviewing publication and comparing published versions.

Publication validates all five projected target configurations and exercises the production renderer with empty and representative node sets. Publication atomically inserts a new encrypted version and changes the active pointer. Rollback revalidates historical content and publishes it as a **new** version. History is not rewritten. Default template archive requires an active published replacement of the same format and its revision. Archive retains drafts/history.

To export a usable configuration in the Mac app, open the user's subscription menu, choose a published template and target platform, then export the final YAML or JSON. Keep the file private because it contains connection material.

For API integrations, the template preview selects a real user, explicit platform and optional stable kernel version. It uses the download renderer, sends `no-store`, and redacts credentials by default. Results include origins, platform omissions, filtered-node reasons, selections and diagnostics.

## Dynamic nodes and import/export

Injection rules select only currently authorized, eligible, compatible nodes by IDs, display-name substring/prefix, exclusions, name sorting, and preferred prefix. A rule targets a final group name, independently of array position. Missing targets fail validation. Group membership may mix policy groups, built-in actions, and injected nodes. Output names include the complete stable node ID in the reserved `hx:` namespace. Duplicate display names cannot collide with groups or each other. Empty selection falls back to direct or deliberately returns 422 according to the rule. If authorized nodes exist but all are incompatible, the established 422 behavior remains.

Import analysis removes manual proxy nodes (including other protocols), reports names/types and removed external proxy providers without credentials, and maps group member references to authorized dynamic injection. Platform-specific fields remain in the single configuration and are projected during generation. Review diagnostics before accepting the local draft. Unresolved references and illegal fields block publication. A complete JSON template package contains the format, schema 2 document, common configuration, and injection rules. Native source edits update the single common configuration.

## Client compatibility

UA families recognized: `mihomo`, `clash.meta`, `clashmeta`; `sing-box`, `singbox`; `shadowrocket`, `v2rayn`, `v2rayng` (Base64). Token boundaries prevent accidental name matches. A wrapper app's version is not a kernel version. Unknown/prerelease kernel versions retain representable advanced nodes; known versions apply the existing capability ceilings.

Known sing-box kernels below 1.14.0 return 422, including empty subscriptions. Known ECH support uses the 1.14.2 validation profile. Older known Mihomo versions filter advanced mTLS/ECH/Gecko/Realm nodes. URI/Base64 omit advanced features not validated for their clients; Base64 is exactly the UTF-8 URI list, including its trailing newline. Partially compatible output reports `X-HysteriaX-Filtered-Nodes`; all-incompatible output returns `subscription_incompatible`.

mTLS sing-box nodes retain `disable_chrome_parrot: true`. Realm retains its independent HTTP/TLS settings and Mihomo `handshake-timeout: 30`. Bandwidth conversion to sing-box integral Mbps remains exact rather than rounded. Connection material always comes from successful deployed configuration, not pending desired changes.

## API and validation

The independent `hysteriax-template-validator` crate checks the fixed native schemas and prepared rules against their meta-schemas at build time and compiles validators with jsonschema 0.59.0. Ordinary server-code changes reuse that artifact. The server passes borrowed JSON values and receives field paths, then applies reference, platform and final-render checks.

`python3 tests/contracts/verify-subscription-schema-build-gate.py` proves corrupted raw and prepared rules fail the build without modifying repository sources, application-only edits reuse the same rlib, and valid Schema changes rebuild and change verdicts. `python3 tests/contracts/verify-subscription-validator-compatibility.py` compares the actual internal crate with the frozen 1,046-case verdict/path baseline in `tests/fixtures/template-validator`. `python3 tests/contracts/verify-subscription-schema-runtime.py --image <locally-built-image>` checks fresh Linux processes, both formats and five platforms, invalid configurations, repeated validation and four-worker concurrency. It enforces a 96 MiB process peak and at most 16 MiB growth after warming the same workload. The offline `hysteriax-server --check-template-runtime` diagnostic needs no database or secrets; it reports Linux RSS/peak/Swap without starting service workers.

Under `/api/v2/subscription-templates`: paginated list/create; `/{id}` metadata/patch; `/{id}/draft` read/save; `/{id}/validate`, `/preview`, `/publish`, `/rollback`; `/{id}/versions` and `/versions/{version}`; `/fields`, `/source`, `/import`, `/{id}/export`, and `/{id}/refactor` (cross-layer group/rule-set rename of a local document). See the OpenAPI specification for request/response DTOs. Body-bearing reads/previews are not cached. Audit events include successful and failed operation results, revisions, publication/source versions, and error codes; they never include document bodies or error messages containing configuration text.

Runtime validation operates on the configuration. Digest-pinned Mihomo and sing-box checks and the local SOCKS forwarding fixture run through `tests/integration/verify-subscription.py`. The [Mac regression index](../apps/macos/Tests/README.md) covers explicit subscription links, typed editing, import/export and publication workflows. `tests/integration/verify-subscription-template-lifecycle.py --container <disposable-postgres-container>` verifies process restart and whole-database restore in temporary databases.
