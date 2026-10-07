#!/usr/bin/env python3
"""Exercise group-only authorization through real HTTP and isolated PostgreSQL."""
import base64
import json
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
            request = urllib.request.Request(_base + path, method=method,
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

        def group(name, users, nodes, bindings=None):
            draft = {"action": "create", "name": name, "user_ids": users, "node_ids": nodes,
                     "mtls_bindings": bindings or []}
            preview = expect("/api/v1/authorization-groups/preview", "POST", draft)
            assert not preview["missing_mtls"]
            draft.pop("action")
            draft["preview_token"] = preview["preview_token"]
            return expect("/api/v1/authorization-groups", "POST", draft, 201)

        def remove(group_data):
            path = "/api/v1/authorization-groups/" + group_data["id"]
            preview = expect(path + "/preview", "POST", {"action": "delete", "expected_revision": group_data["revision"]})
            return expect(path, "DELETE", {"expected_revision": group_data["revision"], "preview_token": preview["preview_token"]})

        log = open(pathlib.Path(folder) / "server.log", "wb")
        server = subprocess.Popen([str(ROOT / "target/debug/hysteriax-server")], cwd=ROOT, env=env, stdout=log, stderr=log)
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
            assert "authorization_groups" in expect("/api/v1/version")["features"]
            node = expect("/api/v1/nodes", "POST", {"name": "Group node", "ssh_host": "127.0.0.1",
                "ssh_port": 22, "ssh_username": "root", "ssh_auth_type": "password", "ssh_secret": "fixture",
                "public_host": "node.example.test", "public_port": 443, "listen_addr": ":443", "config": {}}, 201, fixtures=True)["node"]["id"]
            users = [expect("/api/v1/users", "POST", {"name": name}, 201)["id"] for name in ("Alice", "Bob")]
            first = group("Both users", users, [node])
            assert first["additions_count"] == 2 and len(first["created_credentials"]) == 2
            with db.connect() as connection:
                original = connection.execute("SELECT user_id,credential_hash FROM node_assignments ORDER BY user_id").fetchall()
            second = group("Overlap", [users[0]], [node])
            assert second["additions_count"] == 0 and second["created_credentials"] == []
            alice = expect("/api/v1/users/" + users[0])
            assert len(alice["assignments"][0]["source_groups"]) == 2
            # Retired writes must fail without altering authorization state.
            for method, path, payload in (("POST", f"/api/v1/users/{users[0]}/assignments", {"expected_revision": alice["revision"], "node_id": node}),
                                          ("DELETE", f"/api/v1/users/{users[0]}/assignments/{node}", {"expected_revision": alice["revision"]})):
                code, _ = raw_request(base, path, admin, method, payload)
                assert 400 <= code < 500
            remove(first["group"])
            assert len(expect("/api/v1/users/" + users[0])["assignments"]) == 1
            assert expect("/api/v1/users/" + users[1])["assignments"] == []
            with db.connect() as connection:
                current = connection.execute("SELECT user_id,credential_hash FROM node_assignments").fetchall()
            assert current == [pair for pair in original if pair[0] == users[0]]
            # Preview must reject a concurrent user mutation atomically.
            alice = expect("/api/v1/users/" + users[0])
            membership = {"expected_revision": alice["revision"], "group_ids": []}
            preview = expect(f"/api/v1/users/{users[0]}/authorization-groups/preview", "POST", membership)
            expect(f"/api/v1/users/{users[0]}", "PATCH", {"expected_revision": alice["revision"], "name": "Alice edited"})
            membership["preview_token"] = preview["preview_token"]
            expect(f"/api/v1/users/{users[0]}/authorization-groups", "PUT", membership, 409)
            assert len(expect("/api/v1/users/" + users[0])["assignments"]) == 1
            final = remove(second["group"])
            assert final["removals_count"] == 1
            assert expect("/api/v1/users/" + users[0])["assignments"] == []
            # Regrant produces exactly one new pair, and old pending revocation cannot remove it.
            regrant = group("Regrant", [users[0]], [node])["group"]
            with db.connect() as connection:
                count = connection.execute("SELECT count(*) FROM node_assignments WHERE user_id=%s AND node_id=%s", (users[0], node)).fetchone()[0]
                stale_kicks = connection.execute("SELECT count(*) FROM kick_requests WHERE user_id=%s AND node_id=%s AND state NOT IN ('completed','cancelled') AND reasons ? 'authorization_group_removed'", (users[0], node)).fetchone()[0]
            assert count == 1 and stale_kicks == 0
            # User membership updates and group editing are genuine bulk operations.
            empty = group("Empty group", [], [node])["group"]
            alice = expect("/api/v1/users/" + users[0])
            membership = {"expected_revision": alice["revision"], "group_ids": [regrant["id"], empty["id"]]}
            preview = expect(f"/api/v1/users/{users[0]}/authorization-groups/preview", "POST", membership)
            assert preview["additions_count"] == 0 and preview["removals_count"] == 0
            membership["preview_token"] = preview["preview_token"]
            joined = expect(f"/api/v1/users/{users[0]}/authorization-groups", "PUT", membership)
            assert len(joined["assignments"][0]["source_groups"]) == 2
            empty = expect("/api/v1/authorization-groups/" + empty["id"])
            draft = {"action": "update", "expected_revision": empty["revision"], "name": "Expanded group",
                     "user_ids": users, "node_ids": [node]}
            path = "/api/v1/authorization-groups/" + empty["id"]
            preview = expect(path + "/preview", "POST", draft)
            assert preview["additions_count"] == 1
            draft.pop("action")
            draft["preview_token"] = preview["preview_token"]
            expanded = expect(path, "PUT", draft)
            assert expanded["additions_count"] == 1
            bob = expect("/api/v1/users/" + users[1])
            membership = {"expected_revision": bob["revision"], "group_ids": []}
            preview = expect(f"/api/v1/users/{users[1]}/authorization-groups/preview", "POST", membership)
            membership["preview_token"] = preview["preview_token"]
            expect(f"/api/v1/users/{users[1]}/authorization-groups", "PUT", membership)
            assert expect("/api/v1/users/" + users[1])["assignments"] == []
            certificate = (ROOT / "tests/fixtures/credentials-test.crt").read_text()
            private_key = (ROOT / "tests/fixtures/credentials-test.key").read_text()
            identity = expect("/api/v1/credentials", "POST", {"name": "Server identity", "kind": "tls_identity",
                "payload": {"certificate": certificate, "private_key": private_key}}, 201)
            ca = expect("/api/v1/credentials", "POST", {"name": "Client CA", "kind": "ca_certificate",
                "payload": {"content": certificate}}, 201)
            prefix = f"credential://{identity['id']}/{identity['version']}/"
            mtls_node = expect("/api/v1/nodes", "POST", {"name": "mTLS group node", "ssh_host": "127.0.0.1",
                "ssh_port": 22, "ssh_username": "root", "ssh_auth_type": "password", "ssh_secret": "fixture",
                "public_host": "mtls.example.test", "public_port": 443, "listen_addr": ":443", "config": {"tls": {
                    "cert": prefix + "certificate", "key": prefix + "private_key",
                    "clientCA": f"credential://{ca['id']}/{ca['version']}/content"}}}, 201, fixtures=True)["node"]
            assert mtls_node["mtls_required"] is True
            draft = {"action": "create", "name": "mTLS users", "user_ids": users, "node_ids": [mtls_node["id"]]}
            preview = expect("/api/v1/authorization-groups/preview", "POST", draft)
            assert len(preview["missing_mtls"]) == 2
            commit = dict(draft, preview_token=preview["preview_token"])
            commit.pop("action")
            expect("/api/v1/authorization-groups", "POST", commit, 422)
            assert expect("/api/v1/users/" + users[1])["assignments"] == []
            bindings = []
            for user_id in users:
                personal = expect("/api/v1/credentials", "POST", {"name": "Personal identity", "kind": "tls_identity",
                    "owner_user_id": user_id, "payload": {"certificate": certificate, "private_key": private_key}}, 201)
                bindings.append({"user_id": user_id, "node_id": mtls_node["id"], "credential_id": personal["id"],
                                 "credential_version": personal["version"]})
            wrong = [dict(bindings[0], user_id=users[1]), bindings[0]]
            invalid = dict(draft, mtls_bindings=wrong)
            code, _ = raw_request(base, "/api/v1/authorization-groups/preview", admin, "POST", invalid)
            assert 400 <= code < 500
            result = group("mTLS complete", users, [mtls_node["id"]], bindings)
            assert result["additions_count"] == 2
            with db.connect() as connection:
                actual = connection.execute("SELECT user_id,mtls_credential_id FROM node_assignments WHERE node_id=%s ORDER BY user_id", (mtls_node["id"],)).fetchall()
            assert dict(actual) == {item["user_id"]: item["credential_id"] for item in bindings}
            # Deleting entities cascades memberships and updates group revisions.
            mtls_group = result["group"]
            bob = expect("/api/v1/users/" + users[1])
            expect(f"/api/v1/users/{users[1]}?expected_revision={bob['revision']}", "DELETE", status=204)
            latest = expect("/api/v1/authorization-groups/" + mtls_group["id"])
            assert latest["revision"] > mtls_group["revision"] and latest["user_count"] == 1
            old_group = expect("/api/v1/authorization-groups/" + regrant["id"])
            expect(f"/api/v1/nodes/{node}?expected_revision=1", "DELETE", status=204)
            latest = expect("/api/v1/authorization-groups/" + regrant["id"])
            assert latest["revision"] > old_group["revision"] and latest["node_count"] == 0
            print("Authorization groups HTTP acceptance passed: union, credentials, last-source revocation, stale preview, retired writes and regrant.")
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
