# Subscription templates and formats

## Upgrade contract

Server and Mac client must upgrade together. Migration **0037** creates encrypted drafts and immutable published versions, then initializes one published default template per native format using the master key. Existing users, node assignments, and subscription tokens are retained.

The only public subscription route is `GET /sub/{token}`. The old `/sub/{token}/clash.yaml` route returns **404**. Management responses return one canonical `url`; `auto_url` and Mac legacy fallbacks are removed. Replace saved legacy links with a template/platform link from the upgraded Mac app or selection page. This feature does not deploy a server or change deployed nodes.

## Addresses and qualification

Example: `/sub/{token}?template=default-mihomo&platform=windows`.

- `template` selects a global published template. Every new request reads its current published version. A draft save never changes subscribers. Archived or unpublished explicit templates return 404.
- Explicit `format=mihomo|singbox` must match a selected template. Without a template, explicit format or recognized UA selects that format's current default.
- `platform=linux|openwrt|macos|windows|android|ios` selects an overlay; OpenWrt aliases Linux. Explicit platform wins over UA. Omitted or `auto` uses OS detection; unknown OS shows the platform selection page. It never silently assumes Linux.
- Unknown client families show all active published templates and explicit platform URLs. Links retain the selected template and determine the format from that template.
- `format=uri|base64` produces authorized node links directly and needs no platform. Combining these formats with `template` returns 400.
- Invalid/repeated template, format, or platform parameters return 400.

Token revocation, user enablement/expiry/quota, effective authorization groups, deployed-node state, node package eligibility, and deployed mTLS trust continue to apply on every request. Templates cannot grant node access. Missing/revoked tokens return 404; inactive users return 403. Cache keys include user/node/credential state, template ID, published version, platform, and client capability. Default switches, publication, rollback, and archive are visible to new requests immediately.

Public responses use `Cache-Control: private, no-store`, `Vary: User-Agent`, `Referrer-Policy: no-referrer`, and `X-Robots-Tag: noindex, nofollow`. The selection page loads no external assets. Keep token-bearing addresses private.

## Configuration document

A version 1 document contains `common` (native configuration object), `platforms` (platform overlays), and independent `injections`. Each overlay has an object `set` and JSON-pointer `remove` array.

Objects merge recursively. Arrays replace as complete values. `false`, `0`, `null`, and empty arrays are values, never deletion markers. Removing an array item requires replacing the whole array. Set/remove conflicts are rejected. Common settings must be portable across the target set; baseline platform-specific listeners/TUN settings belong in explicit overlays. Runtime generation never filters invalid settings away.

The versioned field catalogs derive from Mihomo **v1.19.31** and sing-box **v1.14.2**. Rust validates types, enumerations, ranges, required properties, applicable baseline platform constraints, policy references, and outbound cycles. The same catalogs drive typed Mac controls for scalars, enums, nested objects, maps, unions, and ordered arrays. Connection fields are injected read-only. Unknown native fields survive roundtrips and generate warnings; their platform compatibility is unverified. YAML anchors/merge keys and JSON comments/trailing commas are normalized on the server. Original comments and whitespace are not retained. Native documents require finite JSON-compatible numeric values and string mapping keys; unsupported custom YAML tags are diagnosed rather than silently converted.

The shared `platform-capabilities.json` records explicit upstream documentation and the project's client-owned field profile. It includes process/interface/user matching, Linux socket and namespace options, Android package matching, graphical-client requirements, TUN routing dependencies, and the established adaptation: Mihomo redirect on Linux/macOS and TProxy on Linux; sing-box TProxy on Linux, redirect on Linux/macOS, TUN auto-redirect on Linux, fixed TUN interface names on Linux/Windows, and client-owned TUN routing on Android/iOS. This is a structural and baseline capability profile, not proof of runtime behavior on every OS or wrapper client. Unknown fields do not acquire compatibility guarantees from the catalog.

## Draft, publication, and rollback

The Mac editor offers format filtering, create/duplicate/rename/default/archive, common/platform selection, field search, typed controls, inheritance/set/removal, ordered arrays, source editing, undo/redo, and independent dynamic node selection rules. sing-box fields with contextual guidance expose an information button for their explanation or documented default, plus a configuration-documentation link. Links follow protocol, shared-field, and rule-action ownership; nested OpenVPN/TUN headings are located when available. The form follows the pinned 1.14.2 catalog; the linked online documentation may describe newer versions. Reading guidance or previewing defaults never changes the draft. Source parsing/serialization runs on the server. Invalid source stays in the local buffer and blocks replacing parsed form state. Navigation retains each service's editor in memory; quitting prompts to save or discard unsaved drafts.

Every persistent write uses `expected_revision`; stale requests return 409. The editor retains local changes, fetches a comparison copy, and offers an explicit choice to adopt the latest revision. It never silently retries an overwrite.

Publication validates all five merged target configurations and exercises the production renderer with empty and representative node sets. Publication atomically inserts a new encrypted version and changes the active pointer. Rollback revalidates historical content and publishes it as a **new** version. History is not rewritten. Default template archive requires an active published replacement of the same format and its revision. Archive retains drafts/history.

Preview selects a real user, explicit platform, and optional stable kernel version. It shares the download renderer, is `no-store`, and redacts credentials by default; an administrator may explicitly reveal them. Results include origins, overlay differences, filtered-node reasons, selection results, and layer/path diagnostics. Final native exports use the same pipeline. Redacted preview exports retain their redaction; choose full credentials explicitly when an operational file is required.

