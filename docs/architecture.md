# Architecture

## Components

- `crates/hysteriax-server` is the authoritative management service. It exposes the HTTPS API, validates writes, stores versioned configuration and credentials, performs Hysteria HTTP authentication, renders Mihomo subscriptions, and persists jobs and events.
- `apps/macos` is the SwiftUI management client. It stores the service URL and administrator token in the macOS Keychain and treats server state as authoritative.
- PostgreSQL 18 is the service's only database. Compose stores it and Caddy state in separate persistent volumes.
- Managed nodes run the pinned Hysteria 2 binary and request user authorization from `/hy2/auth/{node_id}/{node_token}`.

## Storage and security

PostgreSQL migrations are embedded in the service binary and run at startup. The schema uses `BOOLEAN`, `TIMESTAMPTZ`, `JSONB`, and `BIGINT`; encrypted secrets remain ciphertext text. Each configuration edit carries an `expected_revision`; stale writes return HTTP 409. A node configuration update and its queued sync job are committed in the same transaction. Transactional advisory locks preserve serialized write and event-cursor ordering. A session advisory lock rejects a second service instance using the same database.

The service requires a 32-byte base64 master key. XChaCha20-Poly1305 encrypts SSH authentication material, node and user credentials, subscription tokens, configuration snapshots, and node resources. SHA-256 digests are used to validate high entropy bearer credentials. The master key is read from the process environment and is not written into PostgreSQL.

Administrator tokens are random, rotatable, and stored as digests. Newly created administrator tokens, node tokens, user credentials, and subscription URLs are returned once by their corresponding write operations. Request bodies and API errors do not log credential fields.

## Long running work

Jobs, ordered events, and redacted phase logs are stored in PostgreSQL. The job worker serializes work per node, recovers jobs left running after a process restart, and applies new desired revisions after a running operation finishes. Deployment, sync, rollback, uninstall, SSH test, and credential revocation execute outside the macOS client. Deploy, sync, and rollback validate the installed service and run the pinned Hysteria client probe before reporting success. Clients read persisted job state and reconnect to the SSE stream using the last event id (`?after=<id>`).

The traffic worker samples each deployed node over SSH every ten seconds, updates node freshness even when no user traffic is present, and transactionally records traffic baselines, deltas, and sampling gaps. It schedules expiry and quota revocations across assigned nodes; kick jobs keep checking online sessions until they are gone or report failure.

## Code layout

- `api/` contains versioned HTTP routes for nodes, users, subscriptions, resources, jobs, audit, and health.
- `config.rs` validates fixed-version Hysteria server options and renders managed node configuration.
- `jobs.rs` claims and recovers queued work and persists job transitions; `deployment.rs` performs remote install, sync, rollback, health checks, and the client probe.
- `ssh.rs` implements host-key-verified SSH/SFTP operations.
- `traffic.rs` samples usage, records per-node baselines and data gaps, and queues access revocations.
- `security/` contains secret encryption and token hashing; `state.rs` owns the service database pool and runtime settings.
- `db.rs` owns PostgreSQL connection settings, advisory locks, test-schema setup, and embedded migrations; `migrations/` defines the fresh PostgreSQL schema.

## API contract

`openapi/openapi.yaml` documents the versioned management API. The API returns JSON and UTC RFC 3339 timestamps. Public endpoints are restricted to health checks, subscription downloads, and the Hysteria HTTP auth callback. The subscription endpoint disables caching and returns 404 for an unknown or revoked token, and 403 for a disabled, expired, or over quota user.
