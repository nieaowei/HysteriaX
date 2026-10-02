#!/usr/bin/env python3
"""Restart the service with an interrupted job and verify persisted recovery."""

import base64
from datetime import datetime, timezone
import json
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid

from postgres_test import PostgresTestSchema


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_ready(base, process, timeout=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if process.poll() is not None:
            raise RuntimeError("HysteriaX exited before readiness")
        try:
            with urllib.request.urlopen(base + "/readyz", timeout=1) as response:
                if response.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.2)
    raise TimeoutError("HysteriaX did not become ready")


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def job_detail(base, job_id, admin_token):
    request = urllib.request.Request(
        base + f"/api/v1/jobs/{job_id}",
        headers={"Authorization": f"Bearer {admin_token}"},
    )
    with urllib.request.urlopen(request, timeout=3) as response:
        return json.loads(response.read())


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)

    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-restart-recovery-") as folder:
        temp = pathlib.Path(folder)
        port = free_port()
        base = f"http://127.0.0.1:{port}"
        environment = os.environ.copy()
        admin_token = "hx_" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        environment.update(
            {
                "DATABASE_URL": database.url,
                "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{port}",
                "HYSTERIAX_PUBLIC_URL": "http://127.0.0.1",
                "HYSTERIAX_ADMIN_TOKEN": admin_token,
                "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="),
                "RUST_LOG": "warn",
            }
        )
        api_log = open(temp / "service.log", "wb")
        process = subprocess.Popen(
            [str(SERVER)], cwd=ROOT, env=environment, stdout=api_log, stderr=api_log
        )
        try:
            wait_ready(base, process)
            stop(process)
            process = None

            job_id = str(uuid.uuid4())
            timestamp = datetime(2026, 1, 1, tzinfo=timezone.utc)
            with database.connect() as connection:
                connection.execute(
                    "INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, "
                    "ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, "
                    "traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) "
                    "VALUES ('recovery-node', 'Recovery node', '127.0.0.1', 22, 'root', 'private_key', "
                    "'encrypted-ssh', 'node.example.test', 443, ':443', 'node-hash', 'encrypted-token', "
                    "'encrypted-stats', 'encrypted-config', %s, %s)",
                    (timestamp, timestamp),
                )
                connection.execute(
                    "INSERT INTO jobs (id, kind, node_id, target_revision, status, stage, payload_json, attempts, "
                    "available_at, created_at, updated_at, started_at) "
                    "VALUES (%s, 'sync', 'recovery-node', 1, 'running', 'interrupted', '{}', 4, %s, %s, %s, %s)",
                    (job_id, timestamp, timestamp, timestamp, timestamp),
                )

            process = subprocess.Popen(
                [str(SERVER)], cwd=ROOT, env=environment, stdout=api_log, stderr=api_log
            )
            wait_ready(base, process)
            deadline = time.time() + 10
            recovered_event = None
            job = None
            while time.time() < deadline:
                with database.connect() as connection:
                    row = connection.execute(
                        "SELECT status, stage, attempts FROM jobs WHERE id = %s", (job_id,)
                    ).fetchone()
                    events = connection.execute(
                        "SELECT event_type, payload_json FROM job_events WHERE job_id = %s ORDER BY id",
                        (job_id,),
                    ).fetchall()
                job = row
                for event_type, payload_json in events:
                    if event_type == "job.recovered":
                        recovered_event = payload_json
                        break
                if recovered_event is not None and row and row[2] >= 5:
                    break
                time.sleep(0.2)

            if recovered_event is None:
                raise RuntimeError("restart did not persist a job.recovered event")
            if recovered_event.get("status") != "queued" or recovered_event.get("stage") != "recovered":
                raise RuntimeError(f"recovery event payload is invalid: {recovered_event}")
            if not job or job[2] < 5:
                raise RuntimeError(f"recovered job was not claimed by the restarted worker: {job}")
            detail = None
            deadline = time.time() + 5
            while time.time() < deadline:
                detail = job_detail(base, job_id, admin_token)
                if detail.get("logs"):
                    break
                time.sleep(0.1)
            logs = (detail or {}).get("logs") or []
            if not any(entry.get("stage") == "loading_revision" for entry in logs):
                raise RuntimeError(f"job detail did not expose the persisted progress log: {detail}")
            print(
                "Service restart recovered the persisted running job, recorded its event, "
                f"resumed worker processing, and exposed its progress log (attempts={job[2]})."
            )
        finally:
            stop(process)
            api_log.close()


if __name__ == "__main__":
    main()
