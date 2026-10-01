#!/usr/bin/env python3
"""Check two-node authentication isolation and per-node credential rotation."""

import base64
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
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


def request(base, path, token=None, method="GET", payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    headers = {}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)

    with tempfile.TemporaryDirectory(prefix="hysteriax-auth-isolation-") as temporary:
        temp = pathlib.Path(temporary)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]

        base = f"http://127.0.0.1:{port}"
        admin = "hx_" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        master_key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        environment = os.environ.copy()
        environment.update(
            {
                "DATABASE_URL": f"sqlite://{temp / 'service.db'}?mode=rwc",
                "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{port}",
                "HYSTERIAX_PUBLIC_URL": "https://management.example.test",
                "HYSTERIAX_ADMIN_TOKEN": admin,
                "HYSTERIAX_MASTER_KEY": master_key,
                "RUST_LOG": "warn",
            }
        )

        with open(temp / "server.log", "wb") as log:
            server = subprocess.Popen(
                [str(SERVER)], cwd=ROOT, env=environment, stdout=log, stderr=log
            )

            def expect(path, method="GET", payload=None, status=200, token=admin):
                code, body = request(base, path, token, method, payload)
                if code != status:
                    raise RuntimeError(f"{method} {path} returned HTTP {code}")
                return json.loads(body) if body else None

            def create_node(name, config=None):
                result = expect(
                    "/api/v1/nodes",
                    "POST",
                    {
                        "name": name,
                        "ssh_host": "127.0.0.1",
                        "ssh_port": 22,
                        "ssh_username": "root",
                        "ssh_auth_type": "private_key",
                        "ssh_secret": "unconnected-test-key",
                        "public_host": "node.example.test",
                        "public_port": 443,
                        "listen_addr": ":443",
                        "config": config or {},
                    },
                    201,
                )
                return result["node"]["id"], result["node_auth_token"]

            def upload_resource(node_id, name, resource_kind, content):
                return expect(
                    f"/api/v1/nodes/{node_id}/resources",
                    "POST",
                    {
                        "name": name,
                        "resource_kind": resource_kind,
                        "content_base64": base64.b64encode(content).decode(),
                    },
                    201,
                )["id"]

            def create_user(name, quota_bytes=None):
                payload = {"name": name}
                if quota_bytes is not None:
                    payload["quota_bytes"] = quota_bytes
                return expect("/api/v1/users", "POST", payload, 201)["id"]

            def assign(user_id, node_id, revision, certificate=None, private_key=None):
                payload = {"expected_revision": revision, "node_id": node_id}
                if certificate is not None:
                    payload["client_certificate"] = certificate
                    payload["client_private_key"] = private_key
                return expect(
                    f"/api/v1/users/{user_id}/assignments",
                    "POST",
                    payload,
                    201,
                )["hy2_credential"]

            def auth(node_id, node_token, credential):
                status, body = request(
                    base,
                    f"/hy2/auth/{node_id}/{node_token}",
                    method="POST",
                    payload={"addr": "127.0.0.1:0", "auth": credential, "tx": 0},
                )
                if status != 200:
                    raise RuntimeError("Hysteria HTTP auth endpoint returned an unexpected status")
                return json.loads(body)

            try:
                ready = False
                for _ in range(30):
                    if server.poll() is not None:
                        raise RuntimeError("temporary management service exited")
                    try:
                        code, _ = request(base, "/readyz")
                        if code == 200:
                            ready = True
                            break
                    except (OSError, urllib.error.URLError):
                        pass
                    time.sleep(0.5)
                if not ready:
                    raise RuntimeError("temporary management service did not become ready")

                token_label = "令牌" * 50
                admin_token = expect(
                    "/api/v1/admin/tokens",
                    "POST",
                    {"label": token_label},
                    201,
                )
                if admin_token["label"] != token_label or not admin_token["token"]:
                    raise RuntimeError("administrator token creation lost its Unicode label or one-time value")
                token_list_status, token_list_body = request(
                    base, "/api/v1/admin/tokens", admin_token["token"]
                )
                token_list = json.loads(token_list_body)
                if (
                    token_list_status != 200
                    or not any(item.get("id") == admin_token["id"] for item in token_list)
                    or any("token" in item for item in token_list)
                ):
                    raise RuntimeError("new administrator token could not list metadata without exposing token values")
                revoke_status, _ = request(
                    base,
                    f"/api/v1/admin/tokens/{admin_token['id']}",
                    admin,
                    "DELETE",
                )
                revoked_status, _ = request(
                    base, "/api/v1/version", admin_token["token"]
                )
                if revoke_status != 204 or revoked_status != 401:
                    raise RuntimeError("administrator token revocation did not reject the revoked token")

                client_certificate = temp / "client-cert.pem"
                client_private_key = temp / "client-key.pem"
                wrong_private_key = temp / "wrong-client-key.pem"
                subprocess.run(
                    [
                        "openssl",
                        "req",
                        "-x509",
                        "-newkey",
                        "rsa:2048",
                        "-nodes",
                        "-keyout",
                        str(client_private_key),
                        "-out",
                        str(client_certificate),
                        "-days",
                        "2",
                        "-subj",
                        "/CN=HysteriaX mTLS fixture",
                        "-addext",
                        "extendedKeyUsage=clientAuth,serverAuth",
                    ],
                    check=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                subprocess.run(
                    [
                        "openssl", "genpkey", "-algorithm", "RSA", "-out", str(wrong_private_key),
                        "-pkeyopt", "rsa_keygen_bits:2048",
                    ],
                    check=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                client_certificate_pem = client_certificate.read_text()
                client_private_key_pem = client_private_key.read_text()

                resource_node, _ = create_node("TLS resource validation node")
                certificate_id = upload_resource(
                    resource_node,
                    "server-certificate.pem",
                    "certificate",
                    client_certificate.read_bytes(),
                )
                private_key_id = upload_resource(
                    resource_node,
                    "server-private-key.pem",
                    "private_key",
                    client_private_key.read_bytes(),
                )
                mismatched_key_id = upload_resource(
                    resource_node,
                    "mismatched-private-key.pem",
                    "private_key",
                    wrong_private_key.read_bytes(),
                )
                valid_tls_config = {
                    "tls": {
                        "cert": f"resource://{certificate_id}",
                        "key": f"resource://{private_key_id}",
                    }
                }
                valid_tls_status, _ = request(
                    base,
                    f"/api/v1/nodes/{resource_node}",
                    admin,
                    "PATCH",
                    {"expected_revision": 1, "config": valid_tls_config},
                )
                if valid_tls_status != 200:
                    raise RuntimeError("a matching uploaded server certificate/key pair was rejected")
                mismatched_tls_config = {
                    "tls": {
                        "cert": f"resource://{certificate_id}",
                        "key": f"resource://{mismatched_key_id}",
                    }
                }
                mismatched_tls_status, _ = request(
                    base,
                    f"/api/v1/nodes/{resource_node}",
                    admin,
                    "PATCH",
                    {"expected_revision": 2, "config": mismatched_tls_config},
                )
                if mismatched_tls_status != 400:
                    raise RuntimeError("a mismatched uploaded server certificate/key pair was accepted")

                mtls_config = {
                    "tls": {
                        "cert": "/etc/hysteriax/server.pem",
                        "key": "/etc/hysteriax/server-key.pem",
                        "clientCA": "/etc/hysteriax/client-ca.pem",
                    }
                }
                node1, token1 = create_node("Isolation node one", mtls_config)
                node2, token2 = create_node("Isolation node two")
                user1 = create_user("Only node one")
                user2 = create_user("Only node two", quota_bytes=250_000)
                shared = create_user("Assigned to both nodes")
                mismatched_status, _ = request(
                    base,
                    f"/api/v1/users/{user1}/assignments",
                    admin,
                    "POST",
                    {
                        "expected_revision": 1,
                        "node_id": node1,
                        "client_certificate": client_certificate_pem,
                        "client_private_key": wrong_private_key.read_text(),
                    },
                )
                if mismatched_status != 400:
                    raise RuntimeError("mTLS assignment accepted a certificate/key mismatch")
                missing_cert_status, _ = request(
                    base,
                    f"/api/v1/users/{user1}/assignments",
                    admin,
                    "POST",
                    {"expected_revision": 1, "node_id": node1},
                )
                if missing_cert_status != 400:
                    raise RuntimeError("mTLS node accepted an assignment without client credentials")
                credential1 = assign(
                    user1, node1, 1, client_certificate_pem, client_private_key_pem
                )
                credential2 = assign(user2, node2, 1)
                shared1 = assign(
                    shared, node1, 1, client_certificate_pem, client_private_key_pem
                )
                shared2 = assign(shared, node2, 2)

                rejected_enable_status, _ = request(
                    base,
                    f"/api/v1/nodes/{node2}",
                    admin,
                    "PATCH",
                    {
                        "expected_revision": 1,
                        "config": {
                            "tls": {
                                "cert": "/etc/hysteriax/server.pem",
                                "key": "/etc/hysteriax/server-key.pem",
                                "clientCA": "/etc/hysteriax/client-ca.pem",
                            }
                        },
                    },
                )
                if rejected_enable_status != 409:
                    raise RuntimeError("mTLS configuration was enabled before all assignments had certificates")

                for user_id, expected_revision in ((user2, 2), (shared, 3)):
                    status, _ = request(
                        base,
                        f"/api/v1/users/{user_id}/assignments/{node2}",
                        admin,
                        "PUT",
                        {
                            "expected_revision": expected_revision,
                            "client_certificate": client_certificate_pem,
                            "client_private_key": client_private_key_pem,
                        },
                    )
                    if status != 200:
                        raise RuntimeError("per-node mTLS certificate update failed")

                enabled_mtls_status, _ = request(
                    base,
                    f"/api/v1/nodes/{node2}",
                    admin,
                    "PATCH",
                    {
                        "expected_revision": 1,
                        "config": {
                            "tls": {
                                "cert": "/etc/hysteriax/server.pem",
                                "key": "/etc/hysteriax/server-key.pem",
                                "clientCA": "/etc/hysteriax/client-ca.pem",
                            }
                        },
                    },
                )
                if enabled_mtls_status != 200:
                    raise RuntimeError("mTLS configuration was not enabled after client credentials were set")

                usage = expect(f"/api/v1/users/{user2}/usage")
                if usage["data_freshness"]["status"] != "not_collected":
                    raise RuntimeError("un-deployed test node should report traffic as not collected")
                pending_job_id = str(uuid.uuid4())
                future_time = "2099-01-01T00:00:00Z"
                with sqlite3.connect(temp / "service.db", timeout=10) as database:
                    database.execute(
                        "INSERT INTO jobs (id, kind, node_id, target_revision, payload_json, status, stage, available_at, created_at, updated_at) "
                        "VALUES (?, 'kick', ?, NULL, ?, 'queued', 'retry_wait', ?, ?, ?)",
                        (
                            pending_job_id,
                            node2,
                            json.dumps({"user_id": user2}, separators=(",", ":")),
                            future_time,
                            future_time,
                            future_time,
                        ),
                    )
                usage = expect(f"/api/v1/users/{user2}/usage")
                pending = next(
                    (item for item in usage["pending_revocations"] if item["job_id"] == pending_job_id),
                    None,
                )
                if pending is None or pending["node_id"] != node2 or pending["status"] != "queued":
                    raise RuntimeError("user usage did not report its queued node revocation")
                node_two_usage = next(
                    (item for item in usage["by_node"] if item["node_id"] == node2),
                    None,
                )
                if node_two_usage is None or node_two_usage["assigned"] is not True or node_two_usage["sampled_at"] is not None:
                    raise RuntimeError("usage did not show an assigned node with no sampling record")

                sampled_at = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
                with sqlite3.connect(temp / "service.db", timeout=10) as database:
                    database.execute(
                        "INSERT INTO traffic_records (id, node_id, user_id, instance_id, baseline_tx, baseline_rx, delta_tx, delta_rx, gap_reason, sampled_at) "
                        "VALUES (?, ?, ?, 'usage-fixture', 10, 20, 10, 20, NULL, ?)",
                        (str(uuid.uuid4()), node1, shared, sampled_at),
                    )
                    database.execute(
                        "INSERT INTO traffic_baselines (node_id, user_id, instance_id, tx_total, rx_total, sampled_at) "
                        "VALUES (?, ?, 'zero-traffic-fixture', 0, 0, ?)",
                        (node1, user1, sampled_at),
                    )
                    database.execute(
                        "UPDATE users SET usage_bytes = usage_bytes + 30 WHERE id = ?",
                        (shared,),
                    )
                zero_usage = expect(f"/api/v1/users/{user1}/usage")
                zero_node = next(
                    (item for item in zero_usage["by_node"] if item["node_id"] == node1),
                    None,
                )
                if (
                    zero_usage["data_freshness"]["status"] != "fresh"
                    or zero_node is None
                    or zero_node["sampled_at"] != sampled_at
                    or zero_node["tx_bytes"] != 0
                    or zero_node["rx_bytes"] != 0
                ):
                    raise RuntimeError("a traffic baseline with zero usage was not shown as a fresh sample")
                shared_usage = expect(f"/api/v1/users/{shared}/usage")
                node_samples = {item["node_id"]: item for item in shared_usage["by_node"]}
                if (
                    shared_usage["data_freshness"]["status"] != "stale"
                    or node_samples.get(node1, {}).get("sampled_at") is None
                    or node_samples.get(node2, {}).get("sampled_at") is not None
                    or node_samples.get(node1, {}).get("assigned") is not True
                    or node_samples.get(node2, {}).get("assigned") is not True
                ):
                    raise RuntimeError("a fresh node sample hid an assigned node with stale or missing data")

                checks = [
                    (auth(node1, token1, credential1), True, user1),
                    (auth(node2, token2, credential1), False, None),
                    (auth(node1, token1, credential2), False, None),
                    (auth(node2, token2, credential2), True, user2),
                    (auth(node1, token1, shared1), True, shared),
                    (auth(node2, token2, shared2), True, shared),
                ]
                for result, expected_ok, expected_id in checks:
                    if result.get("ok") is not expected_ok:
                        raise RuntimeError("node assignment authentication result was incorrect")
                    if expected_ok and result.get("id") != expected_id:
                        raise RuntimeError("authenticated identity changed across nodes")

                expect(
                    f"/api/v1/users/{user1}",
                    "PATCH",
                    {"expected_revision": 2, "name": "Renamed node one user"},
                )
                if auth(node1, token1, credential1).get("id") != user1:
                    raise RuntimeError("renaming a user changed its Hysteria statistics identity")

                def update_as_concurrent_client(name):
                    return request(
                        base,
                        f"/api/v1/users/{user1}",
                        admin,
                        "PATCH",
                        {"expected_revision": 3, "name": name},
                    )

                with ThreadPoolExecutor(max_workers=2) as clients:
                    concurrent_updates = list(
                        clients.map(update_as_concurrent_client, ("Mac A edit", "Mac B edit"))
                    )
                concurrent_statuses = sorted(status for status, _ in concurrent_updates)
                if concurrent_statuses != [200, 409]:
                    raise RuntimeError(
                        f"simultaneous edits at one user revision returned {concurrent_statuses}, expected one 200 and one 409"
                    )
                current_user = expect(f"/api/v1/users/{user1}")
                if current_user["revision"] != 4 or current_user["name"] not in ("Mac A edit", "Mac B edit"):
                    raise RuntimeError("concurrent edits did not preserve exactly one current revision")

                stale_revision_status, _ = request(
                    base,
                    f"/api/v1/users/{user1}",
                    admin,
                    "PATCH",
                    {"expected_revision": 2, "name": "Stale client update"},
                )
                if stale_revision_status != 409:
                    raise RuntimeError("stale user revision was not rejected with HTTP 409")

                rotation = expect(
                    f"/api/v1/users/{user1}/credentials/rotate",
                    "POST",
                    {"expected_revision": 4},
                )
                new_credential = rotation["credentials"][0]["credential"]
                if auth(node1, token1, credential1).get("ok") is not False:
                    raise RuntimeError("rotated Hysteria credential remained valid")
                if auth(node1, token1, new_credential).get("id") != user1:
                    raise RuntimeError("replacement Hysteria credential was not accepted")

                user2_subscription = expect(
                    f"/api/v1/users/{user2}/subscription",
                    "POST",
                    {"expected_revision": 3},
                )
                future_expiry = "2099-01-01T00:00:00Z"
                past_expiry = "2000-01-01T00:00:00Z"
                cleared_quota = expect(
                    f"/api/v1/users/{user2}",
                    "PATCH",
                    {
                        "expected_revision": 4,
                        "quota_bytes": None,
                        "expires_at": future_expiry,
                    },
                )
                if cleared_quota["quota_bytes"] is not None:
                    raise RuntimeError("user edit did not clear the previous quota")
                expect(
                    f"/api/v1/users/{user2}",
                    "PATCH",
                    {"expected_revision": 5, "expires_at": past_expiry},
                )
                if auth(node2, token2, credential2).get("ok") is not False:
                    raise RuntimeError("expired user was accepted by the Hysteria callback")
                expired_status, _ = request(
                    base,
                    f"/sub/{user2_subscription['token']}/clash.yaml",
                    method="GET",
                )
                if expired_status != 403:
                    raise RuntimeError("expired user subscription did not return HTTP 403")

                expect(
                    f"/api/v1/users/{user2}",
                    "PATCH",
                    {
                        "expected_revision": 6,
                        "expires_at": future_expiry,
                        "quota_bytes": 0,
                    },
                )
                if auth(node2, token2, credential2).get("ok") is not False:
                    raise RuntimeError("over-quota user was accepted by the Hysteria callback")
                over_quota_status, _ = request(
                    base,
                    f"/sub/{user2_subscription['token']}/clash.yaml",
                    method="GET",
                )
                if over_quota_status != 403:
                    raise RuntimeError("over-quota user subscription did not return HTTP 403")

                print(
                    "Admin token create/use/revoke, two-node auth isolation, server TLS resource parsing/key matching, mTLS certificate requirements/updates, stable identity, simultaneous edit conflict, credential rotation, "
                    "per-node usage freshness, pending revocation, quota clearing, expiry denial, and over-quota subscription denial passed."
                )
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
