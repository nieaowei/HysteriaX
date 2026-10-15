# HysteriaX

HysteriaX is a self hosted Hysteria 2 node and user manager. The repository contains a Rust management API and a SwiftUI macOS client package. The service owns node configuration, user credentials, subscriptions, jobs, quota data, and audit history.

## Current implementation

- Rust workspace and Axum API backed by PostgreSQL 18.
- Encrypted at rest SSH secrets, Hysteria node tokens, user credentials, subscription tokens, configuration snapshots, and uploaded config resources.
- Cloudflare DNS record management with an independent macOS page, automatic/manual node domain allocation, encrypted credential reuse, durable writes and published connection snapshots. See [DNS management](docs/dns.md).
- Unified credential center with encrypted SSH authentication, versioned TLS identities/CA/ECH/DNS/API secrets, cross-node reuse, automatic application batches, expiry reminders, and business-token lifecycle views.
- Bearer token authentication with one time token creation and revocation; the server protects the current and last active administrator token.
- Revision guarded node and user updates; configuration updates and sync job creation share a database transaction.
- Node and user CRUD, authorization groups, credential rotation, quota reset, Hysteria HTTP authentication, Mihomo YAML subscription generation, persisted job records, resumable SSE events, SSH job execution, and periodic quota sampling.
- Docker Compose and Caddy deployment files.
- The macOS client supports English and Simplified Chinese. Language follows the system by default (Chinese variants use Simplified Chinese; other languages use English). Settings → Language lets you select Follow system, English or 中文; the preference is saved and UI text updates immediately.
- SwiftUI management screens for overview, nodes, users, jobs, audit, and Keychain backed service settings. Node and user details show sample freshness, open statistics gaps, and pending revocations. Settings supports one-time administrator-token rotation and revocation while protecting the token used by the current Mac. Per-service display snapshots remain visible after a restart while offline; detail reads and writes require a live connection.

