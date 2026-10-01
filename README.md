# HysteriaX

HysteriaX is a self hosted Hysteria 2 node and user manager. The repository contains a Rust management API and a SwiftUI macOS client package. The service owns node configuration, user credentials, subscriptions, jobs, quota data, and audit history.

## Current implementation

- Rust workspace and Axum API with SQLite WAL migrations.
- Encrypted at rest SSH secrets, Hysteria node tokens, user credentials, subscription tokens, configuration snapshots, and uploaded config resources.
- Bearer token authentication with one time token creation and revocation.
- Revision guarded node and user updates; configuration updates and sync job creation share a database transaction.
- Node and user CRUD, assignments, credential rotation, quota reset, Hysteria HTTP authentication, Mihomo YAML subscription generation, persisted job records, resumable SSE events, SSH job execution, and periodic quota sampling.
- Docker Compose and Caddy deployment files.
- SwiftUI management screens for overview, nodes, users, jobs, audit, and Keychain backed service settings. Node and user details show sample freshness, open statistics gaps, and pending revocations. Settings supports one-time administrator-token rotation and revocation while protecting the token used by the current Mac. Per-service display snapshots remain visible after a restart while offline; detail reads and writes require a live connection.

The deployment worker pins the Hysteria 2 release and checks its asset digest before SFTP transfer. Disposable Debian/Ubuntu arm64 systemd containers have verified deployment through root-key and sudo key/password access, TCP/UDP proxy traffic, two-node user isolation, aggregate quota enforcement, and real Hysteria/Mihomo v1.19.31 TCP transfers with mTLS, ECH, Gecko, and port hopping enabled. Native amd64 and arm64 GitHub Actions matrices have passed for all supported distro images. Independent cloud-VM failure coverage, Realm compatibility, and Apple credential-backed notarized DMG publishing remain open. See [the implementation status](docs/implementation-status.md) before using this as a production control plane.

## Quick start

Requirements: Rust 1.89 or newer for local development, or Docker Compose for a server deployment. Configure DNS to point the management hostname at the server before starting Caddy.

```sh
scripts/init-secrets.sh manage.example.com
docker compose up -d --build
```

The administrator token and encryption key are written to `.env` with mode 600. Store the administrator token in a password manager. Back up `HYSTERIAX_MASTER_KEY` separately from the database; encrypted secrets cannot be recovered without it.

Check readiness:

```sh
curl --fail https://manage.example.com/readyz
```

Management endpoints are under `/api/v1` and require `Authorization: Bearer …`. The OpenAPI document is available at `/openapi.yaml`. A local API process can be started with `scripts/run.sh` after setting `DATABASE_URL`, `HYSTERIAX_ADMIN_TOKEN`, and `HYSTERIAX_MASTER_KEY`.

## Development

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
ruby scripts/generate-swift-api-models.rb
```

`openapi/openapi.yaml` is the API contract source. The macOS request/response DTOs and typed operation definitions, including request/response types, HTTP methods, paths, and required query parameters, are generated from its schemas and operations; the API client enforces those operation bindings at compile time. CI runs `ruby scripts/generate-swift-api-models.rb --check` to detect drift. The dynamic JSON value codec and generic HTTP transport are shared handwritten components. The app is available as both an Xcode project and a Swift package. Run `./script/build_and_run.sh --verify` for the local app build and launch check. Pushing a `vX.Y.Z` tag starts the Developer ID signing and notarization workflow for a DMG distributed directly through the GitHub release, which requires the Apple secrets documented in [the release guide](docs/release.md); it does not publish to the Mac App Store.

See [architecture](docs/architecture.md), [installation](docs/installation.md), [field coverage](docs/field-coverage.md), [compatibility profile](docs/compatibility.md), [deployment testing](docs/deployment-testing.md), [backup and recovery](docs/backup-restore.md), and [troubleshooting](docs/troubleshooting.md).
