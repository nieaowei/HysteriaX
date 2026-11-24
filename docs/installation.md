# Installation

## Management service with Docker Compose

1. Point a DNS A/AAAA record for the management hostname at the server.
2. Install Docker Engine and the Docker Compose plugin.
3. From the project directory, run `scripts/ops/init-secrets.sh manage.example.com`. This creates a private `.env` with the administrator token, encryption key, and separate PostgreSQL administrator and application passwords.
4. Save the administrator token and make a separate protected backup of `HYSTERIAX_MASTER_KEY`.
5. Build the image on your development machine for the server's architecture and transfer it to the server using the commands below. Start dependencies with `docker compose up -d --wait postgres redis`, then initialize the database with `docker compose --profile maintenance run --pull never --rm --no-deps migrate`. Start the service with `docker compose up -d --no-build api caddy`.
6. Confirm `https://manage.example.com/readyz` returns `{"status":"ready","database":"ok","management_protocol":2}`.

Build locally, using a fixed release tag in place of `dev` for a release:

```sh
docker buildx build --platform linux/amd64 --load -t hysteriax-server:dev .
docker save hysteriax-server:dev | gzip > hysteriax-server.tar.gz
scp hysteriax-server.tar.gz user@server:/tmp/hysteriax-server.tar.gz
```

Use `linux/arm64` for an arm64 server. On the server, load the image with `gzip -dc /tmp/hysteriax-server.tar.gz | docker load`. Keep `HYSTERIAX_IMAGE` and `HYSTERIAX_VERSION` in `.env` consistent with the loaded tag. Keep the Compose files and mounted deployment scripts from the same source revision on both machines. Do not build source on the deployment host.

Compose starts PostgreSQL 18 with a persistent volume and a non-superuser application role. The database listens only on the Compose network and is not published to the host. Caddy obtains and renews the management HTTPS certificate; TCP 80 and 443 must reach the host. Node Hysteria traffic uses the configured UDP ports. This service does not alter cloud security groups or host firewalls.