The deployment worker pins the Hysteria 2 release and checks its asset digest before SFTP transfer. Disposable Debian/Ubuntu systemd containers have verified deployment through root-key and sudo key/password access, TCP/UDP proxy traffic, two-node user isolation, aggregate quota enforcement, and real Hysteria/Mihomo v1.19.31 transfers with mTLS, ECH, Gecko, and port hopping enabled. Native amd64 and arm64 GitHub Actions matrices passed for all supported distro images. Independent Debian cloud VMs passed three-user node isolation, two-node subscription/proxy/usage sampling, disable/expiry/over-quota revocation, and credential rotation. Server1 passed clean first install, unmanaged-directory refusal, and first-install UDP-occupancy rejection followed by successful retry; Server2 passed SSH, listener rollback, and quota-recovery probes. Realm uses a 30-second Mihomo handshake-timeout and passes a pinned v1.19.31 delayed-STUN live transfer test; Mimic remains blocked without a Mihomo mapping. The locally built `linux/amd64` server image is published as [`nieaowei/hysteriax-server:authorization-groups-20261007-190149`](https://hub.docker.com/r/nieaowei/hysteriax-server), digest `sha256:cd93784f81e1d0d08ac622017ae1d89376c0f0d0dcdb0251db9c2930a6535937`. See [the implementation status](docs/implementation-status.md) before using this as a production control plane.

## Quick start

Requirements: Rust 1.89 or newer for local development, or Docker Compose for a server deployment. Configure DNS to point the management hostname at the server before starting Caddy.

```sh
scripts/init-secrets.sh manage.example.com
docker compose up -d --build
```

Administrator, encryption, and PostgreSQL credentials are written to `.env` with mode 600. Store the administrator token in a password manager. Back up `HYSTERIAX_MASTER_KEY` separately from the database; encrypted secrets cannot be recovered without it.

Existing SQLite installations start with an empty PostgreSQL database and are not imported automatically. See the [installation guide](docs/installation.md#existing-sqlite-installations) before switching an existing service.

Check readiness:

```sh
curl --fail https://manage.example.com/readyz
```

Management endpoints are under `/api/v1` and require `Authorization: Bearer …`. The OpenAPI document is available at `/openapi.yaml`. A local API process can be started with `scripts/run.sh` after setting `DATABASE_URL`, `HYSTERIAX_ADMIN_TOKEN`, and `HYSTERIAX_MASTER_KEY`.

Collection endpoints `/api/v1/nodes`, `/api/v1/users`, `/api/v1/dns/records`, and `/api/v1/credentials` always return `{ items, total, page, page_size }`. They default to page 1 with 50 items; `page_size` is limited to 200. Search, sorting, and filters apply with or without explicit paging parameters. For example: `/api/v1/users?page=2&page_size=50&q=Alice`. The former `/page` routes and full-array response mode have been removed. Callers that need an entire directory must traverse the pages.

The management API remains at `/api/v1` with version `1.0.0`; response changes are applied directly without compatibility with older clients. Node lists return summaries; fetch `/api/v1/nodes/{id}` for configuration and redacted YAML. `/api/v1/version` includes `directory_generation`, which clients use to reload configuration directories only after a committed configuration change. Active lists, detail panels, and overview monitoring still refresh independently.

Backend response caches are bounded and private, keyed by administrator, normalized query, and committed dependency versions. Configuration directories, runtime state, traffic, online/probe history, jobs and audit have separate versions. Overview responses have a 3-second TTL, history 60 seconds (also partitioned by UTC minute), active jobs 1 second, configuration directories 60 seconds, and terminal job/batch contents 5 minutes with monotonic object versions and dependency digests. `Cache-Control: no-cache` forces a new computation; `/api/v1/cache` reports aggregate and normalized-route computation/reuse/latency counters. Authorization, write validation, subscription eligibility and database readiness always read current state.

Docker Compose runs a private, password-protected Redis service (64 MiB, allkeys-lru, AOF persistence). Responses, directory snapshots, redacted previews and rendered subscriptions use Redis as their primary cache. Values are encrypted with the service master key and keys are hashed and partitioned by database, cache kind, identity and source generation. Redis failures fall back to the database with a bounded two-second memory cache and circuit breaker. Redis I/O is limited to 150 ms. Standalone processes configure `HYSTERIAX_REDIS_URL`; without it they use memory caches. Existing Compose installations must add a random hex `HYSTERIAX_REDIS_PASSWORD` to their protected `.env`; `scripts/init-secrets.sh` generates it for new installations. Redis has no published host port. See [plan 0027](docs/plans/0027_redis_cache_plan.md).

History accepts `metrics=traffic|quality|all`. Its `generated_at` is rounded down to the current UTC minute; rolling windows end at that timestamp. Traffic uses hour/minute aggregates and probe counts use minute aggregates, online statistics use weighted minute aggregates with cycle fallback, and P50/P95 remain exact calculations over raw latency. Migrations 0016–0024 backfill and maintain derived data, cache dependency/object versions, hourly traffic, minute online metrics and task/retry-chain read models; billing counters keep their existing transaction semantics. Probe and online history is retained for 31 days.

Pinned installation binaries are verified by SHA-256 and cached atomically on disk (300 MiB budget). Set `HYSTERIAX_ASSET_CACHE_DIR` to a persistent writable directory to reuse downloads across restarts; otherwise the process uses its temporary directory. Docker Compose mounts `./data/assets` as a persistent cache; ensure that directory is writable by the API container UID/GID. Further dependency splitting and rollups are recorded in [plan 0028](docs/plans/0028_cache_domains_history_rollups_plan.md). Original implementation details and validation are recorded in [plan 0026](docs/plans/0026_backend_cache_preaggregation_plan.md).


## Client-aware subscriptions

The macOS subscription menu copies the automatic URL by default. The same
`/sub/{token}` URL returns Mihomo YAML, sing-box JSON, or Base64 Hysteria2 links
according to the client's User-Agent. Browsers and unknown clients receive a
format selection page with copyable links. Use `?format=mihomo`, `singbox`,
`base64`, or `uri` to select a format explicitly. Existing
`/sub/{token}/clash.yaml` subscriptions remain fixed Mihomo YAML and use the same token.

The macOS menu also copies individual format URLs and exports `.yaml`, `.json`,
or `.txt` files. sing-box exports provide a localhost mixed proxy on port 7890
and a node selector, without TUN or remote rule sets. Older servers remain usable
for automatic-link copying and Mihomo exports; additional formats require a
server upgrade. Deploy the server before updating the macOS client.

See [subscription formats and compatibility](docs/subscriptions.md) for version
requirements and filtering behavior.

## Node packages

Nodes support expiry, whole-host network quotas, monthly or fixed billing cycles, advance warnings, and automatic proxy restriction/recovery. See [node package configuration and limitations](docs/node-packages.md). User access is managed through [authorization groups](docs/authorization-groups.md). System notifications can be enabled in the macOS settings.

## Development

Start the PostgreSQL service for local tests with `docker compose --env-file .env -f compose.yaml -f compose.test.yaml up -d postgres`. Load the generated database password into the shell before setting `TEST_DATABASE_URL`:

```sh
set -a
. ./.env
set +a
export TEST_DATABASE_URL="postgresql://postgres:${HYSTERIAX_DB_ADMIN_PASSWORD}@127.0.0.1:${HYSTERIAX_DB_HOST_PORT:-55432}/hysteriax"
python3 -m pip install -r scripts/requirements-test.txt
```

```sh
cargo fmt --all
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
xcodebuild -project apps/macos/HysteriaX.xcodeproj -scheme HysteriaX -configuration Release -sdk macosx -destination 'generic/platform=macOS' build CODE_SIGNING_ALLOWED=NO
scripts/verify-mihomo-config.sh tests/fixtures/mihomo-hysteria2.yaml
scripts/verify-subscription.sh
scripts/verify-subscription-client.sh
scripts/verify-node-package-client.sh
scripts/verify-restart-recovery.py
scripts/verify-sampling-failure.py
scripts/backup-restore.py backup
scripts/verify-auth-isolation.py
scripts/verify-deployment-matrix.py --only debian12-arm64 --only debian13-arm64 --only ubuntu2204-arm64 --only ubuntu2404-arm64
scripts/verify-mtls-live.py
scripts/verify-live-macos-ui.py
ruby scripts/generate-swift-api-models.rb
```

`scripts/verify-live-macos-ui.py` requires XcodeGen and a local `.env` pointing to a ready service with two deployed nodes. It creates a temporary XCUITest harness, uses in-memory token storage for that test build, and removes its temporary user; it does not access Keychain.

`openapi/openapi.yaml` is the API contract source. The macOS request/response DTOs and typed operation definitions, including request/response types, HTTP methods, paths, and required query parameters, are generated from its schemas and operations; the API client enforces those operation bindings at compile time. CI runs `ruby scripts/generate-swift-api-models.rb --check` to detect drift. The dynamic JSON value codec and generic HTTP transport are shared handwritten components. The app is available as both an Xcode project and a Swift package. Run `./script/build_and_run.sh --verify` for the local app build and launch check. Pushing a `vX.Y.Z` tag builds an unsigned universal DMG for direct distribution through the GitHub release; this does not require Apple signing or notarization credentials and does not publish to the Mac App Store. See the [release guide](docs/release.md) for the Gatekeeper tradeoff.

See [credential management](docs/credentials.md), [architecture](docs/architecture.md), [installation](docs/installation.md), [field coverage](docs/field-coverage.md), [compatibility profile](docs/compatibility.md), [deployment testing](docs/deployment-testing.md), [backup and recovery](docs/backup-restore.md), and [troubleshooting](docs/troubleshooting.md).

后端查询优化及部署验证见 [0031 查询优化计划](docs/plans/0031_query_optimization_plan.md)：后台按待处理范围重建、任务/审计索引化搜索和排序、事务性审计计数，以及数据库分页后的凭据摘要加载。
