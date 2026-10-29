#!/usr/bin/env python3
"""Real HTTP acceptance for unified async authorization and user passwords."""
import base64
import json
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from management_test_http import management_request, start_test_server
from postgres_test import PostgresTestSchema
from credential_test_fixtures import request_with_credentials, wait_authorization_job, user_connection_password, grant_group

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hx-async-groups-") as folder:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        admin = secrets.token_urlsafe(36)
        environment = os.environ | {"DATABASE_URL": database.url, "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{port}",
            "HYSTERIAX_PUBLIC_URL": base, "HYSTERIAX_ADMIN_TOKEN": admin,
            "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="), "RUST_LOG": "warn"}

        def raw_request(base, path, token, method, payload):
            request = management_request(base + path, method=method,
                data=None if payload is None else json.dumps(payload).encode(),
                headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
            try:
                with urllib.request.urlopen(request, timeout=20) as response:
                    return response.status, response.read()
            except urllib.error.HTTPError as error:
                return error.code, error.read()

        def expect(path, method="GET", payload=None, statuses=(200,)):
            status, body = request_with_credentials(raw_request, base, path, admin, method, payload)
            assert status in statuses, f"{method} {path}: {status}: {body[:250]!r}"
            return json.loads(body) if body else None

        def mutation(path, method, payload):
            receipt = expect(path, method, payload, (202,))
            assert set(receipt) == {"job_id", "resource_id", "status"} and receipt["status"] == "queued"
            wait_authorization_job(expect, receipt)
            return receipt

        def group(name, users, nodes):
            created = mutation("/api/v2/authorization-groups", "POST", {"name": name})
            path = "/api/v2/authorization-groups/" + created["resource_id"]
            if users or nodes:
                mutation(path, "PATCH", {"expected_revision": 1, "add_user_ids": users, "add_node_ids": nodes})
            return expect(path)

        with open(pathlib.Path(folder) / "server.log", "wb") as log:
            server = start_test_server([str(ROOT / "target/debug/hysteriax-server")], cwd=ROOT,
                env=environment, stdout=log, stderr=log)
            try:
                for _ in range(100):
                    try:
                        if raw_request(base, "/readyz", admin, "GET", None)[0] == 200:
                            break
                    except OSError:
                        pass
                    time.sleep(0.1)
                else:
                    raise RuntimeError("test service did not become ready")
                users = [expect("/api/v2/users", "POST", {"name": name}, (201,))["id"] for name in ["Alice", "Bob"]]
                catalog_path = "/api/v2/credentials?category=user&kind=user_credential"
                catalog = expect(catalog_path)
                assert catalog["total"] == 2
                assert {item["id"] for item in catalog["items"]} == {"user:" + user for user in users}
                assert all(item["reference_count"] == 0 for item in catalog["items"])
                nodes = [expect("/api/v2/nodes", "POST", {"name": name, "ssh_host": "127.0.0.1", "ssh_port": 22,
                    "ssh_username": "root", "ssh_auth_type": "password", "ssh_secret": "fixture-only",
                    "public_host": "example.test", "public_port": 443, "listen_addr": ":443"}, (201,)) for name in ["One", "Two"]]
                node_ids = [item["node"]["id"] for item in nodes]
                password = user_connection_password(database, environment["HYSTERIAX_MASTER_KEY"], users[0])
                first = group("First", [users[0]], node_ids)
                second = group("Overlap", [users[0], users[1]], [node_ids[0]])
                catalog = expect(catalog_path)
                assert catalog["total"] == 2
                assert {item["owner_user_id"]:item["reference_count"] for item in catalog["items"]} == {users[0]:2, users[1]:1}
                assert password == user_connection_password(database, environment["HYSTERIAX_MASTER_KEY"], users[0])

                def auth(user_password, index):
                    node = nodes[index]
                    return expect(f"/hy2/auth/{node_ids[index]}/{node['node_auth_token']}", "POST",
                        {"addr": "127.0.0.1:0", "auth": user_password, "tx": 0})["ok"]
                assert auth(password, 0) and auth(password, 1)
                bob_password = user_connection_password(database, environment["HYSTERIAX_MASTER_KEY"], users[1])
                assert auth(bob_password, 0) and not auth(bob_password, 1)
                mutation("/api/v2/authorization-groups/" + first["id"], "DELETE", {"expected_revision": first["revision"]})
                assert auth(password, 0) and not auth(password, 1)
                alice = expect("/api/v2/users/" + users[0])
                mutation(f"/api/v2/users/{users[0]}/authorization-groups", "PUT", {"expected_revision": alice["revision"], "group_ids": []})
                assert not auth(password, 0)
                bob = expect("/api/v2/users/" + users[1])
                rotated = expect(f"/api/v2/users/{users[1]}/credentials/rotate", "POST", {"expected_revision": bob["revision"]})
                assert not auth(bob_password, 0) and auth(rotated["credential"], 0)
                catalog = expect(catalog_path)
                assert catalog["total"] == 2
                assert {item["owner_user_id"]:item["reference_count"] for item in catalog["items"]} == {users[0]:0, users[1]:1}
                assert {item["id"] for item in catalog["items"]} == {"user:" + user for user in users}
                assert all("credential" not in item and item["metadata"] == {} for item in catalog["items"])
                tls = expect("/api/v2/credentials", "POST", {"name":"Server TLS","kind":"tls_identity","payload":{
                    "certificate":(ROOT/"tests/fixtures/credentials-test.crt").read_text(),
                    "private_key":(ROOT/"tests/fixtures/credentials-test.key").read_text()}}, (201,))
                ca = expect("/api/v2/credentials/ca-identities", "POST", {"name":"Async CA","source":"generated"}, (201,))
                prefix = f"credential://{tls['id']}/{tls['version']}/"
                mtls = expect("/api/v2/nodes", "POST", {"name":"Managed","ssh_host":"127.0.0.1","ssh_port":22,
                    "ssh_username":"root","ssh_auth_type":"password","ssh_secret":"fixture-only","public_host":"mtls.example.test",
                    "public_port":443,"listen_addr":":443","config":{"tls":{"cert":prefix+"certificate","key":prefix+"private_key"},
                    "mtls":{"mode":"managed","ca_credential_id":ca["id"],"ca_credential_version":1}}}, (201,))
                grant_group(expect, users[0], [mtls["node"]["id"]])
                info = expect("/api/v2/users/" + users[0])
                assert info["managed_certificates"][0]["versions"]
                assert user_connection_password(database, environment["HYSTERIAX_MASTER_KEY"], users[0]) == password
                with database.connect() as connection:
                    assert connection.execute("SELECT count(*) FROM authorization_changes WHERE NOT complete").fetchone()[0] == 0
                    assert connection.execute("SELECT count(*) FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='node_assignments' AND column_name IN ('credential_hash','credential_enc')").fetchone()[0] == 0
                print("Async authorization HTTP acceptance passed: uniform 202 receipts, real workers, source union, shared passwords, revocation and rotation.")
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