Tagged images published by the release workflow can also be selected with `HYSTERIAX_IMAGE=ghcr.io/<owner>/hysteriax-server` and `HYSTERIAX_VERSION=vX.Y.Z` in `.env`. Load or pull the selected image before maintenance; existing installations must follow the [database baseline requirements](#database-baseline). Caddy state and PostgreSQL data use separate named volumes. Keep `.env`, database backups, `HYSTERIAX_MASTER_KEY`, and Caddy state in protected backups. Never commit `.env` or production data.

After checking the host's memory and PostgreSQL connection budget, multiple API replicas can share that database, Redis and master key. For example, `docker compose up -d --no-build --scale api=2 api caddy` starts two replicas without building on the deployment host. The Caddy configuration discovers the `api` Docker DNS addresses every five seconds and selects them with round-robin load balancing. Passive failure tracking and a one-second dial timeout leave room to try a healthy replica within the three-second retry window; Caddy's default retry rules still apply. No sticky session is required. See the [Caddy reverse proxy reference](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy) and run `python3 tests/integration/verify-caddy-replicas.py --report /tmp/caddy-replicas.json` for the isolated proxy check. That check uses lightweight HTTP backends and does not replace the full HysteriaX business acceptance or host capacity measurements.

Budget each production host for the services actually scheduled there. The bundled Compose stack is a convenient single-host topology, not a requirement that PostgreSQL, Redis and API replicas share one machine. The server accepts separate `DATABASE_URL` and `HYSTERIAX_REDIS_URL` endpoints; supply the external addresses in the production service configuration. Account for API process memory on API hosts, PostgreSQL connections and query memory on database hosts, and Redis memory on its host. Do not compare the sum of separate hosts' service limits with an API host's RAM.

For local Docker acceptance with a PostgreSQL dump from this baseline, use `python3 tests/performance/verify-snapshot-capacity.py --help` and `--replicas 3`. Build its test-only network helper from `tests/fixtures/runtime/egress-guard.Dockerfile` first. The verifier restores into separate owned containers, installs an outbound allowlist before releasing each API startup gate, and exercises real API replicas through Caddy with Redis available, stopped, and restarted empty. It records cgroup memory, OOM events, response failures and cross-replica idempotency. The default two-replica fixture limits total 752 MiB, excluding the host OS and Docker daemon. These limits describe that local fixture; they are not a production gate for a separated deployment. Local Docker is sufficient for this functional acceptance; an additional native x86_64 test host is not required.

## Database baseline

The current PostgreSQL migration chain starts at `0001_baseline.sql` and reaches schema 15. New installations apply the full chain to an empty database. Compatible installations apply pending migrations. Use the matching old image and master key to open databases or backups from the former 0001–0074 chain. Ordinary API startup checks every migration checksum, the management protocol and the master-key fingerprint. Preserve the recorded migration history.

Run `--migrate-only` before starting the API on a new database. It initializes administrator credentials, templates, reference indexes and work queues with workers stopped and no node access. See [migration maintenance](../crates/hysteriax-server/migrations/README.md).

### Upgrade an existing installation

1. Verify a recoverable [backup](backup-restore.md) and retain the matching old image, deployment configuration and master key. Build and transfer the new image locally, then prepare its corresponding Compose configuration.
2. Stop every API replica that uses the database, including replicas on other hosts. For the bundled configuration use `docker compose stop api` and check the drain result. Keep PostgreSQL and Redis available.
3. Select the loaded image in `.env` and run `docker compose --profile maintenance run --pull never --rm --no-deps migrate` once. Schema 15 rebuilds and adds indexes, so verify PostgreSQL storage headroom and allow time for maintenance.
4. Start the API and Caddy, then check `/readyz`, restart counts and the API logs. Open the matching Mac client and confirm it connects. Restore with matching database and image versions if a rollback requires reversing a schema change.

Use `docker compose stop api` so the API's configured 150-second stop grace applies. On SIGTERM, sampling stops admitting work and drains in-flight collections while their leases continue renewing; HTTP requests have an 8-second drain budget, followed by up to 120 seconds for sampling. A sampling drain timeout exits with an error and requires inspection of uncaptured clear requests. For a direct Docker stop, use `docker stop --time 150 <api-container>`; a shorter override can kill the process before it saves a destructive traffic response. The grace period applies when the container is created, so recreate existing API containers with the current Compose configuration.

Check the running version's shutdown behavior before stopping it. Forced stops, crashes, response loss or a database outage can leave a `traffic_clear_uncertain` gap. Preserve captured batches and inspect the sampling log before continuing an upgrade with a failed drain.

## Local development and tests

The Rust workspace declares a minimum version of 1.94; [Cargo.toml](../Cargo.toml) is authoritative. Mac source builds use Xcode; the separate SwiftPM requirements are declared in [Package.swift](../apps/macos/Package.swift) and explained in [compatibility](compatibility.md). Build and run the client with `scripts/build/build-macos.sh`, or use its `--verify` mode to check startup. See the [operation index](../scripts/README.md) for build tools and [architecture](architecture.md#api-and-event-contract) for the management API contract.

Set `DATABASE_URL` to a PostgreSQL URL, `HYSTERIAX_ADMIN_TOKEN` to at least 256 bits of random token material, `HYSTERIAX_MASTER_KEY` to a base64-encoded 32-byte key, and `HYSTERIAX_PUBLIC_URL` to the public HTTPS origin. With those variables exported, run `cargo run -p hysteriax-server -- --migrate-only` once, then start the API with `scripts/ops/run.sh`. Set `HYSTERIAX_REDIS_URL` for Redis-backed caches; unavailable Redis permits database fallback.

Build, generation, operations and actual migration tools live in `scripts/`. Repository-level verification lives in `tests/`; macOS-specific regression sources and runners live in `apps/macos/Tests/`. See [directory and test ownership](code-layout.md) for the layout and [the test index](../tests/README.md) for entry points.

From the repository root, prepare Python verification dependencies and run the script unit tests:

```sh
python3 -m pip install -r tests/requirements.txt
python3 -m unittest discover -s tests/unit -p 'test_*.py'
```

For local database tests, use a separate checkout and a disposable Compose project, then start the same PostgreSQL image with the loopback-only port override. Do not use a production `.env` or project for these commands:

```sh
set -a
. ./.env
set +a
export TEST_DATABASE_URL="postgresql://postgres:${HYSTERIAX_DB_ADMIN_PASSWORD}@127.0.0.1:${HYSTERIAX_DB_HOST_PORT:-55432}/hysteriax"
docker compose --env-file .env -f compose.yaml -f compose.test.yaml up -d postgres
cargo test --workspace
```

Rust database tests create isolated schemas. For Redis integration tests, also supply `TEST_REDIS_URL` for a disposable Redis service. Some live scripts accept `TEST_DATABASE_URL`; newer full-process verifiers have explicit command-line fixture arguments. Check each script's `--help` and the [verification guide](deployment-testing.md) before running it. Stop only the test project's resources when finished.

## Existing SQLite installations

The PostgreSQL release starts with a new, empty database and does not import SQLite data. Before switching an existing installation, use the old service's uninstall action for remote nodes that will be reused, retain the old database and encryption key as a separate archive, then recreate nodes, resources, users, assignments, and subscriptions in the new service. Old SQLite backup archives cannot be restored by the PostgreSQL backup tool.

## First administrator connection

Use the initial token from `.env` as the Bearer token. In macOS Settings, create and switch to a new administrator token; the app saves it in Keychain and shows the plaintext once. Confirm that it is marked as this Mac's current token, then revoke the initial token if desired. The API is also available through `POST /api/v2/admin/tokens` and `DELETE /api/v2/admin/tokens/{id}`, with an `Idempotency-Key` for each intended write. The server never returns a token digest as a usable credential.

## Sampling concurrency

`HYSTERIAX_BACKGROUND_WORKER_PARALLELISM` defaults to **16** and accepts 1–16. It limits each sampling queue (`traffic`, `network`, and `proxy`) separately in each API instance. Missing or invalid values use 16; explicit lower values are respected. Management jobs retain their separate ceiling of four. A node's resource lease serializes its sampling and management work across queues and instances.

Sampling cadence defaults to 10 seconds and is set by `HYSTERIAX_SAMPLING_INTERVAL_SECONDS`. Budget `HYSTERIAX_DB_POOL_MAX_CONNECTIONS` per API replica and leave connections for maintenance and other database clients. Recreate containers after changing environment settings. Use the [verification guide](deployment-testing.md) to choose capacity checks for the intended workload.

## Sampling history retention

The API automatically deletes eligible terminal sampling work and sampling cycles after 14 days. It can also remove their released work leases and discarded batches that never requested a remote counter clear or captured a payload.

The worker checks every five minutes when idle and resumes a partial sweep after one second. Logs under `sampling retention claim finished` report committed deletions by table, elapsed time and remaining pages.

## Database history retention

The API starts five database history cleanup groups (14 worker lanes total) automatically.

| Cleanup group | Workers | Data handled |
| --- | --- | --- |
| Traffic detail | 4 (node-sharded, shared traffic gate) | Fully settled `traffic_sample_items` capture details after 14 days. |
| Traffic history | 4 (node-sharded, shared traffic gate) | `traffic_records`, eligible `node_network_samples`, settled `traffic_sample_batches` (`complete` and `discarded`), resolved `data_gaps`, and unassigned `traffic_baselines` after 60 days. |
| Monitoring history | 4 (sharded, independent gate) | `online_samples` (cycle-grouped by `cycle_id`) and `proxy_probe_samples` (by `node_id`) after 31 days. |
| Operations history | 1 (unsharded) | Expired `deployment_probe_tokens` and `monitoring_probe_tokens` (0 days); inactive `service_instances`, `event_publications`, terminal `kick_requests`, and whitelisted terminal non-sampling `background_work` (30 days); eligible `execution_operations`, `execution_recheck_requests`, `jobs` retry chains, `job_events`, and `audit_records` (90 days). |
| Metadata history | 1 (unsharded) | Released orphan `resource_leases` and zeroed orphan `job_node_kick_totals` (0 days); `management_receipts`, `mtls_operations`, and non-`quota_rank` `cache_entity_versions` tombstones (30 days); revoked `user_credentials`, `credential_application_receipts`, `credential_batches`, `dns_operations`, deleted `dns_records`, `mtls_node_operations`, terminal `mtls_rotations`, completed `mtls_trust_cleanup`, unreferenced `mtls_deployment_snapshots`, obsolete `mtls_activations`, and excess `config_versions` (90 days). |

Logs under `history retention page committed` record the table, counts, oldest inspected timestamp and skip reasons. Sharded traffic groups share four per-shard page gates with independent cursors, while `monitoring-history-retention` runs its four sharded lanes independently without serializing against traffic cleanup. Database rules and exceptions are listed in [Database tables](database-tables.md).

Retention windows are eligibility cutoffs. Workers may defer rows with unresolved references, active leases, unfinished billing or pending samples. Cleanup keeps current user quota accounting and the protected node package period. When reviewing disk usage, compare live rows, dead rows, table and index sizes and the last autovacuum. Normal vacuum makes space reusable inside PostgreSQL.

## File uploads and traffic capture

`HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS` controls each SSH file upload independently of `HYSTERIAX_OPERATION_TIMEOUT_SECONDS`. It defaults to **180 seconds (3 minutes)** and accepts 1–3600 seconds. Missing or invalid values use 180 seconds. For a bandwidth-limited management server, increase only this setting; ordinary SSH commands, probes and sampling retain their existing operation budgets. The client reserves another five seconds after the receiver deadline to collect its completion receipt.

`HYSTERIAX_TRAFFIC_CAPTURE_ENABLED` defaults to `true`. To pause new captures, set it to `false` in the API process environment and recreate the API container. With Compose, pass the variable through the API service's `environment` mapping. Previously captured batches continue billing. Node sampling uses interval reads (`/traffic?clear=1`). The server persists complete responses before verifying the node instance and records a statistics gap when capture or verification is uncertain. Each batch issues at most one clear request.
