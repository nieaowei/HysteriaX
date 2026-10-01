# Installation

## Management service with Docker Compose

1. Point a DNS A/AAAA record for the management hostname at the server.
2. Install Docker Engine and the Docker Compose plugin.
3. From the project directory, run `scripts/init-secrets.sh manage.example.com`. This creates a private `.env` file containing a random administrator bearer token and a separate encryption key, plus a protected `backups/migrations/` directory for automatic pre-migration database snapshots.
4. Save both values in a password manager and make a separate protected backup of the encryption key.
5. Start the service with `docker compose up -d --build`.
6. Confirm `https://manage.example.com/readyz` returns `{"status":"ready","database":"ok"}`.

For a tagged release image, set `HYSTERIAX_IMAGE=ghcr.io/<owner>/hysteriax-server` and `HYSTERIAX_VERSION=vX.Y.Z` in `.env`, then run `docker compose pull` and `docker compose up -d`. The default `.env` selects the local `hysteriax-server:dev` image and the quick start builds it from source. Published image tags and the unsigned macOS DMG for direct distribution are produced by the release workflows; see [the unsigned DMG release guide](release.md).

Caddy obtains and renews the management HTTPS certificate. TCP 80 and 443 must reach the host. Node Hysteria traffic uses UDP ports configured for each node; this service does not alter cloud security groups or host firewalls.

The SQLite database is stored in `./data`; verified snapshots made before schema migrations are stored in `./backups/migrations/`. The API container needs write access to that directory. For an existing installation, create it and grant the configured `HYSTERIAX_UID`/`HYSTERIAX_GID` ownership before upgrading the Compose configuration. Caddy state is stored in named Compose volumes. Keep `.env`, the database, migration snapshots, and Caddy state in protected backups. Never commit `.env` or production data.

## Local development

Set the following variables in the shell or an ignored local environment file:

- `DATABASE_URL=sqlite://data/hysteriax.db?mode=rwc`
- `HYSTERIAX_ADMIN_TOKEN`: at least 256 bits of random token material, encoded as text
- `HYSTERIAX_MASTER_KEY`: a base64 encoded 32-byte key
- `HYSTERIAX_PUBLIC_URL`: public HTTPS origin used in node auth URLs and subscription links

Start the API with `scripts/run.sh`. `HYSTERIAX_LISTEN_ADDR` defaults to `0.0.0.0:8080`.

## First administrator connection

Use the token from `.env` as the Bearer token. In macOS Settings, create and switch to a new administrator token; the app saves it in Keychain and shows the plaintext once. Confirm that it is marked as this Mac's current token, then revoke the initial token if desired. The API is also available through `POST /api/v1/admin/tokens` and `DELETE /api/v1/admin/tokens/{id}`. The server never returns a token digest as a usable credential.
