# Architecture

## Components

- `crates/hysteriax-server` is the authoritative management service. It exposes the HTTPS API, validates writes, stores versioned configuration and credentials, performs Hysteria HTTP authentication, renders Mihomo subscriptions, and persists jobs and events.
- `apps/macos` is the SwiftUI management client. It stores the service URL and administrator token in the macOS Keychain and treats server state as authoritative.
- PostgreSQL 18 is the service's only database. Compose stores it and Caddy state in separate persistent volumes.
- Managed nodes run the pinned Hysteria 2 binary and request user authorization from `/hy2/auth/{node_id}/{node_token}`.

## Storage and security

PostgreSQL migrations are embedded in the service binary and run at startup. The schema uses `BOOLEAN`, `TIMESTAMPTZ`, `JSONB`, and `BIGINT`; encrypted secrets remain ciphertext text. Each configuration edit carries an `expected_revision`; stale writes return HTTP 409. A node configuration update and its queued sync job are committed in the same transaction. Transactional advisory locks preserve serialized write and event-cursor ordering. A session advisory lock rejects a second service instance using the same database.

Management API contract 1.0.0 uses `/api/v1`; The v1 contract is changed in place; clients and server must upgrade together. Subscriptions, health endpoints and Hysteria authentication callbacks keep their public URLs.

The service requires a 32-byte base64 master key. XChaCha20-Poly1305 encrypts SSH authentication material, node and user credentials, subscription tokens, configuration snapshots, and node resources. SHA-256 digests are used to validate high entropy bearer credentials. The master key is read from the process environment and is not written into PostgreSQL.

Administrator tokens are random, rotatable, and stored as digests. Newly created administrator tokens, node tokens, user credentials, and subscription URLs are returned once by their corresponding write operations. Request bodies and API errors do not log credential fields.

Credential objects have immutable encrypted versions. Node configuration and deployment snapshots reference exact versions; SSH uses the currently verified binding. Publication creates durable per-node batches and queues automatic application. Credential application commits its completion marker with the binding or follow-up job, so restart recovery cannot apply the same change twice. User mTLS identities remain user-owned. Public certificate metadata is readable; secret material is not returned after import. See [credentials](credentials.md).

## Long running work

Jobs, ordered events, and redacted phase logs are stored in PostgreSQL. The job worker serializes work per node, recovers jobs left running after a process restart, and applies new desired revisions after a running operation finishes. Deployment, sync, rollback, uninstall, SSH test, and credential revocation execute outside the macOS client. Deploy, sync, and rollback validate the installed service and run the pinned Hysteria client probe before reporting success. Clients read persisted job state and reconnect to the SSE stream using the last event id (`?after=<id>`).

The traffic worker samples each deployed node over SSH every ten seconds, updates node freshness even when no user traffic is present, and transactionally records traffic baselines, deltas, and sampling gaps. It schedules expiry and quota revocations across assigned nodes. Durable `kick_requests` merge reasons per node/user and survive user deletion. Each execution is limited to five attempts with 30/60/120/300-second retry delays. Exhausted SSH transport failures wait for an observed trusted SSH connection before a linked execution is queued; permanent or other exhausted errors require explicit retry. Renewals and quota resets clear conditional reasons while credential revocations, unassignments and deletions remain owed until the node confirms the user is offline. Request generations prevent a running job from acknowledging a newer revocation, and jobs are ordered by availability time to release the queue for SSH tests and repairs.

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


Node removal has two explicit operations. `DELETE /api/v1/nodes/{id}` performs managed remote uninstall before removing an installed node. `DELETE /api/v1/nodes/{id}/record` removes only the management record without SSH, after checking the optimistic revision and rejecting running remote deployment/uninstall/credential changes. It cancels other active jobs, signals their local workers, removes dependent assignments/configuration/monitoring/revocation requests, and retains historical job identifiers, names, events and an explicit `node.record_removed` audit. Shared credentials and users remain. The remote service may still be running; client capability `node_record_removal` gates the separate operation.
