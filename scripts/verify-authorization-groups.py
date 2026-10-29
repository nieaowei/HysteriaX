#!/usr/bin/env python3
"""Exercise group-only authorization through real HTTP and isolated PostgreSQL."""
from management_test_http import management_request, start_test_server
import base64
import json
from urllib.parse import quote
import os
import pathlib
import secrets
import subprocess
import tempfile
import urllib.error
import urllib.request

from postgres_test import PostgresTestSchema
import socket
import time
from credential_test_fixtures import request_with_credentials

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    with PostgresTestSchema() as db, tempfile.TemporaryDirectory(prefix="hx-groups-") as folder:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        admin = secrets.token_urlsafe(36)
        env = dict(os.environ, DATABASE_URL=db.url, HYSTERIAX_LISTEN_ADDR=f"127.0.0.1:{port}",
                   HYSTERIAX_PUBLIC_URL=base, HYSTERIAX_ADMIN_TOKEN=admin,
                   HYSTERIAX_MASTER_KEY=base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="), RUST_LOG="warn")

        def raw_request(_base, path, token=admin, method="GET", payload=None):
            request = management_request(_base + path, method=method,
                data=None if payload is None else json.dumps(payload).encode(),
                headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
            try:
                with urllib.request.urlopen(request, timeout=10) as response:
                    return response.status, response.read()
            except urllib.error.HTTPError as error:
                return error.code, error.read()

        def expect(path, method="GET", payload=None, status=200, fixtures=False):
            if fixtures:
                code, body = request_with_credentials(raw_request, base, path, admin, method, payload)
            else:
                code, body = raw_request(base, path, admin, method, payload)
            assert code == status, f"{method} {path}: expected {status}, got {code}: {body[:250]!r}"
            return json.loads(body) if body else None

        def group(name, users, nodes):
            receipt = expect("/api/v2/authorization-groups", "POST", {"name": name}, 201)
            assert receipt == {"revocation_job_ids": []}
            current = next(item for item in expect("/api/v2/authorization-groups?q=" + quote(name, safe=""))["items"] if item["name"] == name)
            if users or nodes:
                patch = {"expected_revision": current["revision"]}
                if users: patch["add_user_ids"] = users
                if nodes: patch["add_node_ids"] = nodes
                assert expect("/api/v2/authorization-groups/" + current["id"], "PATCH", patch) == {"revocation_job_ids": []}
                current = expect("/api/v2/authorization-groups/" + current["id"])
            return current

        def remove(group_data):
            path = "/api/v2/authorization-groups/" + group_data["id"]
            return expect(path, "DELETE", {"expected_revision": group_data["revision"]})

        log = open(pathlib.Path(folder) / "server.log", "wb")
        server = start_test_server([str(ROOT / "target/debug/hysteriax-server")], cwd=ROOT, env=env, stdout=log, stderr=log)
        try:
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError("temporary server exited before readiness")
                try:
                    if raw_request(base, "/readyz")[0] == 200:
                        break
                except OSError:
                    pass
                time.sleep(.1)
            else:
                raise RuntimeError("server readiness timeout")
            assert "authorization_groups" in expect("/api/v2/version")["features"]
            node = expect("/api/v2/nodes", "POST", {"name": "Group node", "ssh_host": "127.0.0.1",
                "ssh_port": 22, "ssh_username": "root", "ssh_auth_type": "password", "ssh_secret": "fixture",
                "public_host": "node.example.test", "public_port": 443, "listen_addr": ":443", "config": {}}, 201, fixtures=True)["node"]["id"]
            users = [expect("/api/v2/users", "POST", {"name": name}, 201)["id"] for name in ("Alice", "Bob")]
            first = group("Both users", users, [node])
            with db.connect() as connection:
                original = connection.execute("SELECT user_id,credential_hash FROM node_assignments ORDER BY user_id").fetchall()
            second = group("Overlap", [users[0]], [node])
            alice = expect("/api/v2/users/" + users[0])
            assert len(alice["assignments"][0]["source_groups"]) == 2
            # Retired writes must fail without altering authorization state.
            for method, path, payload in (("POST", f"/api/v2/users/{users[0]}/assignments", {"expected_revision": alice["revision"], "node_id": node}),
                                          ("DELETE", f"/api/v2/users/{users[0]}/assignments/{node}", {"expected_revision": alice["revision"]})):
                code, _ = raw_request(base, path, admin, method, payload)
                assert 400 <= code < 500
            remove(first)
            assert len(expect("/api/v2/users/" + users[0])["assignments"]) == 1
            assert expect("/api/v2/users/" + users[1])["assignments"] == []
            with db.connect() as connection:
                current = connection.execute("SELECT user_id,credential_hash FROM node_assignments").fetchall()
            assert current == [pair for pair in original if pair[0] == users[0]]
            # A stale user revision must reject membership changes atomically.
            alice = expect("/api/v2/users/" + users[0])
            membership = {"expected_revision": alice["revision"], "group_ids": []}
            expect(f"/api/v2/users/{users[0]}", "PATCH", {"expected_revision": alice["revision"], "name": "Alice edited"})
            expect(f"/api/v2/users/{users[0]}/authorization-groups", "PUT", membership, 409)
            assert len(expect("/api/v2/users/" + users[0])["assignments"]) == 1
            final = remove(second)
            assert set(final) == {"revocation_job_ids"} and len(final["revocation_job_ids"]) == 1
            assert expect("/api/v2/users/" + users[0])["assignments"] == []
            # Regrant produces exactly one new pair, and old pending revocation cannot remove it.
            regrant = group("Regrant", [users[0]], [node])
            with db.connect() as connection:
                count = connection.execute("SELECT count(*) FROM node_assignments WHERE user_id=%s AND node_id=%s", (users[0], node)).fetchone()[0]
                stale_kicks = connection.execute("SELECT count(*) FROM kick_requests WHERE user_id=%s AND node_id=%s AND state NOT IN ('completed','cancelled') AND reasons ? 'authorization_group_removed'", (users[0], node)).fetchone()[0]
            assert count == 1 and stale_kicks == 0
            # User membership updates and group editing are genuine bulk operations.
            empty = group("Empty group", [], [node])
            alice = expect("/api/v2/users/" + users[0])
            membership = {"expected_revision": alice["revision"], "group_ids": [regrant["id"], empty["id"]]}
            joined = expect(f"/api/v2/users/{users[0]}/authorization-groups", "PUT", membership)
            assert joined["additions_count"] == 0 and joined["removals_count"] == 0
            assert len(joined["assignments"][0]["source_groups"]) == 2
            empty = expect("/api/v2/authorization-groups/" + empty["id"])
            draft = {"expected_revision": empty["revision"], "name": "Expanded group",
                     "add_user_ids": [users[1]]}
            path = "/api/v2/authorization-groups/" + empty["id"]
            expanded = expect(path, "PATCH", draft)
            assert expanded == {"revocation_job_ids": []}
            assert len(expect("/api/v2/users/" + users[1])["assignments"]) == 1
            bob = expect("/api/v2/users/" + users[1])
            membership = {"expected_revision": bob["revision"], "group_ids": []}
            expect(f"/api/v2/users/{users[1]}/authorization-groups", "PUT", membership)
            assert expect("/api/v2/users/" + users[1])["assignments"] == []
            certificate = (ROOT / "tests/fixtures/credentials-test.crt").read_text()
            private_key = (ROOT / "tests/fixtures/credentials-test.key").read_text()
            identity = expect("/api/v2/credentials", "POST", {"name": "Server identity", "kind": "tls_identity",
                "payload": {"certificate": certificate, "private_key": private_key}}, 201)
            ca = expect("/api/v2/credentials/ca-identities", "POST", {"name": "Shared client CA", "source":"generated", "idempotency_key":"group-live-ca"}, 201)
            prefix = f"credential://{identity['id']}/{identity['version']}/"
            mtls_node = expect("/api/v2/nodes", "POST", {"name": "mTLS group node", "ssh_host": "127.0.0.1",
                "ssh_port": 22, "ssh_username": "root", "ssh_auth_type": "password", "ssh_secret": "fixture",
                "public_host": "mtls.example.test", "public_port": 443, "listen_addr": ":443", "config": {"tls": {
                    "cert": prefix + "certificate", "key": prefix + "private_key"}, "mtls":{"mode":"managed","ca_credential_id":ca["id"],"ca_credential_version":1}}}, 201, fixtures=True)["node"]
            assert mtls_node["mtls_required"] is True
            invalid = {"name":"Invalid","user_ids":users,"node_ids":[mtls_node["id"]],"mtls_bindings":[]}
            code, _ = raw_request(base, "/api/v2/authorization-groups", admin, "POST", invalid)
            assert 400 <= code < 500
            result = group("mTLS complete", users, [mtls_node["id"]])
            actor_id=expect("/api/v2/version")["current_admin_token_id"]
            with db.connect() as connection:
                audit=connection.execute("SELECT actor,detail_json FROM audit_records WHERE action='authorization_group.updated' AND entity_id=%s",(result["id"],)).fetchone()
            assert audit[0] == actor_id and len(audit[1]["managed_identities"]) == 2
            assert all(item["ca_versions"] == [1] and item["affected_node_ids"] == [mtls_node["id"]] for item in audit[1]["managed_identities"])

            with db.connect() as connection:
                actual = connection.execute("SELECT user_id,ca_id FROM mtls_user_identities ORDER BY user_id").fetchall()
            assert dict(actual) == {user_id:ca["id"] for user_id in users}
            for user_id in users:
                info=expect("/api/v2/users/"+user_id)
                assert info["managed_certificates"][0]["ca_credential_id"] == ca["id"]
            # Deleting entities cascades memberships and updates group revisions.
            mtls_group = result
            bob = expect("/api/v2/users/" + users[1])
            expect(f"/api/v2/users/{users[1]}?expected_revision={bob['revision']}", "DELETE", status=204)
            latest = expect("/api/v2/authorization-groups/" + mtls_group["id"])
            assert latest["revision"] > mtls_group["revision"] and latest["user_count"] == 1
            old_group = expect("/api/v2/authorization-groups/" + regrant["id"])
            expect(f"/api/v2/nodes/{node}?expected_revision=1", "DELETE", status=204)
            latest = expect("/api/v2/authorization-groups/" + regrant["id"])
            assert latest["revision"] > old_group["revision"] and latest["node_count"] == 0
            print("Authorization groups HTTP acceptance passed: union, credentials, last-source revocation, stale revision, retired writes and regrant.")
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
            log.close()


if __name__ == "__main__":
    main()
