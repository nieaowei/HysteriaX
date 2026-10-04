#!/usr/bin/env python3
"""Exercise public proxy monitoring against a disposable real SSH/Hysteria node.

Requires TEST_DATABASE_URL, scripts/requirements-test.txt, Docker and the
hysteriax-package-test:local systemd image. Existing nodes are never modified.
"""
import base64
import argparse
import http.server
import importlib.util
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import uuid

from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("matrix", ROOT / "scripts/verify-deployment-matrix.py")
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)


def wait_for(predicate, label, timeout=210):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
        except (OSError, ValueError):
            value = None
        if value:
            return value
        time.sleep(1)
    raise TimeoutError(label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--restart-only", action="store_true")
    parser.add_argument("--keep-on-failure", action="store_true", help="keep the disposable lab alive until interrupted for diagnosis")
    args = parser.parse_args()
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    container = "hysteriax-overview-" + uuid.uuid4().hex[:10]
    service = client = target = None
    class Target(http.server.BaseHTTPRequestHandler):
        healthy = True
        def do_GET(self):
            self.send_response(200 if self.healthy else 503)
            self.end_headers()
            self.wfile.write(b"overview-probe-target")
        def log_message(self, *_):
            pass
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-overview-live-") as folder:
        temp = Path(folder)
        try:
            ssh_port, udp_port, api_port = matrix.free_port(), matrix.free_port(socket.SOCK_DGRAM), matrix.free_port()
            matrix.run(["docker", "run", "--privileged", "--cgroupns=host", "--tmpfs", "/run", "--tmpfs", "/run/lock", "--name", container, "-p", f"127.0.0.1:{ssh_port}:22", "-p", f"127.0.0.1:{udp_port}:443/udp", "-d", "hysteriax-package-test:local"])
            matrix.wait_for_systemd(container)
            key = temp / "id_ed25519"
            matrix.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)])
            matrix.run(["docker", "cp", str(key.with_suffix(".pub")), f"{container}:/root/.ssh/authorized_keys"])
            matrix.run(["docker", "exec", container, "sh", "-c", "mkdir -p /run/sshd; chown -R root:root /root/.ssh; chmod 700 /root/.ssh; chmod 600 /root/.ssh/authorized_keys; ssh-keygen -A; systemctl enable --now ssh.service"])
            binary = temp / "hysteria"
            matrix.download_hysteria_client(binary)
            target = http.server.ThreadingHTTPServer(("0.0.0.0", 0), Target)
            threading.Thread(target=target.serve_forever, daemon=True).start()
            callback = "host.docker.internal" if os.uname().sysname == "Darwin" else "172.17.0.1"
            target_url = f"http://{callback}:{target.server_port}/health"
            admin = secrets.token_urlsafe(48)
            base = f"http://127.0.0.1:{api_port}"
            environment = os.environ | {"DATABASE_URL": database.url, "HYSTERIAX_LISTEN_ADDR": f"0.0.0.0:{api_port}", "HYSTERIAX_PUBLIC_URL": f"http://{callback}:{api_port}", "HYSTERIAX_ADMIN_TOKEN": admin, "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="), "HYSTERIAX_PROBE_BINARY": str(binary), "RUST_LOG": "warn"}
            with open(temp / "server.log", "wb") as log:
                service = subprocess.Popen([str(ROOT / "target/debug/hysteriax-server")], env=environment, stdout=log, stderr=log)
                def api(path, method="GET", payload=None, expected=(200, 201, 202)):
                    status, body = matrix.request(base, path, admin, method, payload)
                    if status not in expected:
                        raise RuntimeError(f"{method} {path}: HTTP {status}: {body[:200]!r}")
                    return json.loads(body) if body else None
                wait_for(lambda: matrix.request(base, "/readyz")[0] == 200, "service readiness", 30)
                assert matrix.request(base, "/api/v1/overview")[0] == 401
                created = api("/api/v1/nodes", "POST", {"name": "Overview live node", "ssh_host": "127.0.0.1", "ssh_port": ssh_port, "ssh_username": "root", "ssh_auth_type": "private_key", "ssh_secret": key.read_text(), "public_host": "127.0.0.1", "public_port": udp_port, "listen_addr": ":443", "tls_skip_verify": True, "proxy_probe_url": "" if args.restart_only else target_url})
                node = created["node"]["id"]
                path = f"/api/v1/nodes/{node}"
                def detail(): return api(path)
                def job(job_id):
                    result = wait_for(lambda: (v if (v := api(f"/api/v1/jobs/{job_id}"))["job"]["status"] in ("succeeded", "failed", "rolled_back") else None), "job completion", 300)
                    if result["job"]["status"] != "succeeded": raise RuntimeError(result["job"].get("error_message"))
                    return result
                fingerprint = job(api(path + "/ssh-test", "POST", {"expected_revision": 1})["job_id"])["result"]["result"]["fingerprint"]
                api(path, "PATCH", {"expected_revision": 1, "ssh_host_fingerprint": fingerprint})
                cert, cert_key = temp / "cert.pem", temp / "key.pem"
                matrix.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(cert_key), "-out", str(cert), "-days", "2", "-subj", "/CN=overview.test"])
                refs = [api(path + "/resources", "POST", {"name": resource.name, "resource_kind": kind, "content_base64": base64.b64encode(resource.read_bytes()).decode()})["reference"] for resource, kind in [(cert, "certificate"), (cert_key, "private_key")]]
                api(path, "PATCH", {"expected_revision": detail()["revision"], "config": {"tls": {"cert": refs[0], "key": refs[1]}}})
                configured = wait_for(lambda: (value if (value := detail())["deployed_revision"] == value["revision"] and value["state"] == "deployed" else None), "automatic configuration deployment", 300)
                print("Disposable real Hysteria node deployed", flush=True)
                def monitor(): return api("/api/v1/overview")["nodes"][0]
                healthy = wait_for(lambda: (v if (v := monitor())["probe_status"] == "healthy" else None), "two successful public probes", 160)
                assert healthy["inlet_status"] == "ok"
                assert healthy["external_status"] == (None if args.restart_only else "ok")
                print("Initial public proxy probes healthy", flush=True)
                assert healthy["latency_ms"] is not None
                user = api("/api/v1/users", "POST", {"name": "Overview real user", "enabled": True})["id"]
                assigned = api(f"/api/v1/users/{user}/assignments", "POST", {"expected_revision": 1, "node_id": node})
                client_port = matrix.free_port()
                client_config = temp / "user.yaml"
                client_config.write_text(f"server: 127.0.0.1:{udp_port}\nauth: {assigned['hy2_credential']}\ntls:\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:{client_port}\n")
                with open(temp / "client.log", "wb") as client_log:
                    client = subprocess.Popen([str(binary), "--disable-update-check", "client", "-c", str(client_config)], stdout=client_log, stderr=client_log)
                    wait_for(lambda: api("/api/v1/overview")["online_users"] == 1, "real user online count", 45)
                    matrix.run(["curl", "--noproxy", "", "--fail", "--silent", "--socks5-hostname", f"127.0.0.1:{client_port}", target_url])
                    assert api("/api/v1/overview")["connections"] == 1
                    wait_for(lambda: api(f"/api/v1/users/{user}")["usage_bytes"] > 0, "real user traffic accounting", 45)
                    if not args.restart_only:
                        Target.healthy = False
                        failed = wait_for(lambda: (v if (v := monitor())["probe_status"] == "failed" else None), "three failed external probes", 210)
                        assert failed["inlet_status"] == "ok" and failed["external_status"] == "failed"
                        assert any(i["kind"] == "proxy_probe" for i in api("/api/v1/overview")["issues"])
                        Target.healthy = True
                        wait_for(lambda: monitor()["probe_status"] == "healthy", "two successful recovery probes", 150)
                        print("Public entrypoint, external target outage, hysteresis and recovery passed", flush=True)
                    for source in ["users", "network"]:
                        for span in ["24h", "7d", "30d"]:
                            history = api(f"/api/v1/overview/history?range={span}&timezone=Asia%2FShanghai&source={source}")
                            assert history["buckets"] and any(b["probe_attempts"] for b in history["buckets"])
                    with database.connect() as connection:
                        assert connection.execute("SELECT count(*) FROM traffic_records WHERE user_id LIKE 'monitor-%'").fetchone()[0] == 0
                        assert connection.execute("SELECT count(*) FROM monitoring_probe_tokens").fetchone()[0] == 0
                    print("Online identities, independent traffic sources, history ranges and probe credential cleanup passed", flush=True)
                    if not args.restart_only:
                        api(path, "PATCH", {"expected_revision": detail()["revision"], "proxy_probe_url": ""})
                        wait_for(lambda: (value if (value := detail())["deployed_revision"] == value["revision"] and value["state"] == "deployed" else None), "updated configuration deployment", 300)
                        no_target = wait_for(lambda: (v if (v := monitor())["probe_status"] == "healthy" and v["external_status"] is None else None), "default entrypoint-only probes", 160)
                        assert no_target["inlet_status"] == "ok"
                        print("Default entrypoint-only probe and deployed-revision transition passed", flush=True)
                    matrix.run(["docker", "exec", container, "systemctl", "stop", "hysteriax.service"])
                    unavailable = wait_for(lambda: (v if (v := monitor())["probe_status"] == "failed" else None), "three public entrypoint failures", 220)
                    assert unavailable["inlet_status"] == "failed"
                    print("Stopped service correctly classified as failed", flush=True)
                    # Recreate the published UDP forwarding path too. OrbStack can retain
                    # an unusable UDP publication when its listener has been absent for minutes.
                    matrix.run(["docker", "restart", container])
                    matrix.wait_for_systemd(container)
                    wait_for(lambda: monitor()["probe_status"] == "healthy", "entrypoint recovery", 160)
                    print("Actual proxy shutdown and two-sample recovery passed", flush=True)
        except Exception as failure:
            print("Verification failure: " + type(failure).__name__ + ": " + str(failure)[:240], flush=True)
            if "monitor" in locals():
                try: print("Last monitor state: " + json.dumps(monitor()), flush=True)
                except Exception: pass
            try:
                with database.connect() as connection:
                    if "node" in locals():
                        rows = connection.execute("SELECT revision,status,external_status,reason,sampled_at FROM proxy_probe_samples WHERE node_id=%s ORDER BY sampled_at DESC LIMIT 8", (node,)).fetchall()
                        print("Recent probe observations: " + repr(rows), flush=True)
                        rows = connection.execute("SELECT health,failures,successes,revision,sampled_at FROM monitoring_probe_state WHERE node_id=%s", (node,)).fetchall()
                        print("Persistent health state: " + repr(rows), flush=True)
                print("Node service state: " + matrix.run(["docker", "exec", container, "systemctl", "is-active", "hysteriax.service"], check=False).stdout.strip(), flush=True)
            except Exception: pass
            log = temp / "server.log"
            if log.exists(): print("Temporary API diagnostics: " + log.read_text()[-1600:], flush=True)
            if args.keep_on_failure:
                print("Keeping disposable lab for diagnosis; interrupt this verifier to clean up.", flush=True)
                while True: time.sleep(1)
            raise
        finally:
            for process in [client, service]:
                if process and process.poll() is None:
                    process.terminate()
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired: process.kill(); process.wait()
            if target: target.shutdown()
            matrix.run(["docker", "rm", "-f", container], check=False)


if __name__ == "__main__":
    main()
