#!/usr/bin/env python3
"""Verify real process restart and pg_dump/pg_restore of published template state.

Requires a disposable PostgreSQL container, TEST_DATABASE_URL, and a role allowed
  to create/drop databases. Only randomly named test databases are touched.
"""
from management_test_http import management_request, start_test_server
import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True, help="Disposable PostgreSQL container holding TEST_DATABASE_URL")
    args = parser.parse_args()
    configured = os.environ.get("TEST_DATABASE_URL")
    if not configured: raise SystemExit("TEST_DATABASE_URL is required")
    parts = urllib.parse.urlsplit(configured)
    db_user = urllib.parse.unquote(parts.username or "postgres")
    prefix = "hx_template_" + uuid.uuid4().hex[:16]
    source_db, restored_db = prefix + "_source", prefix + "_restored"
    query = urllib.parse.urlencode([(k, v) for k, v in urllib.parse.parse_qsl(parts.query) if k != "options"])

    def database_url(name): return urllib.parse.urlunsplit((parts.scheme, parts.netloc, "/" + name, query, ""))
    def postgres(command, *arguments, data=None):
        result = subprocess.run(["docker", "exec", "-i", args.container, command, "-U", db_user, *arguments], input=data, capture_output=True)
        if result.returncode: raise RuntimeError(command + " failed: " + result.stderr.decode(errors="replace")[-1500:])
        return result.stdout

    subprocess.run(["cargo", "build", "-q"], cwd=ROOT, check=True)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0)); port = listener.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    admin = "hx_" + secrets.token_urlsafe(32)
    master = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
    environment = os.environ | {"HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{port}", "HYSTERIAX_PUBLIC_URL": base, "HYSTERIAX_MASTER_KEY": master, "HYSTERIAX_ADMIN_TOKEN": admin, "RUST_LOG": "warn"}
    environment.pop("REDIS_URL", None); environment.pop("HYSTERIAX_REDIS_URL", None)
    server = None; created = []
    with tempfile.TemporaryDirectory(prefix="hysteriax-template-lifecycle-") as folder:
        log = (Path(folder) / "server.log").open("w")

        def stop():
            nonlocal server
            if server is not None:
                server.terminate()
                try: server.wait(timeout=10)
                except subprocess.TimeoutExpired: server.kill(); server.wait()
                server = None

        def start(db):
            nonlocal server
            server = start_test_server([str(ROOT / "target/debug/hysteriax-server")], env=environment | {"DATABASE_URL": database_url(db)}, stdout=log, stderr=subprocess.STDOUT)
            for _ in range(150):
                if server.poll() is not None: raise RuntimeError("template lifecycle server exited during startup")
                try:
                    with urllib.request.urlopen(base + "/readyz", timeout=1) as response:
                        if response.status == 200: return
                except (OSError, urllib.error.URLError): time.sleep(0.2)
            raise TimeoutError("template lifecycle readiness timed out")

        def api(path, payload=None, method=None, expected=200):
            body = None if payload is None else json.dumps(payload).encode()
            request = management_request(base + "/api/v2" + path, data=body, method=method or ("GET" if body is None else "POST"), headers={"Authorization": "Bearer " + admin, "Content-Type": "application/json", "Cache-Control": "no-store"})
            try:
                with urllib.request.urlopen(request, timeout=10) as response: status, data = response.status, response.read()
            except urllib.error.HTTPError as error: status, data = error.code, error.read()
            if status != expected: raise AssertionError(f"{path}: expected HTTP {expected}, got {status}")
            return json.loads(data)

        try:
            postgres("createdb", source_db); created.append(source_db)
            start(source_db)
            user = api("/users", {"name": "Lifecycle fixture"}, expected=201)
            subscription = api(f"/users/{user['id']}/subscription", {"expected_revision": 1})
            secret = "fixture-template-secret-" + secrets.token_hex(16)
            document = {"schema_version": 1, "common": {"mixed-port": 8100, "secret": secret, "future": {"counter": 18446744073709551615}, "proxies": [], "proxy-groups": [{"name": "Select", "type": "select", "proxies": []}], "rules": ["MATCH,Select"]}, "platforms": {}, "injections": [{"group": "Select"}]}
            metadata = api("/subscription-templates", {"expected_revision": 0, "name": "Lifecycle published template", "format": "mihomo", "document": document}, expected=201)
            path = "/subscription-templates/" + metadata["id"]
            metadata = api(path + "/publish", {"expected_revision": metadata["revision"]})
            metadata = api(path, {"expected_revision": metadata["revision"], "is_default": True}, "PATCH")

            def download():
                with urllib.request.urlopen(base + f"/sub/{subscription['token']}?template={metadata['id']}&format=mihomo&platform=windows", timeout=10) as response: return response.read().decode()

            assert "mixed-port: 8100" in download()
            document["common"]["mixed-port"] = 8200
            metadata = api(path + "/draft", {"expected_revision": metadata["revision"], "document": document}, "PUT")
            assert "mixed-port: 8100" in download()
            metadata = api(path + "/publish", {"expected_revision": metadata["revision"]})
            assert "mixed-port: 8200" in download()
            document["injections"][0]["group"] = "missing"
            metadata = api(path + "/draft", {"expected_revision": metadata["revision"], "document": document}, "PUT")
            api(path + "/publish", {"expected_revision": metadata["revision"]}, expected=422)
            revision = metadata["revision"]
            stop(); start(source_db)
            restarted = api(path)
            assert restarted["revision"] == revision and restarted["published_version"] == 2 and restarted["is_default"]
            assert api(path + "/draft")["document"]["injections"][0]["group"] == "missing"
            assert "mixed-port: 8200" in download()
            metadata = api(path + "/rollback", {"expected_revision": revision, "version": 1})
            assert metadata["published_version"] == 3 and "mixed-port: 8100" in download()
            expected_document = api(path + "/draft")["document"]
            stop()
            archive = postgres("pg_dump", "--format=custom", "--compress=0", "--no-owner", "--no-privileges", source_db)
            assert secret.encode() not in archive, "template bodies must remain encrypted in the database backup"
            postgres("createdb", restored_db); created.append(restored_db)
            postgres("pg_restore", "--exit-on-error", "--no-owner", "--no-privileges", "--dbname", restored_db, data=archive)
            start(restored_db)
            restored = api(path)
            assert restored["is_default"] and restored["published_version"] == 3
            assert api(path + "/draft")["document"] == expected_document
            assert api(path + "/versions")["total"] == 3
            assert "mixed-port: 8100" in download()
            assert api(path + "/export")["document"]["common"]["future"]["counter"] == 18446744073709551615
            print("Template restart, draft isolation, rollback/new-version identity, encrypted pg_dump, and full pg_restore passed.")
        finally:
            stop(); log.close()
            for db in reversed(created): postgres("dropdb", "--if-exists", db)


if __name__ == "__main__": main()
