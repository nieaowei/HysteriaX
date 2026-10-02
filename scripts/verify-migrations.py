#!/usr/bin/env python3
"""Verify that a database at migration 0001 upgrades without losing rows."""

import base64
import hashlib
import os
import pathlib
import secrets
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    migration = ROOT / "crates/hysteriax-server/migrations/0001_initial.sql"
    sql = migration.read_text()

    with tempfile.TemporaryDirectory(prefix="hysteriax-migration-upgrade-") as temporary:
        database_path = pathlib.Path(temporary) / "service.db"
        backup_directory = pathlib.Path(temporary) / "migration-backups"
        with sqlite3.connect(database_path) as database:
            database.executescript(sql)
            database.execute(
                "CREATE TABLE _sqlx_migrations ("
                "version BIGINT PRIMARY KEY, description TEXT NOT NULL, "
                "installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, "
                "success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)"
            )
            database.execute(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) "
                "VALUES (1, 'initial', TRUE, ?, 1)",
                (sqlite3.Binary(hashlib.sha384(sql.encode()).digest()),),
            )
            database.execute(
                "INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, "
                "ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, "
                "traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) "
                "VALUES ('legacy-node', 'Legacy node', '127.0.0.1', 22, 'root', 'private_key', "
                "'encrypted-ssh', 'node.example.test', 443, ':443', 'legacy-hash', 'encrypted-token', "
                "'encrypted-stats', 'encrypted-config', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')"
            )

        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        admin_token = "hx_" + secrets.token_urlsafe(36)
        environment = os.environ.copy()
        environment.update(
            {
                "DATABASE_URL": f"sqlite://{database_path}?mode=rwc",
                "HYSTERIAX_MIGRATION_BACKUP_DIR": str(backup_directory),
                "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{port}",
                "HYSTERIAX_PUBLIC_URL": "https://management.example.test",
                "HYSTERIAX_ADMIN_TOKEN": admin_token,
                "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="),
                "RUST_LOG": "warn",
            }
        )
        server = subprocess.Popen(
            [str(SERVER)], cwd=ROOT, env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        try:
            ready = False
            for _ in range(40):
                if server.poll() is not None:
                    raise RuntimeError("management service exited during the migration upgrade")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{port}/readyz", timeout=2) as response:
                        if response.status == 200:
                            ready = True
                            break
                except (OSError, urllib.error.URLError):
                    pass
                time.sleep(0.25)
            if not ready:
                raise RuntimeError("management service did not become ready after migrating the legacy database")
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()

        with sqlite3.connect(database_path) as database:
            versions = [row[0] for row in database.execute("SELECT version FROM _sqlx_migrations ORDER BY version")]
            columns = {row[1] for row in database.execute("PRAGMA table_info(nodes)")}
            legacy_node = database.execute(
                "SELECT name, last_sample_at, proxy_probe_url FROM nodes WHERE id = 'legacy-node'"
            ).fetchone()
        if versions != [1, 2, 3, 4, 5] or not {"last_sample_at", "proxy_probe_url", "traffic_stats_port"}.issubset(columns):
            raise RuntimeError(f"migration history or node column was not upgraded: {versions}")
        tables = {
            row[0]
            for row in database.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
        }
        if "deployment_probe_tokens" not in tables:
            raise RuntimeError("deployment-probe credential table was not created")
        if legacy_node != ("Legacy node", None, None):
            raise RuntimeError(f"legacy node data did not survive the upgrade: {legacy_node}")
        backups = list(backup_directory.glob("*.sqlite3"))
        if len(backups) != 1:
            raise RuntimeError(f"expected one pre-migration SQLite snapshot, found {len(backups)}")
        with sqlite3.connect(backups[0]) as backup:
            integrity = backup.execute("PRAGMA quick_check").fetchone()
            backup_versions = [
                row[0]
                for row in backup.execute(
                    "SELECT version FROM _sqlx_migrations ORDER BY version"
                )
            ]
            backup_columns = {row[1] for row in backup.execute("PRAGMA table_info(nodes)")}
            backup_node = backup.execute(
                "SELECT name FROM nodes WHERE id = 'legacy-node'"
            ).fetchone()
        if integrity != ("ok",) or backup_versions != [1]:
            raise RuntimeError(
                f"pre-migration snapshot is invalid or has the wrong schema version: {integrity}, {backup_versions}"
            )
        if "last_sample_at" in backup_columns or backup_node != ("Legacy node",):
            raise RuntimeError(f"snapshot does not reflect the pre-migration database: {backup_node}")
        if backup_directory.stat().st_mode & 0o777 != 0o700 or backups[0].stat().st_mode & 0o777 != 0o600:
            raise RuntimeError("pre-migration backup directory or SQLite snapshot permissions are too broad")
        print("Database migration 0001→0005 passed; legacy data survived and a verified, mode-restricted v0001 snapshot was created before migration.")


if __name__ == "__main__":
    main()
