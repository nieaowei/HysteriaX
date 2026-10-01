#!/usr/bin/env python3
"""Exercise the API subscription generator and parse its output with pinned Mihomo."""

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


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"


def request(base, path, token, method="GET", payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    headers = {"Authorization": f"Bearer {token}"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def main():
    subprocess.run(
        ["cargo", "build", "-p", "hysteriax-server"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.DEVNULL,
    )

    with tempfile.TemporaryDirectory(prefix="hysteriax-subscription-check-") as temporary:
        temp = pathlib.Path(temporary)
        with socket.socket() as socket_:
            socket_.bind(("127.0.0.1", 0))
            port = socket_.getsockname()[1]
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
        server = subprocess.Popen(
            [str(SERVER)], cwd=ROOT, env=environment,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            ready = False
            for _ in range(30):
                try:
                    status, _ = request(base, "/readyz", admin)
                    if status == 200:
                        ready = True
                        break
                except Exception:
                    pass
                time.sleep(1)
            if not ready:
                raise RuntimeError("temporary HysteriaX service did not become ready")

            client_certificate = temp / "client-certificate.pem"
            client_private_key = temp / "client-private-key.pem"
            subprocess.run(
                [
                    "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-keyout", str(client_private_key), "-out", str(client_certificate),
                    "-days", "2", "-subj", "/CN=Mihomo mTLS fixture",
                    "-addext", "extendedKeyUsage=clientAuth",
                ],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

            status, body = request(
                base,
                "/api/v1/nodes",
                admin,
                "POST",
                {
                    "name": "Mihomo parser fixture",
                    "ssh_host": "127.0.0.1",
                    "ssh_port": 22,
                    "ssh_username": "root",
                    "ssh_auth_type": "password",
                    "ssh_secret": "fixture-only",
                    "public_host": "example.com",
                    "public_port": 443,
                    "listen_addr": ":443,445-446",
                    "tls_sni": "example.com",
                    "config": {
                        "tls": {
                            "cert": "/etc/hysteriax/server.pem",
                            "key": "/etc/hysteriax/server-key.pem",
                            "clientCA": "/etc/hysteriax/client-ca.pem",
                        },
                        "obfs": {
                            "type": "gecko",
                            "gecko": {
                                "password": "fixture-obfs-secret",
                                "minPacketSize": 512,
                                "maxPacketSize": 1200,
                            },
                        },
                        "bandwidth": {"up": "100 Mbps", "down": "200 Mbps"},
                    },
                },
            )
            if status != 201:
                raise RuntimeError("could not create node fixture")
            node = json.loads(body)
            node_id = node["node"]["id"]

            ech_config = (
                "AEn+DQBFKwAgACABWIHUGj4u+PIggYXcR5JF0gYk3dCRioBW8uJq9H4mKAAIAAEAAQABAANAEnB1YmxpYy50bHMtZWNoLmRldgAA"
            )
            ech_pem = (
                "-----BEGIN ECH KEYS-----\nZmFrZS1rZXk=\n-----END ECH KEYS-----\n"
                f"-----BEGIN ECH CONFIGS-----\n{ech_config}\n-----END ECH CONFIGS-----\n"
            )
            status, body = request(
                base,
                f"/api/v1/nodes/{node_id}/resources",
                admin,
                "POST",
                {
                    "name": "ech.pem",
                    "resource_kind": "ech_key",
                    "content_base64": base64.b64encode(ech_pem.encode()).decode(),
                },
            )
            if status != 201:
                raise RuntimeError("could not upload the ECH fixture resource")
            ech_reference = json.loads(body)["reference"]
            status, body = request(base, f"/api/v1/nodes/{node_id}", admin)
            if status != 200:
                raise RuntimeError("could not read the node fixture before enabling ECH")
            node_config = json.loads(body)["config"]
            node_config["ech"] = {"keyPath": ech_reference}
            status, _ = request(
                base,
                f"/api/v1/nodes/{node_id}",
                admin,
                "PATCH",
                {"expected_revision": 1, "config": node_config},
            )
            if status != 200:
                raise RuntimeError("could not enable ECH on the parser fixture node")

            realm_token = "parser-realm-token_123"
            status, body = request(
                base,
                "/api/v1/nodes",
                admin,
                "POST",
                {
                    "name": "Mihomo Realm parser fixture",
                    "ssh_host": "127.0.0.1",
                    "ssh_port": 22,
                    "ssh_username": "root",
                    "ssh_auth_type": "password",
                    "ssh_secret": "fixture-only",
                    "public_host": "hy2.example.test",
                    "public_port": 443,
                    "listen_addr": ":443",
                    "tls_sni": "hy2.example.test",
                    "config": {
                        "tls": {
                            "cert": "/etc/hysteriax/server.pem",
                            "key": "/etc/hysteriax/server-key.pem",
                        },
                        "realm": {
                            "connection": {
                                "serverURL": "http://rendezvous.example.test:10820",
                                "token": realm_token,
                                "realmID": "parser-realm-123",
                            },
                            "stunServers": ["stun.example.test:3478"],
                        },
                    },
                },
            )
            if status != 400 or b"Mihomo v1.19.31" not in body:
                raise RuntimeError("unsupported Realm mode was not rejected after the live client timeout")

            status, body = request(
                base, "/api/v1/users", admin, "POST", {"name": "Mihomo parser user"}
            )
            if status != 201:
                raise RuntimeError("could not create user fixture")
            user_id = json.loads(body)["id"]
            status, _ = request(
                base,
                f"/api/v1/users/{user_id}/assignments",
                admin,
                "POST",
                {
                    "expected_revision": 1,
                    "node_id": node_id,
                    "client_certificate": client_certificate.read_text(),
                    "client_private_key": client_private_key.read_text(),
                },
            )
            if status != 201:
                raise RuntimeError("could not assign the parser fixture user")
            status, body = request(
                base,
                f"/api/v1/users/{user_id}/subscription",
                admin,
                "POST",
                {"expected_revision": 2},
            )
            if status != 200:
                raise RuntimeError("could not create parser fixture subscription")
            first_subscription = json.loads(body)["token"]
            status, body = request(
                base,
                f"/api/v1/users/{user_id}/subscription",
                admin,
                "POST",
                {"expected_revision": 3},
            )
            if status != 200:
                raise RuntimeError("could not rotate parser fixture subscription")
            subscription = json.loads(body)["token"]
            status, _ = request(base, f"/sub/{first_subscription}/clash.yaml", admin, method="GET")
            if status != 404:
                raise RuntimeError("rotated subscription token remained valid")
            status, body = request(
                base,
                f"/api/v1/users/{user_id}/subscription",
                admin,
                method="GET",
            )
            active = json.loads(body).get("active") if status == 200 else None
            if not active or active.get("token") != subscription:
                raise RuntimeError("active subscription lookup did not return the current token")

            status, body = request(
                base, "/api/v1/users", admin, "POST", {"name": "No assigned nodes"}
            )
            if status != 201:
                raise RuntimeError("could not create empty-subscription user fixture")
            empty_user_id = json.loads(body)["id"]
            status, body = request(
                base,
                f"/api/v1/users/{empty_user_id}/subscription",
                admin,
                "POST",
                {"expected_revision": 1},
            )
            if status != 200:
                raise RuntimeError("could not create empty-subscription token")
            empty_token = json.loads(body)["token"]

            # This test isolates the subscription renderer from SSH deployment.
            with sqlite3.connect(temp / "service.db") as database:
                database.execute(
                    "UPDATE nodes SET deployed_revision = desired_revision, "
                    "deployed_config_enc = desired_config_enc, state = 'deployed' WHERE id = ?",
                    (node_id,),
                )
                database.commit()

            status, config = request(
                base, f"/sub/{subscription}/clash.yaml", admin, method="GET"
            )
            if status != 200:
                raise RuntimeError("subscription endpoint did not return a configuration")
            if b"certificate:" not in config or b"private-key:" not in config:
                raise RuntimeError("mTLS certificate material is missing from the subscription")
            if b"ech-opts:" not in config or ech_config.encode() not in config:
                raise RuntimeError("ECH client config is missing from the subscription")
            if b"ports:" not in config or b"443,445-446" not in config:
                raise RuntimeError("port hopping range is missing from the subscription")
            if b"hop-interval: 30" not in config:
                raise RuntimeError("port hopping interval default is missing from the subscription")
            config_path = temp / "clash.yaml"
            config_path.write_bytes(config)
            subprocess.run(
                [str(ROOT / "scripts" / "verify-mihomo-config.sh"), str(config_path)],
                check=True,
                stdout=subprocess.DEVNULL,
            )
            status, empty_config = request(
                base, f"/sub/{empty_token}/clash.yaml", admin, method="GET"
            )
            if (
                status != 200
                or b"proxies: []" not in empty_config
                or b"DIRECT" not in empty_config
                or b"MATCH,DIRECT" not in empty_config
            ):
                raise RuntimeError("unassigned user did not receive a valid DIRECT-only subscription")
            empty_config_path = temp / "empty-subscription.yaml"
            empty_config_path.write_bytes(empty_config)
            subprocess.run(
                [str(ROOT / "scripts" / "verify-mihomo-config.sh"), str(empty_config_path)],
                check=True,
                stdout=subprocess.DEVNULL,
            )
            print("Generated mTLS, ECH, and port hopping subscription parses with pinned Mihomo v1.19.31; Realm mode is gated after failed live acceptance.")
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    main()