## Dynamic nodes and import/export

Injection rules select only currently authorized, eligible, compatible nodes by IDs, display-name substring/prefix, exclusions, name sorting, and preferred prefix. A rule targets a final group name, independently of array position. Missing targets fail validation. Group membership may mix policy groups, built-in actions, and injected nodes. Output names include the complete stable node ID in the reserved `hx:` namespace. Duplicate display names cannot collide with groups or each other. Empty selection falls back to direct or deliberately returns 422 according to the rule. If authorized nodes exist but all are incompatible, the established 422 behavior remains.

Import analysis removes manual proxy nodes (including other protocols), reports names/types and removed external proxy providers without credentials, maps group member references to authorized dynamic injection, and moves known platform-specific fields into explicit overlays. Rule arrays are replaced as complete units on each platform; a rule containing an unsupported OS predicate moves as a whole rather than having its matching conditions weakened. All relocations appear in the import diff. Review the mapping/diagnostics before accepting the local draft. Unresolved references and illegal fields block publication. A complete JSON template package contains format, document version, common configuration, every overlay, removals, and injection rules; importing the package preserves all of them. Native source edits affect only their selected layer.

## Client compatibility

UA families recognized: `mihomo`, `clash.meta`, `clashmeta`; `sing-box`, `singbox`; `shadowrocket`, `v2rayn`, `v2rayng` (Base64). Token boundaries prevent accidental name matches. A wrapper app's version is not a kernel version. Unknown/prerelease kernel versions retain representable advanced nodes; known versions apply the existing capability ceilings.

Known sing-box kernels below 1.14.0 return 422, including empty subscriptions. Known ECH support uses the 1.14.2 validation profile. Older known Mihomo versions filter advanced mTLS/ECH/Gecko/Realm nodes. URI/Base64 omit advanced features not validated for their clients; Base64 is exactly the UTF-8 URI list, including its trailing newline. Partially compatible output reports `X-HysteriaX-Filtered-Nodes`; all-incompatible output returns `subscription_incompatible`.

mTLS sing-box nodes retain `disable_chrome_parrot: true`. Realm retains its independent HTTP/TLS settings and Mihomo `handshake-timeout: 30`. Bandwidth conversion to sing-box integral Mbps remains exact rather than rounded. Connection material always comes from successful deployed configuration, not pending desired changes.

## API and validation

Fixed native schemas are checked against their meta-schemas during every relevant Cargo build, including container builds. The independent `hysteriax-template-validator` crate checks original files and actual prepared rules, then uses jsonschema 0.59.0's compile-time macro to generate validators. Ordinary server-code changes reuse that crate's compiled artifact; Schema, preparation, compiler, feature or dependency changes can correctly invalidate it. The server passes borrowed JSON values and receives field paths without constructing a runtime Schema validator. All user-configuration, reference, platform and final-render checks remain active. No endpoint accepts a replacement Schema. The former 0.33.0 patch has been removed; there is no active production patch.

`python3 scripts/verify-subscription-schema-build-gate.py` proves corrupted raw and prepared rules fail the build without modifying repository sources, application-only edits reuse the same rlib, and valid Schema changes rebuild and change verdicts. `python3 scripts/verify-subscription-validator-compatibility.py` compares the actual internal crate with the frozen 1,046-case verdict/path baseline in `tests/fixtures/template-validator`. `python3 scripts/verify-subscription-schema-runtime.py --image <locally-built-image>` checks fresh Linux processes, both formats and five platforms, invalid configurations, repeated validation and four-worker concurrency. It enforces a 96 MiB process peak and at most 16 MiB growth after warming the same workload. The offline `hysteriax-server --check-template-runtime` diagnostic needs no database or secrets; it reports Linux RSS/peak/Swap without starting service workers.

Under `/api/v2/subscription-templates`: paginated list/create; `/{id}` metadata/patch; `/{id}/draft` read/save; `/{id}/validate`, `/preview`, `/publish`, `/rollback`; `/{id}/versions` and `/versions/{version}`; `/fields`, `/source`, `/import`, `/{id}/export`, and `/{id}/refactor` (cross-layer group/rule-set rename of a local document). See the OpenAPI specification for request/response DTOs. Body-bearing reads/previews are not cached. Audit events include successful and failed operation results, revisions, publication/source versions, and error codes; they never include document bodies or error messages containing configuration text.

Runtime and publication do not spawn real kernels or fetch remote rule files. CI retains digest-pinned Mihomo/sing-box checks and a real local SOCKS forwarding fixture in `scripts/verify-subscription.sh`. These checks validate supported-platform examples and transport behavior; they do not prove every target's runtime compatibility. Rust tests exercise storage/publication/authorization/cache behavior; `scripts/verify-subscription-client.sh` checks explicit URL selection, HTML-export rejection, overlay semantics, and typed catalog coverage.


The Mac regression script `scripts/verify-subscription-template-ui.py` runs a uniquely identified temporary app against an isolated local service/database schema. It verifies native JSON/YAML file saves, complete-package reimport, explicit copy/download addresses, editing, platform overrides, publication and rollback. `scripts/verify-subscription-template-lifecycle.py --container <disposable-postgres-container>` verifies actual process restart and full pg_dump/pg_restore in dedicated temporary databases; it is also run in Linux CI.
