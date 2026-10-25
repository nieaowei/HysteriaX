#!/usr/bin/env python3
"""Restart the service with an interrupted job and verify persisted recovery."""
from management_test_http import management_request, start_test_server

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
    request = management_request(
        base + f"/api/v2/jobs/{job_id}",
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
        process = start_test_server(
            [str(SERVER)], cwd=ROOT, env=environment, stdout=api_log, stderr=api_log
        )
        try:
            wait_ready(base, process)
            credential_request = management_request(base + "/api/v2/credentials", method="POST",
                headers={"Authorization": f"Bearer {admin_token}", "Content-Type": "application/json"},
                data=json.dumps({"name":"Recovery fixture SSH","kind":"ssh_password","payload":{"secret":"unused-password"}}).encode())
            with urllib.request.urlopen(credential_request) as response:
                credential_id = json.loads(response.read())["id"]
            # Create the node through the API so its encrypted configuration,
            # deployment snapshot and credential references remain valid at restart.
            node_request = management_request(base + "/api/v2/nodes", method="POST",
                headers={"Authorization": f"Bearer {admin_token}", "Content-Type": "application/json"},
                data=json.dumps({"name":"Recovery node","ssh_host":"127.0.0.1","ssh_port":22,
                    "ssh_username":"root","ssh_credential_id":credential_id,"ssh_credential_version":1,
                    "public_host":"node.example.test","public_port":443,"listen_addr":":443","config":{}}).encode())
            with urllib.request.urlopen(node_request) as response:
                node_id = json.loads(response.read())["node"]["id"]
            stop(process)
            process = None

            job_id = str(uuid.uuid4())
            timestamp = datetime(2026, 1, 1, tzinfo=timezone.utc)
            with database.connect() as connection:
                connection.execute(
                    "INSERT INTO jobs (id, kind, node_id, target_revision, status, stage, payload_json, attempts, "
                    "available_at, created_at, updated_at, started_at) "
                    "VALUES (%s, 'sync', %s, 1, 'running', 'interrupted', '{}', 4, %s, %s, %s, %s)",
                    (job_id, node_id, timestamp, timestamp, timestamp, timestamp),
                )
                # Interrupted before dispatch: its two owned leases expired and
                # there is no external operation intent that could be replayed.
                for resource in ("job:" + job_id, "node:" + node_id):
                    connection.execute(
                        "INSERT INTO resource_leases(resource_key,owner,generation,expires_at,work_id) "
                        "VALUES(%s,'interrupted-fixture',1,clock_timestamp()-interval '1 second',%s)",
                        (resource, job_id),
                    )

            process = start_test_server(
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
            with database.connect() as connection:
                generation = connection.execute("SELECT generation FROM resource_leases WHERE resource_key=%s", ("job:" + job_id,)).fetchone()[0]
            if generation <= 1:
                raise RuntimeError("recovered job did not acquire a new fencing generation")
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
