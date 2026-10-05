# Installation

## Management service with Docker Compose

1. Point a DNS A/AAAA record for the management hostname at the server.
2. Install Docker Engine and the Docker Compose plugin.
3. From the project directory, run `scripts/init-secrets.sh manage.example.com`. This creates a private `.env` with the administrator token, encryption key, and separate PostgreSQL administrator and application passwords.
4. Save the administrator token and make a separate protected backup of `HYSTERIAX_MASTER_KEY`.
5. Start the service with `docker compose up -d --build`.
6. Confirm `https://manage.example.com/readyz` returns `{"status":"ready","database":"ok"}`.

Compose starts PostgreSQL 18 with a persistent volume and a non-superuser application role. The database listens only on the Compose network and is not published to the host. Caddy obtains and renews the management HTTPS certificate; TCP 80 and 443 must reach the host. Node Hysteria traffic uses the configured UDP ports. This service does not alter cloud security groups or host firewalls.

For a tagged release image, set `HYSTERIAX_IMAGE=ghcr.io/<owner>/hysteriax-server` and `HYSTERIAX_VERSION=vX.Y.Z` in `.env`, then run `docker compose pull` and `docker compose up -d`. Caddy state and PostgreSQL data use separate named volumes. Keep `.env`, database backups, `HYSTERIAX_MASTER_KEY`, and Caddy state in protected backups. Never commit `.env` or production data.

## Local development and tests

Set `DATABASE_URL` to a PostgreSQL URL, `HYSTERIAX_ADMIN_TOKEN` to at least 256 bits of random token material, `HYSTERIAX_MASTER_KEY` to a base64-encoded 32-byte key, and `HYSTERIAX_PUBLIC_URL` to the public HTTPS origin. Start the API with `scripts/run.sh`.

For local database tests, start the same PostgreSQL image with the loopback-only port override:

```sh
set -a
. ./.env
set +a
export TEST_DATABASE_URL="postgresql://postgres:${HYSTERIAX_DB_ADMIN_PASSWORD}@127.0.0.1:${HYSTERIAX_DB_HOST_PORT:-55432}/hysteriax"
docker compose --env-file .env -f compose.yaml -f compose.test.yaml up -d postgres
python3 -m pip install -r scripts/requirements-test.txt
cargo test --workspace
```

The tests create isolated schemas inside the disposable test database. The live verification scripts use the same `TEST_DATABASE_URL` and also need Docker where their instructions require it. Stop the test database with the matching Compose command and `down` when finished.

## Existing SQLite installations

The PostgreSQL release starts with a new, empty database and does not import SQLite data. Before switching an existing installation, use the old service's uninstall action for remote nodes that will be reused, retain the old database and encryption key as a separate archive, then recreate nodes, resources, users, assignments, and subscriptions in the new service. Old SQLite backup archives cannot be restored by the PostgreSQL backup tool.

## First administrator connection

Use the initial token from `.env` as the Bearer token. In macOS Settings, create and switch to a new administrator token; the app saves it in Keychain and shows the plaintext once. Confirm that it is marked as this Mac's current token, then revoke the initial token if desired. The API is also available through `POST /api/v1/admin/tokens` and `DELETE /api/v1/admin/tokens/{id}`. The server never returns a token digest as a usable credential.

## Credential center upgrade (API 1.0.0)

Upgrade the service and macOS client together. The credential changes modify `/api/v1` directly and break its previous request/response contract. No v2 management routes are exposed. Existing subscription URLs and authentication callbacks remain valid. Back up PostgreSQL and the matching master key before upgrading. Startup converts stored SSH material, certificate/ECH resources, DNS configuration and user mTLS material atomically before workers start. It preserves existing token values, revisions, counters and historical deployment references. Invalid ciphertext or missing resources aborts conversion and keeps the API unready.

To preflight a database copy, run `hysteriax-server --migrate-only` with that copy's `DATABASE_URL` and the matching `HYSTERIAX_MASTER_KEY`. This mode does not start workers or contact nodes. `scripts/verify-credentials-migration.py` compares an untouched copy against the migrated copy; provide its two database URLs and master key through environment variables. Restore the pre-upgrade backup with the old image when reverting; merely selecting the old image against the upgraded schema is unsupported.

## TLS certificate-pair consolidation (0008)

Upgrade macOS and the service together. TLS uses `tls_identity`; independent CA
certificates use `ca_certificate`. v1 no longer supports standalone
`certificate`/`private_key` creation. Back up the database before startup:
split configured material is merged, old standalone objects are removed, and
current/deployed/historical configuration retains its runtime content and file
names. No node deployment is scheduled by this migration.
