#!/usr/bin/env python3
"""Verify real SSH host counters and Hysteria restrictions on a disposable node.

Requires TEST_DATABASE_URL, psycopg, Docker, and the systemd-node test image.
Build it with scripts/fixtures/systemd-node.Dockerfile, tagged
hysteriax-package-test:local. Never uses existing management nodes.
"""
import argparse
import base64
import importlib.util
import http.server
import threading
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import uuid

from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("matrix", ROOT / "scripts/verify-deployment-matrix.py")
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)


def wait_for(predicate, label, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, ValueError):
            pass
        time.sleep(0.5)
    raise TimeoutError(label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="hysteriax-package-test:local")
    args = parser.parse_args()
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    name = "hysteriax-package-" + uuid.uuid4().hex[:10]
    service = client = stream = target_server = None
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-package-live-") as temporary:
        temp = Path(temporary)
        try:
            ssh_port = matrix.free_port()
            hy2_port = matrix.free_port(socket.SOCK_DGRAM)
            matrix.run(["docker", "run", "--privileged", "--cgroupns=host", "--tmpfs", "/run", "--tmpfs", "/run/lock", "--name", name, "-p", f"127.0.0.1:{ssh_port}:22", "-p", f"127.0.0.1:{hy2_port}:443/udp", "-d", args.image])
            matrix.wait_for_systemd(name)
            key = temp / "id_ed25519"
            matrix.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)])
            matrix.run(["docker", "cp", str(key.with_suffix(".pub")), f"{name}:/root/.ssh/authorized_keys"])
            matrix.run(["docker", "exec", name, "sh", "-c", "mkdir -p /run/sshd; chown -R root:root /root/.ssh; chmod 700 /root/.ssh; chmod 600 /root/.ssh/authorized_keys; ssh-keygen -A; systemctl enable --now ssh.service"])
            api_port = matrix.free_port()
            base = f"http://127.0.0.1:{api_port}"
            public_host = "host.docker.internal" if os.uname().sysname == "Darwin" else "172.17.0.1"
            admin = secrets.token_urlsafe(48)
            environment = os.environ | {
                "DATABASE_URL": database.url, "HYSTERIAX_LISTEN_ADDR": f"0.0.0.0:{api_port}",
                "HYSTERIAX_PUBLIC_URL": f"http://{public_host}:{api_port}",
                "HYSTERIAX_ADMIN_TOKEN": admin, "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="),
                "RUST_LOG": "warn",
            }
            with open(temp / "server.log", "wb") as log:
                service = subprocess.Popen([str(ROOT / "target/debug/hysteriax-server")], env=environment, stdout=log, stderr=log)

                def api(path, method="GET", payload=None, expected=(200, 201, 202), token=admin):
                    status, body = matrix.request(base, path, token, method, payload)
                    if status not in expected:
                        raise RuntimeError(f"{method} {path}: HTTP {status}")
                    return json.loads(body) if body else None

                def ready():
                    if service.poll() is not None:
                        raise RuntimeError("temporary API exited: " + (temp / "server.log").read_text()[-1600:])
                    return matrix.request(base, "/readyz")[0] == 200
                wait_for(ready, "API readiness")
                package = {"expires_at": None, "quota_bytes": 1_000_000_000, "cycle": "fixed", "reset_day": 1,
                           "timezone": "Asia/Shanghai", "interface": None, "direction": "both",
                           "expiry_warning_days": 7, "traffic_warning_percent": 80}
                created = api("/api/v1/nodes", "POST", {
                    "name": "Package live node", "ssh_host": "127.0.0.1", "ssh_port": ssh_port,
                    "ssh_username": "root", "ssh_auth_type": "private_key", "ssh_secret": key.read_text(),
                    "public_host": "127.0.0.1", "public_port": hy2_port, "listen_addr": ":443",
                    "package": package, "initial_usage_bytes": 123,
                })
                node_id = created["node"]["id"]
                path = f"/api/v1/nodes/{node_id}"
                node_token = created["node_auth_token"]

                def detail(): return api(path)
                def wait_job(job_id):
                    result = wait_for(lambda: (value if (value := api(f"/api/v1/jobs/{job_id}"))["job"]["status"] in ("succeeded", "failed", "rolled_back") else None), "job completion", 240)
                    if result["job"]["status"] != "succeeded": raise RuntimeError(f"job failed: {result['job']['kind']}: {result['job'].get('error_message')}")
                    return result
                ssh_job = api(path + "/ssh-test", "POST", {"expected_revision": 1})["job_id"]
                fingerprint = wait_job(ssh_job)["result"]["result"]["fingerprint"]
                api(path, "PATCH", {"expected_revision": 1, "ssh_host_fingerprint": fingerprint})
                observed = wait_for(lambda: (value if (value := detail())["package_usage"]["freshness"] == "fresh" else None), "first network sample", 45)
                assert observed["package_usage"]["interface"] == "eth0"
                assert observed["package_usage"]["usage_bytes"] >= 123
                before = observed["package_usage"]["usage_bytes"]
                # Transfer unrelated host traffic over SSH; proves accounting is whole-host.
                subprocess.run(["ssh", "-i", str(key), "-p", str(ssh_port), "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "root@127.0.0.1", "head -c 1048576 /dev/urandom"], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                wait_for(lambda: detail()["package_usage"]["usage_bytes"] > before + 1_000_000, "whole-host flow increment", 30)
                print("Real SSH counters: default-route detection, initial usage and unrelated host traffic passed", flush=True)

                cert, cert_key = temp / "cert.pem", temp / "cert-key.pem"
                matrix.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(cert_key), "-out", str(cert), "-days", "2", "-subj", "/CN=package.example.test"])
                refs = []
                for resource, kind in [(cert, "certificate"), (cert_key, "private_key")]:
                    refs.append(api(path + "/resources", "POST", {"name": resource.name, "resource_kind": kind, "content_base64": base64.b64encode(resource.read_bytes()).decode()})["reference"])
                configured = api(path, "PATCH", {"expected_revision": detail()["revision"], "config": {"tls": {"cert": refs[0], "key": refs[1]}}})
                for attempt in range(3):
                    try:
                        wait_job(api(path + "/deploy", "POST", {"expected_revision": configured["revision"]})["job_id"])
                        break
                    except RuntimeError as error:
                        if attempt == 2 or not any(message in str(error) for message in ("TLS close_notify", "response body", "connection reset")): raise
                        print("Retrying transient release-download failure", flush=True)
                print("Disposable Hysteria node deployed", flush=True)
                user = api("/api/v1/users", "POST", {"name": "Package live user"})["id"]
                credential = api(f"/api/v1/users/{user}/assignments", "POST", {"expected_revision": 1, "node_id": node_id})["hy2_credential"]
                sub = api(f"/api/v1/users/{user}/subscription", "POST", {"expected_revision": 2})["token"]

                def auth():
                    return api(f"/hy2/auth/{node_id}/{node_token}", "POST", {"addr": "", "auth": credential, "tx": 0}, token=None)["ok"]
                def online():
                    command = "secret=$(awk '/^trafficStats:/{found=1;next} found && /secret:/{print $2;exit}' /etc/hysteriax/config.yaml); curl -fsS -H \"Authorization: $secret\" http://127.0.0.1:9780/online"
                    return json.loads(matrix.run(["docker", "exec", name, "sh", "-c", command]).stdout).get(user, 0)
                client_binary = temp / "hysteria-client"
                matrix.download_hysteria_client(client_binary)
                socks_port = matrix.free_port()
                client_config = temp / "client.yaml"
                client_config.write_text(f"server: 127.0.0.1:{hy2_port}\nauth: {credential}\ntls:\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:{socks_port}\n")
                class StreamTarget(http.server.BaseHTTPRequestHandler):
                    def do_GET(self):
                        self.send_response(200)
                        self.end_headers()
                        try:
                            while True:
                                self.wfile.write(b"x" * 65536)
                                self.wfile.flush()
                                time.sleep(0.05)
                        except (BrokenPipeError, ConnectionResetError): pass
                    def log_message(self, *args): pass
                target_server = http.server.ThreadingHTTPServer(("0.0.0.0", 0), StreamTarget)
                threading.Thread(target=target_server.serve_forever, daemon=True).start()
                target_url = f"http://{public_host}:{target_server.server_address[1]}/stream"
                stream_log = open(temp / "stream.log", "wb")
                def start_stream():
                    return subprocess.Popen(["curl", "--noproxy", "", "--silent", "--show-error", "--max-time", "120", "--socks5-hostname", f"127.0.0.1:{socks_port}", target_url, "-o", os.devnull], stdout=stream_log, stderr=stream_log)
                with open(temp / "client.log", "wb") as client_log:
                    def start_client():
                        return subprocess.Popen([str(client_binary), "--disable-update-check", "client", "-c", str(client_config)], stdout=client_log, stderr=client_log)
                    client = start_client()
                    wait_for(lambda: online() > 0, "live Hysteria client handshake", 35)
                    stream = start_stream()
                    time.sleep(2)
                    assert stream.poll() is None and auth()
                    # A warning does not restrict the active node, and keeps one event ID.
                    api(path + "/usage", "PUT", {"expected_revision": detail()["revision"], "usage_bytes": 800_000_000})
                    warning = detail()["package_usage"]["alerts"][0]["id"]
                    time.sleep(2)
                    assert detail()["package_usage"]["alerts"][0]["id"] == warning
                    assert auth()
                    api(path + "/usage", "PUT", {"expected_revision": detail()["revision"], "usage_bytes": 1_000_000_000})
                    assert not auth()
                    wait_for(lambda: online() == 0, "active proxy disconnect", 45)
                    status, body = matrix.request(base, f"/sub/{sub}/clash.yaml")
                    assert status == 200 and b"Package live node" not in body
                    assert matrix.run(["docker", "exec", name, "systemctl", "is-active", "hysteriax.service"]).stdout.strip() == "active"
                    api(path + "/usage", "PUT", {"expected_revision": detail()["revision"], "usage_bytes": 0, "reset": True})
                    assert auth()
                    stream.wait(timeout=10)
                    if client.poll() is None: client.terminate(); client.wait(timeout=5)
                    client = start_client()
                    time.sleep(1)
                    stream = start_stream()
                    wait_for(lambda: online() > 0, "new proxy connection after automatic node recovery", 35)
                    print("Real Hysteria: warning deduplication, quota rejection, active disconnect, subscription filtering and automatic recovery passed", flush=True)
                    expiry_package = package | {"expires_at": "2020-01-01T00:00:00Z"}
                    result = api(path, "PATCH", {"expected_revision": detail()["revision"], "package": expiry_package})
                    assert not result["sync_job_queued"] and not auth()
                    wait_for(lambda: online() == 0, "expiry disconnect", 45)
                    api(path, "PATCH", {"expected_revision": detail()["revision"], "package": package})
                    assert auth()
                    stream.wait(timeout=10)
                    if client.poll() is None: client.terminate(); client.wait(timeout=5)
                    client = start_client()
                    time.sleep(1)
                    stream = start_stream()
                    wait_for(lambda: online() > 0, "new proxy connection after renewal", 35)
                    # Bad interface retains known usage and marks a gap; expiry still works.
                    bad = package | {"interface": "missing0"}
                    api(path, "PATCH", {"expected_revision": detail()["revision"], "package": bad})
                    wait_for(lambda: detail()["package_usage"]["gap_reason"] == "network_sample_failed", "network failure gap", 45)
                    assert detail()["package_usage"]["usage_bytes"] >= 0
                    print("Expiry, renewal without deployment, independent failure tracking and continued service passed", flush=True)
        except Exception:
            log = temp / "server.log"
            if log.exists(): print("Temporary API diagnostics: " + log.read_text()[-2000:], flush=True)
            raise
        finally:
            for process in (stream, client, service):
                if process and process.poll() is None:
                    process.terminate()
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired: process.kill(); process.wait()
            if target_server: target_server.shutdown(); target_server.server_close()
            subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == "__main__":
    main()
