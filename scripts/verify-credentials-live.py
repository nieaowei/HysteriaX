#!/usr/bin/env python3
"""Exercise shared credential application on two disposable real systemd nodes."""
import base64
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

from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("matrix", ROOT / "scripts/verify-deployment-matrix.py")
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)


def wait_for(check, description, timeout=240):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.5)
    raise TimeoutError(description)


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    suffix = uuid.uuid4().hex[:10]
    image = f"hysteriax-credentials-node:{suffix}"
    containers = []
    service = None
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-credentials-live-") as folder:
        temp = Path(folder)
        old_key, new_key = temp / "old-ssh", temp / "new-ssh"
        phrase = "fixture-" + secrets.token_hex(12)
        for path, password in [(old_key, ""), (new_key, phrase)]:
            matrix.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", password, "-f", str(path)])
        matrix.run(["docker", "build", "--build-arg", "BASE_IMAGE=debian:trixie", "-f", str(ROOT / "scripts/fixtures/systemd-node.Dockerfile"), "-t", image, str(ROOT)], timeout=900)
        try:
            endpoints = []
            for index in range(2):
                name = f"hysteriax-credentials-{suffix}-{index}"
                ssh_port = matrix.free_port()
                udp_port = matrix.free_port(socket.SOCK_DGRAM)
                matrix.run(["docker", "run", "--detach", "--privileged", "--cgroupns=host", "--tmpfs", "/run", "--tmpfs", "/run/lock", "--name", name, "-p", f"127.0.0.1:{ssh_port}:22", "-p", f"127.0.0.1:{udp_port}:443/udp", image])
                containers.append(name)
                matrix.wait_for_systemd(name)
                matrix.run(["docker", "cp", str(old_key.with_suffix(".pub")), f"{name}:/root/.ssh/authorized_keys"])
                matrix.run(["docker", "exec", name, "sh", "-c", "chown -R root:root /root/.ssh; chmod 0700 /root/.ssh; chmod 0600 /root/.ssh/authorized_keys; ssh-keygen -A; systemctl enable --now ssh.service"])
                endpoints.append((name, ssh_port, udp_port))

            port = matrix.free_port()
            base = f"http://127.0.0.1:{port}"
            admin = "hx_" + secrets.token_urlsafe(36)
            environment = os.environ | {"DATABASE_URL": database.url, "HYSTERIAX_LISTEN_ADDR": f"0.0.0.0:{port}", "HYSTERIAX_PUBLIC_URL": f"http://host.docker.internal:{port}", "HYSTERIAX_ADMIN_TOKEN": admin, "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="), "RUST_LOG": "warn"}
            with open(temp / "server.log", "wb") as log:
                service = subprocess.Popen([str(ROOT / "target/debug/hysteriax-server")], env=environment, stdout=log, stderr=log)

                def api(path, method="GET", payload=None):
                    status, body = matrix.raw_request(base, path, admin, method, payload)
                    if status not in (200, 201, 202, 204):
                        raise RuntimeError(f"{method} {path}: HTTP {status}")
                    return json.loads(body) if body else None

                def ready():
                    if service.poll() is not None:
                        raise RuntimeError("credential test API stopped")
                    try:
                        return matrix.raw_request(base, "/readyz")[0] == 200
                    except OSError:
                        return False
                wait_for(ready, "API readiness", 30)

                def create(name, kind, payload):
                    return api("/api/v1/credentials", "POST", {"name": name, "kind": kind, "payload": payload})["id"]

                def tls_payload(index):
                    certificate, key = temp / f"tls-{index}.crt", temp / f"tls-{index}.key"
                    matrix.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key), "-out", str(certificate), "-days", "365", "-subj", "/CN=credentials.example.test", "-addext", "subjectAltName=DNS:credentials.example.test"])
                    return {"certificate": certificate.read_text(), "private_key": key.read_text()}

                def publish(identity, payload):
                    current = api(f"/api/v1/credentials/{identity}")
                    return api(f"/api/v1/credentials/{identity}/versions", "POST", {"expected_revision": current["revision"], "payload": payload})

                def batch_finished(identity, allow_failure=False):
                    def finished():
                        value = api(f"/api/v1/credential-batches/{identity}")
                        return value if all(i["status"] in ("succeeded", "failed", "rolled_back", "cancelled") for i in value["items"]) else None
                    result = wait_for(finished, "credential batch completion")
                    if not allow_failure and any(i["status"] != "succeeded" for i in result["items"]):
                        raise RuntimeError("credential batch failed")
                    return result

                def job_done(identity):
                    def finished():
                        value = api(f"/api/v1/jobs/{identity}")
                        return value if value["job"]["status"] in ("succeeded", "failed", "rolled_back") else None
                    result = wait_for(finished, "node job completion")
                    if result["job"]["status"] != "succeeded":
                        raise RuntimeError("node verification job failed")
                    return result

                def detail(node):
                    return api(f"/api/v1/nodes/{node}")

                def deployed_version(node):
                    refs = api(f"/api/v1/credentials/{tls}/references")
                    return {r["version"] for r in refs if r["entity_id"] == node and r["source"] == "deployed"}

                ssh = create("Shared SSH", "ssh_private_key", {"secret": old_key.read_text()})
                first_tls = tls_payload(1)
                tls = create("Shared TLS", "tls_identity", first_tls)
                nodes = []
                for index, (_, ssh_port, udp_port) in enumerate(endpoints):
                    created = api("/api/v1/nodes", "POST", {"name": f"Credential node {index}", "ssh_host": "127.0.0.1", "ssh_port": ssh_port, "ssh_username": "root", "ssh_credential_id": ssh, "ssh_credential_version": 1, "public_host": "127.0.0.1", "public_port": udp_port, "listen_addr": ":443", "tls_sni": "credentials.example.test", "tls_skip_verify": True, "config": {"tls": {"cert": f"credential://{tls}/1/certificate", "key": f"credential://{tls}/1/private_key", "sniGuard": "disable"}}})
                    node = created["node"]["id"]
                    fingerprint = job_done(api(f"/api/v1/nodes/{node}/ssh-test", "POST", {"expected_revision": 1})["job_id"])["result"]["result"]["fingerprint"]
                    api(f"/api/v1/nodes/{node}", "PATCH", {"expected_revision": 1, "ssh_host_fingerprint": fingerprint})
                    job_done(api(f"/api/v1/nodes/{node}/deploy", "POST", {"expected_revision": detail(node)["revision"]})["job_id"])
                    nodes.append(node)
                print("Two real nodes deployed with one shared TLS identity and SSH credential.", flush=True)

                published = publish(tls, tls_payload(2))
                assert published["affected_count"] == 2
                batch_finished(published["batch_id"])
                assert all(deployed_version(n) == {2} for n in nodes)
                job_done(api(f"/api/v1/nodes/{nodes[0]}/rollback", "POST", {"expected_revision": detail(nodes[0])["revision"]})["job_id"])
                assert deployed_version(nodes[0]) == {1}, "rollback did not resolve the historical credential version"
                print("Automatic TLS version application and pinned-version rollback passed.", flush=True)

                matrix.run(["docker", "exec", endpoints[1][0], "systemctl", "stop", "ssh.service"])
                published = publish(tls, tls_payload(3))
                partial = batch_finished(published["batch_id"], allow_failure=True)
                assert sorted(i["status"] for i in partial["items"]) == ["failed", "succeeded"]
                assert deployed_version(nodes[0]) == {3} and deployed_version(nodes[1]) == {2}
                matrix.run(["docker", "exec", endpoints[1][0], "systemctl", "start", "ssh.service"])
                retried = api(f"/api/v1/credential-batches/{published['batch_id']}/retry", "POST")
                assert retried["affected_count"] == 1
                batch_finished(published["batch_id"])
                assert all(deployed_version(n) == {3} for n in nodes)
                print("Partial deployment failure preserved the previous effective version; retry changed only the failed node.", flush=True)

                publish(tls, tls_payload(4))
                newest = publish(tls, tls_payload(5))
                batch_finished(newest["batch_id"])
                assert all(deployed_version(n) == {5} for n in nodes)
                print("Back-to-back publication converged both nodes to the newest version.", flush=True)

                published = publish(ssh, {"secret": new_key.read_text(), "passphrase": phrase})
                failures = batch_finished(published["batch_id"], allow_failure=True)
                assert all(i["status"] == "failed" for i in failures["items"])
                assert all(detail(n)["ssh"]["credential_version"] == 1 for n in nodes)
                # Operator preparation is separate from product behavior: the
                # manager never adds or removes remote SSH authorization.
                for container, _, _ in endpoints:
                    matrix.run_input(["docker", "exec", "-i", container, "sh", "-c", "cat >> /root/.ssh/authorized_keys"], new_key.with_suffix(".pub").read_text())
                api(f"/api/v1/credential-batches/{published['batch_id']}/retry", "POST")
                batch_finished(published["batch_id"])
                assert all(detail(n)["ssh"]["credential_version"] == 2 for n in nodes)
                print("Unauthorized SSH keys left the old binding active; operator authorization and retry applied the encrypted key with its passphrase.", flush=True)

                old_password, new_password = secrets.token_hex(20), secrets.token_hex(20)
                for container, _, _ in endpoints:
                    matrix.run(["docker", "exec", container, "sh", "-c", "printf 'PermitRootLogin yes\\nPasswordAuthentication yes\\nPubkeyAuthentication yes\\n' > /etc/ssh/sshd_config.d/99-hysteriax-test.conf; systemctl reload ssh.service"])
                    matrix.run_input(["docker", "exec", "-i", container, "chpasswd"], f"root:{old_password}\n")
                password = create("Shared SSH password", "ssh_password", {"secret": old_password})
                for node in nodes:
                    api(f"/api/v1/nodes/{node}", "PATCH", {"expected_revision": detail(node)["revision"], "ssh_credential_id": password, "ssh_credential_version": 1})
                for container, _, _ in endpoints:
                    matrix.run_input(["docker", "exec", "-i", container, "chpasswd"], f"root:{new_password}\n")
                published = publish(password, {"secret": new_password})
                batch_finished(published["batch_id"])
                assert all(detail(n)["ssh"]["credential_version"] == 2 for n in nodes)
                print("Manually prepared password changes automatically applied to both shared references.", flush=True)
        finally:
            if service and service.poll() is None:
                service.terminate()
                try:
                    service.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.wait()
            for container in containers:
                subprocess.run(["docker", "rm", "--force", container], capture_output=True)
            subprocess.run(["docker", "image", "rm", image], capture_output=True)


if __name__ == "__main__":
    main()
