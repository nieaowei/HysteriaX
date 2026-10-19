#!/usr/bin/env python3
"""Upgrade a real legacy deployment, recover offline sessions, restore matching v31 backup.

Everything runs in disposable containers. Pass the locally built candidate as
HYSTERIAX_UPGRADE_IMAGE; the origin image deliberately exposes the old protocol.
No compatibility adapter is used against the new API.
"""
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import uuid

import yaml

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("mtls_live", ROOT / "scripts/verify-mtls-live.py")
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)
OLD_IMAGE = "nieaowei/hysteriax-server:node-create-fix-202610160914"


def main():
    image = os.environ["HYSTERIAX_UPGRADE_IMAGE"]
    suffix = uuid.uuid4().hex[:10]
    network = "hx-upgrade-" + suffix
    db, node, api = (network + "-" + name for name in ("db", "node", "api"))
    node_image = network + ":node"
    port, udp, socks = live.free_port(), live.free_port(socket.SOCK_DGRAM), live.free_port()
    token, password = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
    containers = []
    client = None
    with tempfile.TemporaryDirectory(prefix="hx-upgrade-live-") as folder:
        temp = Path(folder)
        temp.chmod(0o700)
        env_file = temp / "api.env"
        env_file.write_text("\n".join([
            "DATABASE_URL=postgres://postgres:" + password + "@db:5432/hysteriax",
            "HYSTERIAX_LISTEN_ADDR=0.0.0.0:8080", "HYSTERIAX_PUBLIC_URL=http://api:8080",
            "HYSTERIAX_ADMIN_TOKEN=" + token,
            "HYSTERIAX_MASTER_KEY=" + base64.b64encode(os.urandom(32)).decode().rstrip("="), "RUST_LOG=warn",
        ]) + "\n")
        env_file.chmod(0o600)
        db_env = temp / "db.env"
        db_env.write_text("POSTGRES_DB=hysteriax\nPOSTGRES_PASSWORD=" + password + "\n")
        db_env.chmod(0o600)
        base = "http://127.0.0.1:" + str(port)

        def expect(path, method="GET", payload=None, statuses=(200, 201, 202)):
            status, body = live.raw_request(base, path, token, method, payload)
            assert status in statuses, f"{method} {path}: unexpected HTTP {status}"
            return json.loads(body) if body else None

        def sql(query):
            result = live.run(["docker", "exec", db, "psql", "-U", "postgres", "-d", "hysteriax", "-At", "-v", "ON_ERROR_STOP=1", "-c", query])
            return result.stdout.strip()

        def start_api(tag):
            live.run(["docker", "run", "-d", "--name", api, "--network", network,
                      "--network-alias", "api", "--env-file", str(env_file),
                      "-p", f"127.0.0.1:{port}:8080", tag])
            if api not in containers:
                containers.append(api)
            for _ in range(120):
                try:
                    if live.raw_request(base, "/readyz")[0] == 200:
                        return
                except OSError:
                    pass
                time.sleep(0.5)
            logs = live.run(["docker", "logs", api], check=False)
            message = logs.stdout + logs.stderr
            for value in (password, token, env_file.read_text().split("HYSTERIAX_MASTER_KEY=", 1)[1].splitlines()[0]):
                message = message.replace(value, "[redacted]")
            raise RuntimeError("upgrade API did not become ready: " + message[-2500:])

        def stop_api():
            live.run(["docker", "rm", "-f", api])

        def wait_job(job, timeout=180):
            until = time.monotonic() + timeout
            while time.monotonic() < until:
                result = expect("/api/v1/jobs/" + job)
                status = result["job"]["status"]
                if status == "succeeded":
                    return result
                assert status not in ("failed", "cancelled", "rolled_back"), "deployment failed: " + str(result["job"].get("error_message", ""))
                time.sleep(0.5)
            raise TimeoutError("upgrade deployment timed out")

        def traffic(ok=True):
            result = live.run(["curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                               "--max-time", "5", "--socks5-hostname", f"127.0.0.1:{socks}",
                               "http://127.0.0.1:18081/payload"], check=False)
            success = result.returncode == 0 and result.stdout == "upgrade-traffic-proof"
            assert success == ok, "unexpected legacy/managed forwarding state"

        def start_client(cert, key, credential):
            nonlocal client
            if client is not None:
                client.terminate()
                client.wait(timeout=5)
            for name, value in (("client.pem", cert), ("client.key", key)):
                (temp / name).write_text(value)
                (temp / name).chmod(0o600)
            config = {"server": f"127.0.0.1:{udp}", "auth": credential,
                      "tls": {"insecure": True, "sni": "credentials.example.test", "clientCertificate": str(temp / "client.pem"), "clientKey": str(temp / "client.key")},
                      "socks5": {"listen": f"127.0.0.1:{socks}"}}
            (temp / "client.yaml").write_text(yaml.safe_dump(config))
            (temp / "client.yaml").chmod(0o600)
            with (temp / "client.log").open("ab") as log:
                client = subprocess.Popen([str(temp / "hysteria"), "--disable-update-check", "client", "-c", str(temp / "client.yaml")], stdout=log, stderr=log)
            time.sleep(1)

        try:
            live.run(["docker", "network", "create", network])
            live.run(["docker", "run", "-d", "--name", db, "--network", network,
                      "--network-alias", "db", "--env-file", str(db_env), "postgres:18-bookworm"])
            containers.append(db)
            for _ in range(60):
                if live.run(["docker", "exec", db, "pg_isready", "-U", "postgres"], check=False).returncode == 0:
                    break
                time.sleep(0.5)
            live.run(["docker", "build", "--platform", "linux/arm64", "--build-arg", "BASE_IMAGE=debian:trixie", "-f", str(live.DOCKERFILE), "-t", node_image, str(ROOT)], timeout=900)
            live.run(["docker", "run", "-d", "--privileged", "--cgroupns=host", "--tmpfs", "/run", "--tmpfs", "/run/lock", "--name", node, "--network", network, "--network-alias", "node", "-p", f"127.0.0.1:{udp}:{udp}/udp", node_image])
            containers.append(node)
            live.wait_for_systemd(node)
            ssh_key = temp / "id_ed25519"
            live.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(ssh_key)])
            live.run(["docker", "cp", str(ssh_key.with_suffix(".pub")), node + ":/root/.ssh/authorized_keys"])
            live.run(["docker", "exec", node, "sh", "-ec", "chmod 0700 /root/.ssh; chmod 0600 /root/.ssh/authorized_keys; chown -R root:root /root/.ssh; mkdir -p /run/sshd; chmod 0755 /run/sshd; ssh-keygen -A; systemctl start ssh.service; mkdir -p /tmp/upgrade-http; printf upgrade-traffic-proof > /tmp/upgrade-http/payload"])
            live.run(["docker", "exec", "-d", node, "python3", "-m", "http.server", "18081", "--bind", "127.0.0.1", "--directory", "/tmp/upgrade-http"])
            fingerprint = live.run(["docker", "exec", node, "ssh-keygen", "-lf", "/etc/ssh/ssh_host_ed25519_key.pub"]).stdout.split()[1]
            live.run(["curl", "-fLsS", "--retry", "3", live.HYSTERIA_URL, "-o", str(temp / "hysteria")], timeout=180)
            assert hashlib.sha256((temp / "hysteria").read_bytes()).hexdigest() == live.HYSTERIA_SHA256
            (temp / "hysteria").chmod(0o700)
            live.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", str(temp / "ca.key"), "-out", str(temp / "ca.pem"), "-days", "365", "-subj", "/CN=Legacy upgrade CA", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign"])
            live.run(["openssl", "req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", str(temp / "old.key"), "-out", str(temp / "old.csr"), "-subj", "/CN=Legacy user"])
            (temp / "client.ext").write_text("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n")
            live.run(["openssl", "x509", "-req", "-in", str(temp / "old.csr"), "-CA", str(temp / "ca.pem"), "-CAkey", str(temp / "ca.key"), "-CAcreateserial", "-out", str(temp / "old.pem"), "-days", "30", "-extfile", str(temp / "client.ext")])
            old_cert, old_key = (temp / "old.pem").read_text(), (temp / "old.key").read_text()
            start_api(OLD_IMAGE)

            def credential(name, kind, material):
                return expect("/api/v1/credentials", "POST", {"name": name, "kind": kind, "payload": material})["id"]

            ssh = credential("Upgrade SSH", "ssh_private_key", {"secret": ssh_key.read_text()})
            tls = credential("Upgrade TLS", "tls_identity", {"certificate": (ROOT / "tests/fixtures/credentials-test.crt").read_text(), "private_key": (ROOT / "tests/fixtures/credentials-test.key").read_text()})
            ca = credential("Legacy public CA", "ca_certificate", {"content": (temp / "ca.pem").read_text()})
            user = expect("/api/v1/users", "POST", {"name": "Upgrade user"})["id"]
            client_credential = expect("/api/v1/credentials", "POST", {"name": "Old user certificate", "kind": "tls_identity", "owner_user_id": user, "payload": {"certificate": old_cert, "private_key": old_key}})["id"]
            options = {"tls": {"cert": f"credential://{tls}/1/certificate", "key": f"credential://{tls}/1/private_key", "clientCA": f"credential://{ca}/1/content"}}
            created = expect("/api/v1/nodes", "POST", {"name": "Upgrade node", "ssh_host": "node", "ssh_port": 22, "ssh_username": "root", "ssh_credential_id": ssh, "ssh_credential_version": 1, "ssh_host_fingerprint": fingerprint, "public_host": "node", "public_port": udp, "listen_addr": f":{udp}", "tls_skip_verify": True, "tls_sni": "credentials.example.test", "config": options})
            node_id = created["node"]["id"]
            draft = {"action": "create", "name": "Upgrade group", "user_ids": [user], "node_ids": [node_id], "mtls_bindings": [{"user_id": user, "node_id": node_id, "credential_id": client_credential, "credential_version": 1}]}
            preview = expect("/api/v1/authorization-groups/preview", "POST", draft)
            draft.pop("action")
            draft["preview_token"] = preview["preview_token"]
            granted = expect("/api/v1/authorization-groups", "POST", draft)
            password_before = granted["created_credentials"][0]["hy2_credential"]
            revision = expect("/api/v1/users/" + user)["revision"]
            subscription = expect(f"/api/v1/users/{user}/subscription", "POST", {"expected_revision": revision})["token"]
            wait_job(expect(f"/api/v1/nodes/{node_id}/deploy", "POST", {"expected_revision": 1})["job_id"])
            start_client(old_cert, old_key, password_before)
            traffic()
            before = sql("SELECT json_build_object('users',(SELECT count(*) FROM users),'groups',(SELECT count(*) FROM authorization_groups),'assignments',(SELECT count(*) FROM node_assignments),'password',(SELECT credential_hash FROM node_assignments LIMIT 1),'revision',(SELECT revision FROM users LIMIT 1))")
            stop_api()
            dump = subprocess.check_output(["docker", "exec", db, "pg_dump", "-U", "postgres", "-d", "hysteriax", "-Fc", "--no-owner", "--no-acl"])
            assert dump.startswith(b"PGDMP")
            live.run(["docker", "exec", node, "tar", "-C", "/", "-cpf", "/tmp/upgrade-remote.tar", "opt/hysteriax", "etc/hysteriax", "etc/systemd/system/hysteriax.service"])
            sql("ALTER TABLE credential_versions DISABLE TRIGGER USER; UPDATE credential_versions SET payload_enc='broken-discarded-client' WHERE credential_id='" + client_credential + "'; ALTER TABLE credential_versions ENABLE TRIGGER USER")
            live.run(["docker", "exec", node, "systemctl", "stop", "ssh.service"])
            start_api(image)
            assert expect(f"/api/v1/nodes/{node_id}")["mtls_state"] == "needs_configuration"
            assert sql("SELECT json_build_object('users',(SELECT count(*) FROM users),'groups',(SELECT count(*) FROM authorization_groups),'assignments',(SELECT count(*) FROM node_assignments),'password',(SELECT credential_hash FROM node_assignments LIMIT 1),'revision',(SELECT revision FROM users LIMIT 1))") == before
            assert live.raw_request(base, f"/api/v1/users/{user}/assignments", token)[0] == 404
            assert not yaml.safe_load(live.raw_request(base, f"/sub/{subscription}?format=mihomo&platform=linux")[1]).get("proxies")
            traffic()
            assert sql("SELECT count(*) FROM kick_requests WHERE state<>'completed'") != "0"
            print("Actual v31 deployment upgraded with corrupt discarded material; business state preserved and offline legacy session pending", flush=True)
            live.run(["docker", "exec", node, "systemctl", "start", "ssh.service"])
            wait_job(expect(f"/api/v1/nodes/{node_id}/ssh-test", "POST", {"expected_revision": 1})["job_id"])
            for _ in range(180):
                live.run(["curl", "--noproxy", "", "--silent", "--max-time", "1", "--socks5-hostname", f"127.0.0.1:{socks}", "http://127.0.0.1:18081/payload"], check=False)
                if sql("SELECT count(*) FROM kick_requests WHERE state NOT IN ('completed','cancelled')") == "0":
                    break
                time.sleep(0.5)
            assert sql("SELECT count(*) FROM kick_requests WHERE state NOT IN ('completed','cancelled')") == "0", sql("SELECT json_agg(x) FROM (SELECT kind,status,stage FROM jobs ORDER BY created_at DESC LIMIT 8) x")
            traffic(False)
            managed = expect("/api/v1/credentials/ca-identities", "POST", {"name": "Recovery CA", "source": "generated", "idempotency_key": suffix})["id"]
            options["tls"].pop("clientCA")
            options["mtls"] = {"mode": "managed", "ca_credential_id": managed, "ca_credential_version": 1}
            expect(f"/api/v1/nodes/{node_id}", "PATCH", {"expected_revision": 1, "config": options})
            for _ in range(240):
                if expect(f"/api/v1/nodes/{node_id}")["mtls_state"] == "ready":
                    break
                time.sleep(0.5)
            assert expect(f"/api/v1/nodes/{node_id}")["mtls_state"] == "ready"
            exported = yaml.safe_load(live.raw_request(base, f"/sub/{subscription}?format=mihomo&platform=linux")[1])["proxies"][0]
            assert exported["certificate"] != old_cert and exported["private-key"] != old_key
            start_client(exported["certificate"], exported["private-key"], password_before)
            traffic()
            print("SSH recovery disconnected old session; explicit managed save deployed with independent probe and original subscription/password", flush=True)
            stop_api()
            client.terminate()
            client.wait(timeout=5)
            client = None
            live.run(["docker", "exec", node, "systemctl", "stop", "hysteriax.service"])
            live.run(["docker", "exec", node, "tar", "-C", "/", "-xpf", "/tmp/upgrade-remote.tar"])
            live.run(["docker", "exec", node, "systemctl", "daemon-reload"])
            live.run(["docker", "exec", db, "dropdb", "-U", "postgres", "--force", "hysteriax"])
            live.run(["docker", "exec", db, "createdb", "-U", "postgres", "hysteriax"])
            restored = subprocess.run(["docker", "exec", "-i", db, "pg_restore", "-U", "postgres", "-d", "hysteriax", "--exit-on-error", "--no-owner", "--no-acl"], input=dump, capture_output=True)
            assert restored.returncode == 0, "matching pre-upgrade dump restore failed"
            start_api(OLD_IMAGE)
            live.run(["docker", "exec", node, "systemctl", "restart", "hysteriax.service"])
            start_client(old_cert, old_key, password_before)
            traffic()
            assert yaml.safe_load(live.raw_request(base, f"/sub/{subscription}?format=mihomo&platform=linux")[1])["proxies"][0]["certificate"] == old_cert
            print("Matching old image + database + remote snapshot rollback restored legacy subscription and real traffic", flush=True)
        finally:
            if client is not None:
                client.terminate()
                client.wait(timeout=5)
            for name in reversed(containers):
                live.run(["docker", "rm", "-f", "-v", name], check=False)
            live.run(["docker", "network", "rm", network], check=False)
            live.run(["docker", "image", "rm", node_image], check=False)


if __name__ == "__main__":
    main()
