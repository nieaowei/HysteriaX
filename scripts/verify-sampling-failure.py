#!/usr/bin/env python3
"""Verify a remote traffic-sampling outage records a gap without charging usage."""

import base64
import json
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
import uuid


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def request(base, path, token=None, method="GET", payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    headers = {}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=3) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)

    with tempfile.TemporaryDirectory(prefix="hysteriax-sampling-failure-") as folder:
        temp = pathlib.Path(folder)
        api_port = free_port()
        ssh_port = free_port()
        base = f"http://127.0.0.1:{api_port}"
        admin = "hx_" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        master_key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        environment = os.environ.copy()
        environment.update(
            {
                "DATABASE_URL": f"sqlite://{temp / 'service.db'}?mode=rwc",
                "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{api_port}",
                "HYSTERIAX_PUBLIC_URL": "https://management.example.test",
                "HYSTERIAX_ADMIN_TOKEN": admin,
                "HYSTERIAX_MASTER_KEY": master_key,
                "RUST_LOG": "warn",
            }
        )
        log = open(temp / "service.log", "wb")
        process = subprocess.Popen(
            [str(SERVER)], cwd=ROOT, env=environment, stdout=log, stderr=log
        )
        try:
            ready = False
            for _ in range(50):
                if process.poll() is not None:
                    raise RuntimeError("HysteriaX exited before readiness")
                try:
                    status, _ = request(base, "/readyz", admin)
                    if status == 200:
                        ready = True
                        break
                except Exception:
                    pass
                time.sleep(0.2)
            if not ready:
                raise TimeoutError("HysteriaX did not become ready")

            status, body = request(
                base,
                "/api/v1/nodes",
                admin,
                "POST",
                {
                    "name": "Unreachable sampling fixture",
                    "ssh_host": "127.0.0.1",
                    "ssh_port": ssh_port,
                    "ssh_username": "root",
                    "ssh_auth_type": "private_key",
                    "ssh_secret": "not-used-by-refused-connection",
                    "public_host": "sampling.example.test",
                    "public_port": 443,
                    "listen_addr": ":443",
                },
            )
            if status != 201:
                raise RuntimeError(f"node fixture creation failed: HTTP {status}")
            node_id = json.loads(body)["node"]["id"]

            status, body = request(
                base, "/api/v1/users", admin, "POST", {"name": "Sampling outage user"}
            )
            if status != 201:
                raise RuntimeError(f"user fixture creation failed: HTTP {status}")
            user_id = json.loads(body)["id"]
            status, _ = request(
                base,
                f"/api/v1/users/{user_id}/assignments",
                admin,
                "POST",
                {"expected_revision": 1, "node_id": node_id},
            )
            if status != 201:
                raise RuntimeError(f"user assignment failed: HTTP {status}")

            # Mark the fixture deployed so the real background sampler attempts SSH.
            with sqlite3.connect(temp / "service.db") as database:
                database.execute(
                    "UPDATE nodes SET deployed_revision = desired_revision, "
                    "deployed_config_enc = desired_config_enc, state = 'deployed' WHERE id = ?",
                    (node_id,),
                )
                database.commit()

            usage_path = f"/api/v1/users/{user_id}/usage"
            deadline = time.time() + 12
            response = None
            while time.time() < deadline:
                status, body = request(base, usage_path, admin)
                if status == 200:
                    response = json.loads(body)
                    if response["data_freshness"]["open_gaps"] > 0:
                        break
                time.sleep(0.2)
            if not response or response["data_freshness"]["open_gaps"] != 1:
                raise RuntimeError("sampling outage did not create one open data gap")

            # Let another collection interval fail; the open gap must not multiply.
            time.sleep(11)
            status, body = request(base, usage_path, admin)
            if status != 200:
                raise RuntimeError(f"usage query failed: HTTP {status}")
            usage = json.loads(body)

            with sqlite3.connect(temp / "service.db") as database:
                charged = database.execute(
                    "SELECT usage_bytes FROM users WHERE id = ?", (user_id,)
                ).fetchone()[0]
                node_state = database.execute(
                    "SELECT state FROM nodes WHERE id = ?", (node_id,)
                ).fetchone()[0]
                open_gaps = database.execute(
                    "SELECT COUNT(*) FROM data_gaps WHERE node_id = ? AND resolved_at IS NULL",
                    (node_id,),
                ).fetchone()[0]
                records = database.execute(
                    "SELECT COUNT(*) FROM traffic_records WHERE node_id = ?", (node_id,)
                ).fetchone()[0]
            if charged != 0 or node_state != "unreachable" or open_gaps != 1 or records != 0:
                raise RuntimeError(
                    f"sampling failure changed usage or gap state: {charged=}, {node_state=}, {open_gaps=}, {records=}"
                )
            if usage["data_freshness"]["status"] != "not_collected":
                raise RuntimeError("never-sampled user was not marked as not collected")
            print("Remote sampling outage left usage unchanged, marked the node unreachable, and kept one open data gap.")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            log.close()


if __name__ == "__main__":
    main()
