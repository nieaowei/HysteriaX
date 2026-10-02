# HysteriaX

HysteriaX is a self hosted Hysteria 2 node and user manager. The repository contains a Rust management API and a SwiftUI macOS client package. The service owns node configuration, user credentials, subscriptions, jobs, quota data, and audit history.

## Current implementation

- Rust workspace and Axum API backed by PostgreSQL 18.
- Encrypted at rest SSH secrets, Hysteria node tokens, user credentials, subscription tokens, configuration snapshots, and uploaded config resources.
- Bearer token authentication with one time token creation and revocation.
- Revision guarded node and user updates; configuration updates and sync job creation share a database transaction.
- Node and user CRUD, assignments, credential rotation, quota reset, Hysteria HTTP authentication, Mihomo YAML subscription generation, persisted job records, resumable SSE events, SSH job execution, and periodic quota sampling.
- Docker Compose and Caddy deployment files.
- SwiftUI management screens for overview, nodes, users, jobs, audit, and Keychain backed service settings. Node and user details show sample freshness, open statistics gaps, and pending revocations. Settings supports one-time administrator-token rotation and revocation while protecting the token used by the current Mac. Per-service display snapshots remain visible after a restart while offline; detail reads and writes require a live connection.

The deployment worker pins the Hysteria 2 release and checks its asset digest before SFTP transfer. Disposable Debian/Ubuntu systemd containers have verified deployment through root-key and sudo key/password access, TCP/UDP proxy traffic, two-node user isolation, aggregate quota enforcement, and real Hysteria/Mihomo v1.19.31 transfers with mTLS, ECH, Gecko, and port hopping enabled. Native amd64 and arm64 GitHub Actions matrices passed for all supported distro images. Independent Debian cloud VMs passed three-user node isolation, two-node subscription/proxy/usage sampling, disable/expiry/over-quota revocation, and credential rotation. Server1 passed clean first install, unmanaged-directory refusal, and first-install UDP-occupancy rejection followed by successful retry; Server2 passed SSH, listener rollback, and quota-recovery probes. Realm uses a 30-second Mihomo handshake-timeout and passes a pinned v1.19.31 delayed-STUN live transfer test; Mimic remains blocked without a Mihomo mapping. The locally built `linux/amd64` server image is published as [`nieaowei/hysteriax-server:dev`](https://hub.docker.com/r/nieaowei/hysteriax-server), digest `sha256:efe4bcef2c1eb1d0196e0459f6a0cc776a7123ad7e4c19f484c0ea0ff4ad686a`. See [the implementation status](docs/implementation-status.md) before using this as a production control plane.

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

See [architecture](docs/architecture.md), [installation](docs/installation.md), [field coverage](docs/field-coverage.md), [compatibility profile](docs/compatibility.md), [deployment testing](docs/deployment-testing.md), [backup and recovery](docs/backup-restore.md), and [troubleshooting](docs/troubleshooting.md).
