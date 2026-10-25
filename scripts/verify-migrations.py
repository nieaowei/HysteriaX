#!/usr/bin/env python3
"""Verify maintenance initialization, native schema, and concurrent protocol-2 instances."""

import base64
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from postgres_test import PostgresTestSchema
from management_test_http import start_test_server


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_ready(base, process, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("management service exited before PostgreSQL readiness")
        try:
            with urllib.request.urlopen(base + "/readyz", timeout=1) as response:
                if response.status == 200:
                    return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.2)
    raise TimeoutError("management service did not become ready with PostgreSQL")


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(
        prefix="hysteriax-postgresql-migration-"
    ) as folder:
        admin = "hx_" + secrets.token_urlsafe(36)
        environment = os.environ.copy()
        environment.update(
            {
                "DATABASE_URL": database.url,
                "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{free_port()}",
                "HYSTERIAX_PUBLIC_URL": "https://management.example.test",
                "HYSTERIAX_ADMIN_TOKEN": admin,
                "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="),
                "RUST_LOG": "warn",
            }
        )
        address = environment["HYSTERIAX_LISTEN_ADDR"].rsplit(":", 1)[1]
        base = f"http://127.0.0.1:{address}"
        log = open(pathlib.Path(folder) / "server.log", "wb")
        server = start_test_server(
            [str(SERVER)], cwd=ROOT, env=environment, stdout=log, stderr=log
        )
        duplicate = None
        try:
            wait_ready(base, server)
            with database.connect() as connection:
                version = connection.execute(
                    "SELECT MAX(version) FROM _sqlx_migrations WHERE success"
                ).fetchone()[0]
                columns = dict(
                    connection.execute(
                        "SELECT column_name, data_type FROM information_schema.columns "
                        "WHERE table_schema = %s AND table_name = 'users'",
                        (database.name,),
                    ).fetchall()
                )
                tables = {
                    row[0]
                    for row in connection.execute(
                        "SELECT table_name FROM information_schema.tables WHERE table_schema = %s",
                        (database.name,),
                    )
                }
            expected_version = max(int(path.name.split("_", 1)[0]) for path in (ROOT / "crates/hysteriax-server/migrations").glob("*.sql"))
            if version != expected_version:
                raise RuntimeError(f"unexpected PostgreSQL schema version: {version}")
            expected = {
                "enabled": "boolean",
                "expires_at": "timestamp with time zone",
                "quota_reset_at": "timestamp with time zone",
                "usage_bytes": "bigint",
            }
            if any(columns.get(name) != value for name, value in expected.items()):
                raise RuntimeError(f"PostgreSQL native column types differ: {columns}")
            if not {"deployment_probe_tokens", "node_packages", "node_network_samples", "node_alerts", "node_acme_observations"}.issubset(tables):
                raise RuntimeError("deployment-probe or node-package tables were not created")

            duplicate_environment = environment.copy()
            duplicate_environment["HYSTERIAX_LISTEN_ADDR"] = f"127.0.0.1:{free_port()}"
            duplicate = subprocess.Popen(
                [str(SERVER)],
                cwd=ROOT,
                env=duplicate_environment,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            wait_ready("http://" + duplicate_environment["HYSTERIAX_LISTEN_ADDR"], duplicate)
            if server.poll() is not None:
                raise RuntimeError("starting the second replica stopped the first")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                with database.connect() as connection:
                    fresh = connection.execute("SELECT count(*) FROM service_instances WHERE heartbeat_at > clock_timestamp()-interval '25 seconds'").fetchone()[0]
                if fresh == 2:
                    break
                time.sleep(0.2)
            else:
                raise RuntimeError("both protocol-2 replicas must register fresh independent heartbeats")
            print("Maintenance initialization and native schema passed; two independent protocol-2 instances are ready.")
        finally:
            stop(duplicate)
            stop(server)
            log.close()


if __name__ == "__main__":
    main()
