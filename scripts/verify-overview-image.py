#!/usr/bin/env python3
"""Boot a locally built monitoring image against an isolated PostgreSQL schema."""
import argparse
import base64
import datetime as dt
import json
import os
import secrets
import socket
import subprocess
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit, urlunsplit
import uuid
from postgres_test import PostgresTestSchema

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--image", default="hysteriax-server:overview-20261003")
args = parser.parse_args()
with PostgresTestSchema() as database:
    parts = urlsplit(database.url)
    # Docker Desktop/OrbStack exposes host-loopback test PostgreSQL through this name.
    host = "host.docker.internal"
    userinfo = parts.netloc.rsplit("@", 1)[0]
    container_database = urlunsplit(parts._replace(netloc=f"{userinfo}@{host}:{parts.port}"))
    container = "hysteriax-overview-image-" + uuid.uuid4().hex[:10]
    token = secrets.token_urlsafe(48)
    key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    command = ["docker", "run", "-d", "--name", container, "-p", f"127.0.0.1:{port}:8080", "-e", f"DATABASE_URL={container_database}", "-e", f"HYSTERIAX_ADMIN_TOKEN={token}", "-e", f"HYSTERIAX_MASTER_KEY={key}", args.image]
    if os.uname().sysname == "Linux": command[3:3] = ["--add-host", "host.docker.internal:host-gateway"]
    try:
        subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
        base = f"http://127.0.0.1:{port}"
        def get(path, authenticated=True):
            request = urllib.request.Request(base + path, headers={"Authorization": f"Bearer {token}"} if authenticated else {})
            with urllib.request.urlopen(request, timeout=10) as response:
                return json.load(response)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                if get("/readyz", False)["status"] == "ready": break
            except (OSError, urllib.error.URLError): pass
            time.sleep(0.5)
        else: raise TimeoutError("container readiness failed")
        assert {"overview_monitoring", "job_retry_links"}.issubset(get("/api/v1/version")["features"])
        request = urllib.request.Request(base + "/api/v1/jobs/00000000-0000-0000-0000-000000000000/retry", data=b'{"expected_revision":0}', headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
        try:
            urllib.request.urlopen(request, timeout=10)
            raise AssertionError("missing retry parent must be rejected")
        except urllib.error.HTTPError as error:
            assert error.code == 404

        overview = get("/api/v1/overview")
        assert overview["node_count"] == 0 and overview["online_users"] is None
        for span, days, count in [("24h", 1, 24), ("7d", 7, 7), ("30d", 30, 30)]:
            for source in ["users", "network"]:
                history = get(f"/api/v1/overview/history?range={span}&timezone=UTC&source={source}")
                buckets = history["buckets"]
                assert len(buckets) == count
                assert all(b["tx_bytes"] is None for b in buckets)
                parse = lambda value: dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
                assert parse(buckets[-1]["end"]) == parse(history["generated_at"])
                assert parse(buckets[-1]["end"]) - parse(buckets[0]["start"]) == dt.timedelta(days=days)
                assert all(a["end"] == b["start"] for a, b in zip(buckets, buckets[1:]))
        version = subprocess.check_output(["docker", "exec", container, "/usr/local/bin/hysteria", "version"], text=True)
        assert "v2.12.3" in version
        print("Container startup, PostgreSQL migration, overview/history contracts and pinned probe client passed")
    finally:
        subprocess.run(["docker", "rm", "-f", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
