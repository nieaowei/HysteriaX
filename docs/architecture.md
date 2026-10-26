# Architecture

## Components and storage

- `crates/hysteriax-server` is the authoritative Rust management service. It validates configuration, encrypts credentials, serves subscriptions and node authentication, samples usage over SSH, and executes durable jobs.
- `apps/macos` is the SwiftUI management client. It stores the administrator token in Keychain and reads authoritative server state; the service continues working while the client is closed.
- PostgreSQL 18 stores business state, jobs, execution journals, events, resource leases and cache versions. Redis accelerates caches and rankings; Redis failure permits database fallback.
- Managed Linux nodes run the pinned Hysteria binary and call `/hy2/auth/{node_id}/{node_token}`. The separate node-local ACME component maintains server certificates.
- Caddy terminates management HTTPS and discovers API replicas through Docker DNS. The bundled Compose topology can run on one host; production may place PostgreSQL, Redis and APIs on separate hosts.

## Maintenance and security

Embedded SQLx migrations and one-time initialization run only through `--migrate-only`, with all ordinary API replicas stopped. Ordinary startup checks migration state, management protocol and the shared master-key fingerprint before starting workers. SQLx migration mutual exclusion remains; there is no application-wide write lock or single-instance runtime lock.

`HYSTERIAX_MASTER_KEY` is a base64-encoded 32-byte key held outside PostgreSQL. XChaCha20-Poly1305 protects stored secrets and snapshots; high-entropy bearer credentials are stored as SHA-256 digests. Secret-bearing write receipts are returned once or replayed through encrypted idempotency records. Credential versions are immutable and configuration snapshots pin exact versions. See [credentials](credentials.md) and [backup and recovery](backup-restore.md).

## Transactions, workers and caches

Cross-entity management changes use serializable database transactions. Pure database transactions retry serialization failures and deadlocks at most five times; SSH and DNS side effects stay outside retry closures. Stale revisions return 409 and exhausted transient retries return 503. Resource row locks, constraints and a consistent lock order coordinate related changes.

Each process has a unique instance ID and a database heartbeat. Workers claim resource-scoped work with database-time leases and owner/generation fencing. Leases last 60 seconds and renew every 10 seconds. Startup does not reset other instances' running work. Database-only work resumes after lease expiry; external steps persist an operation intent before dispatch. Uncertain external outcomes enter `reconciling` or `needs_attention` and keep the resource blocked until verification succeeds.

File uploads use SSH stdin under the same `ssh-journal-v2` protocol as commands. Upload intents bind the SSH endpoint, destination, byte count, SHA256 and remote predecessor; payload bytes are never stored in the intent or command arguments. The receiver writes a private temporary file, checks its length and digest, and atomically replaces the destination. A bounded receiver holds the resource lock through completion, including after transport loss. Uploads use the independent `HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS` budget (180 seconds by default, range 1–3600), with a further five seconds for the remote deadline and receipt to settle. The global operation timeout does not override uploads. The SSH transport inactivity deadline is at least the upload budget; ordinary operations retain their own deadlines.

Release staging can reuse `/opt/hysteriax/hysteria` when it is a regular, non-symlink file with the pinned release digest. It hashes both the source and staged copy before atomically publishing the staging file under the normal command journal. Missing or different binaries use the independent upload budget. The SSH client prefers X25519 for interoperability and retains the other supported exchanges as fallbacks; pinned host-key verification remains mandatory.

The job detail's remote operation verification can recover a journaled `ssh-upload` using its exact completion receipt and a newer receiver-side fence. Only after all uncertain operations are verified does it release the resource and associated expired job leases. Verification records completion, including a failed transfer; it does not mark the original job successful or resend its payload. A short transfer preserves the previous destination. Missing receipts, changed endpoints and historical SFTP uploads without journal metadata remain blocked; their outcome cannot be inferred from a file hash or by clearing `blocked` alone.

Traffic sampling uses stable captured batches, applies at most 500 user items per transaction, and atomically records progress with billing. Versioned dirty queues retain refresh work until its result and acknowledgement commit together. Durable node removal uses bounded history deletion and a persisted cursor. Revocations remain owed until confirmed rather than relying on process-local notifications.

Cache validity comes from committed entity versions and per-domain vectors of 64 bucket versions. Fill operations read one consistent snapshot and respect time-dependent expiry. Notifications accelerate invalidation; they are not authoritative. Redis ranking versions and deletion tombstones reject stale updates. Authorization and quota decisions read authoritative state.

## API and event contract

Management API 2.0.0 uses `/api/v2` with Bearer administrator authentication and stable `Idempotency-Key` headers for writes. Subscription and node authentication URLs retain their contracts. The source contract is [OpenAPI](../openapi/openapi.yaml); JSON timestamps use UTC RFC 3339.

Transactional source events are published with a separate publication sequence in batches of at most 256. SSE cursors use `v2:<epoch>:<publication_seq>`. Clients fetch the stream head before loading state, reconnect to any replica, deduplicate events, and save the cursor only after handling an event. An expired cursor returns 410 and triggers full resynchronization.

`DELETE /api/v2/nodes/{id}` performs managed remote uninstall. `DELETE /api/v2/nodes/{id}/record` removes the management record without SSH and checks revisions and conflicting work. Small removals return 204; more than 1,000 history rows queue a durable `record-remove` job and return 202. The node remains `deleting` while progress commits in short transactions. Shared users and credentials remain, as do historical task identifiers and audit evidence. Record-only removal does not stop the remote service and cannot bypass unresolved managed-trust obligations.

Subscription templates have encrypted drafts, immutable publications and explicit platform overlays. Maintenance seeds defaults once; built-in files are initialization resources. Public generation reads a published snapshot, resolves the platform, filters authorized nodes, applies overlays and injection rules, then validates and serializes the result. URI/Base64 remain direct node exports. Runtime never invokes proxy kernels; pinned kernels are verification tools. See [subscriptions](subscriptions.md).

## Code layout and verification

| Area | Responsibility |
| --- | --- |
| `api/`, `openapi/openapi.yaml` | HTTP routes, contracts, templates and idempotent writes. |
| `db.rs`, `bootstrap.rs`, `migrations/` | Transactions, maintenance, runtime compatibility and schema history. |
| `execution.rs`, `background.rs`, `jobs.rs` | Leases, fencing, recovery and job execution. |
| `deployment.rs`, `ssh.rs`, `credentials/` | Verified external operations and credential lifecycle. |
| `traffic/`, `analytics/`, `node_limits.rs` | Captured billing, refresh queues and machine quotas. |
| `cache.rs`, `cache_domains.rs`, `quota_rank.rs`, `events.rs` | Versioned caches, rankings and event publication. |
| `apps/macos/Sources/HysteriaX/` | Client models, transport, stores and views. |

See the [verification guide](deployment-testing.md) for reproducible checks and their isolation requirements. Host capacity and replica counts are deployment decisions, separate from functional acceptance.
