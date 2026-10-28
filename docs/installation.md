# Installation

## Management service with Docker Compose

1. Point a DNS A/AAAA record for the management hostname at the server.
2. Install Docker Engine and the Docker Compose plugin.
3. From the project directory, run `scripts/init-secrets.sh manage.example.com`. This creates a private `.env` with the administrator token, encryption key, and separate PostgreSQL administrator and application passwords.
4. Save the administrator token and make a separate protected backup of `HYSTERIAX_MASTER_KEY`.
5. Build the image on your development machine for the server's architecture and transfer it to the server using the commands below. Start dependencies with `docker compose up -d postgres redis`, then initialize the database with `docker compose --profile maintenance run --pull never --rm --no-deps migrate`. Start the service with `docker compose up -d --no-build api caddy`.
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

After checking the host's memory and PostgreSQL connection budget, multiple API replicas can share that database, Redis and master key. For example, `docker compose up -d --no-build --scale api=2 api caddy` starts two replicas without building on the deployment host. The Caddy configuration discovers the `api` Docker DNS addresses every five seconds and selects them with round-robin load balancing. Passive failure tracking and a one-second dial timeout leave room to try a healthy replica within the three-second retry window; Caddy's default retry rules still apply. No sticky session is required. See the [Caddy reverse proxy reference](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy) and run `python3 scripts/verify-caddy-replicas.py --report /tmp/caddy-replicas.json` for the isolated proxy check. That check uses lightweight HTTP backends and does not replace the full HysteriaX business acceptance or host capacity measurements.

Budget each production host for the services actually scheduled there. The bundled Compose stack is a convenient single-host topology, not a requirement that PostgreSQL, Redis and API replicas share one machine. The server accepts separate `DATABASE_URL` and `HYSTERIAX_REDIS_URL` endpoints; supply the external addresses in the production service configuration. Account for API process memory on API hosts, PostgreSQL connections and query memory on database hosts, and Redis memory on its host. Do not compare the sum of separate hosts' service limits with an API host's RAM.

For local Docker acceptance with a PostgreSQL dump from this baseline, use `python3 scripts/verify-snapshot-capacity.py --help` and `--replicas 3`. Build its test-only network helper from `scripts/fixtures/egress-guard.Dockerfile` first. The verifier restores into separate owned containers, installs an outbound allowlist before releasing each API startup gate, and exercises real API replicas through Caddy with Redis available, stopped, and restarted empty. It records cgroup memory, OOM events, response failures and cross-replica idempotency. The default two-replica fixture limits total 752 MiB, excluding the host OS and Docker daemon. These limits describe that local fixture; they are not a production gate for a separated deployment. Local Docker is sufficient for this functional acceptance; an additional native x86_64 test host is not required.

## Database baseline

This release replaces the former 0001–0074 migration chain with `0001_baseline.sql`. New installations initialize an empty PostgreSQL database. There is no automatic upgrade or import from the former chain; an old version-1 record also has a different checksum and fails startup validation. Do not clear migration history or replay the baseline against an existing database.

Existing installations must retain a matching image, metadata backup, configuration and master key. On 2026-10-28, the current production database was explicitly normalized after logical schema verification and a tested metadata rollback: its 74 migration records were replaced with the baseline record, and `runtime_contract.schema_version` became 1. No baseline SQL or business-data migration ran. This controlled registration applies only to that verified database; this was a verified production maintenance operation. Other legacy databases require separate validation or a fresh installation.

For a new empty database, run `--migrate-only` before the first API startup. It applies the baseline and completes administrator, credential, template, reference-index and work-queue initialization without starting workers or contacting nodes. Ordinary startup checks the exact migration history, protocol and master key. Management API requests use `/api/v2`, and administrative writes require a stable `Idempotency-Key` for retries.

On 2026-10-29, production applied migration 0002 through `--migrate-only`; it cleared old kick requirements and task history without running the baseline SQL. Production now uses schema version 2. Future migrations start at `0003`. For subsequent compatible upgrades, stop every API replica, retain a recoverable backup and matching old image, run maintenance once, then start the corresponding server and macOS client. See [migration maintenance](../crates/hysteriax-server/migrations/README.md).

## Local development and tests

Set `DATABASE_URL` to a PostgreSQL URL, `HYSTERIAX_ADMIN_TOKEN` to at least 256 bits of random token material, `HYSTERIAX_MASTER_KEY` to a base64-encoded 32-byte key, and `HYSTERIAX_PUBLIC_URL` to the public HTTPS origin. With those variables exported, run `cargo run -p hysteriax-server -- --migrate-only` once, then start the API with `scripts/run.sh`. Set `HYSTERIAX_REDIS_URL` for Redis-backed caches; unavailable Redis permits database fallback.

For local database tests, use a separate checkout and a disposable Compose project, then start the same PostgreSQL image with the loopback-only port override. Do not use a production `.env` or project for these commands:

```sh
set -a
. ./.env
set +a
export TEST_DATABASE_URL="postgresql://postgres:${HYSTERIAX_DB_ADMIN_PASSWORD}@127.0.0.1:${HYSTERIAX_DB_HOST_PORT:-55432}/hysteriax"
docker compose --env-file .env -f compose.yaml -f compose.test.yaml up -d postgres
python3 -m pip install -r scripts/requirements-test.txt
cargo test --workspace
```

Rust database tests create isolated schemas. For Redis integration tests, also supply `TEST_REDIS_URL` for a disposable Redis service. Some live scripts accept `TEST_DATABASE_URL`; newer full-process verifiers have explicit command-line fixture arguments. Check each script's `--help` and the [verification guide](deployment-testing.md) before running it. Stop only the test project's resources when finished.

## Existing SQLite installations

The PostgreSQL release starts with a new, empty database and does not import SQLite data. Before switching an existing installation, use the old service's uninstall action for remote nodes that will be reused, retain the old database and encryption key as a separate archive, then recreate nodes, resources, users, assignments, and subscriptions in the new service. Old SQLite backup archives cannot be restored by the PostgreSQL backup tool.

## First administrator connection

Use the initial token from `.env` as the Bearer token. In macOS Settings, create and switch to a new administrator token; the app saves it in Keychain and shows the plaintext once. Confirm that it is marked as this Mac's current token, then revoke the initial token if desired. The API is also available through `POST /api/v2/admin/tokens` and `DELETE /api/v2/admin/tokens/{id}`, with an `Idempotency-Key` for each intended write. The server never returns a token digest as a usable credential.

## File upload timeout

`HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS` controls each SSH file upload independently of `HYSTERIAX_OPERATION_TIMEOUT_SECONDS`. It defaults to **180 seconds (3 minutes)** and accepts 1–3600 seconds. Missing or invalid values use 180 seconds. For a bandwidth-limited management server, increase only this setting; ordinary SSH commands, probes and sampling retain their existing operation budgets. The client reserves another five seconds after the receiver deadline to collect its completion receipt.

`HYSTERIAX_TRAFFIC_CAPTURE_ENABLED` defaults to `true`. Set it to `false` and restart the API to pause new remote traffic captures during maintenance; previously captured batches continue billing. All nodes, including newly created nodes, use destructive interval reads (`/traffic?clear=1`). Complete responses are persisted before follow-up instance verification; a lost response or unverified instance is reported as a statistics gap. The collector never replays a destructive read for the same batch.

The baseline creates interval capture defaults and constraints directly. It does not include the removed traffic cutover command, one-time cleanup tools or historical cleanup progress tables. Completed legacy capture modes remain representable in the accounting schema, but no old-database upgrade path is provided.
