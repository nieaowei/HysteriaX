#!/usr/bin/env python3
"""Deploy one disposable Debian node and verify real Hysteria mTLS plus ECH traffic."""

import base64
from datetime import datetime, timedelta, timezone
import gzip
import hashlib
import json
import os
import pathlib
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

from postgres_test import PostgresTestSchema


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"
DOCKERFILE = ROOT / "scripts" / "fixtures" / "systemd-node.Dockerfile"
HYSTERIA_URL = "https://github.com/apernet/hysteria/releases/download/app/v2.12.3/hysteria-darwin-arm64"
HYSTERIA_SHA256 = "9065dc5dc9cd75f7ba881f481e8cb77e7eae17139460ca09d399682ca6fad443"
MIHOMO_URL = "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.31/mihomo-darwin-arm64-v1.19.31.gz"
MIHOMO_SHA256 = "d131f44b3deb2a8356f7ac75048ad67a10d53243323951c4f3cda7b672922963"


def run(command, check=True, timeout=300, input_text=None):
    result = subprocess.run(
        command, capture_output=True, text=True, timeout=timeout, input=input_text
    )
    if check and result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {' '.join(map(str, command))}\n"
            + result.stdout[-1000:]
            + result.stderr[-1000:]
        )
    return result


def stop_process_group(process):
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def request(base, path, token=None, method="GET", payload=None):
    from credential_test_fixtures import request_with_credentials
    return request_with_credentials(raw_request, base, path, token, method, payload)


def raw_request(base, path, token=None, method="GET", payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    headers = {}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=20) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def free_port(kind=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, kind) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def free_port_range(size, kind=socket.SOCK_DGRAM):
    for _ in range(100):
        first = free_port(kind)
        last = first + size - 1
        if last > 65535:
            continue
        listeners = []
        try:
            for port in range(first, last + 1):
                listener = socket.socket(socket.AF_INET, kind)
                listener.bind(("127.0.0.1", port))
                listeners.append(listener)
        except OSError:
            continue
        finally:
            for listener in listeners:
                listener.close()
        if len(listeners) == size:
            return first, last
    raise RuntimeError(f"could not reserve a free contiguous range of {size} ports")


def install_systemctl_restart_failpoint(temp, container, fail_on=2):
    wrapper = temp / "systemctl-wrapper"
    wrapper.write_text(
        "#!/bin/sh\n"
        "if [ \"$1\" = restart ] && [ \"$2\" = hysteriax.service ]; then\n"
        "  count=0\n"
        "  if [ -f /tmp/hysteriax-systemctl-restart-count ]; then IFS= read -r count < /tmp/hysteriax-systemctl-restart-count; fi\n"
        "  count=$((count + 1))\n"
        "  printf '%s\\n' \"$count\" > /tmp/hysteriax-systemctl-restart-count\n"
        f"  if [ \"$count\" -eq {fail_on} ]; then echo 'injected systemctl restart failure' >&2; exit 97; fi\n"
        "fi\n"
        "exec /usr/bin/systemctl.hx-real \"$@\"\n"
    )
    wrapper.chmod(0o700)
    run(["docker", "exec", container, "mv", "/usr/bin/systemctl", "/usr/bin/systemctl.hx-real"])
    run(["docker", "cp", str(wrapper), f"{container}:/usr/bin/systemctl"])
    run(["docker", "exec", container, "chmod", "0755", "/usr/bin/systemctl"])


def restore_systemctl(container):
    restore_code = (
        "import os; real='/usr/bin/systemctl.hx-real'; wrapper='/usr/bin/systemctl'; "
        "os.unlink(wrapper); os.replace(real, wrapper); "
        "counter='/tmp/hysteriax-systemctl-restart-count'; "
        "os.path.exists(counter) and os.unlink(counter)"
    )
    run(["docker", "exec", container, "python3", "-c", restore_code])


def wait_for_systemd(container, timeout=30):
    deadline = time.monotonic() + timeout
    last_message = "systemd has not reported a state yet"
    while time.monotonic() < deadline:
        result = run(
            ["docker", "exec", container, "systemctl", "is-system-running"],
            check=False,
            timeout=5,
        )
        state = result.stdout.strip()
        if state in {"running", "degraded"}:
            return
        if state == "maintenance":
            raise RuntimeError(f"systemd entered maintenance mode in {container}")
        last_message = (result.stdout + result.stderr).strip() or last_message
        time.sleep(0.25)
    raise RuntimeError(
        f"systemd did not become ready in {container} within {timeout}s: {last_message}"
    )


def main():
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)

    suffix = uuid.uuid4().hex[:8]
    container = f"hysteriax-mtls-{suffix}"
    image = f"hysteriax-mtls:{suffix}"
    ssh_port = free_port()
    hy2_first_port, hy2_last_port = free_port_range(8)
    socks_port = free_port()
    mihomo_port = free_port()
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-mtls-live-") as temporary:
        temp = pathlib.Path(temporary)
        ssh_key = temp / "id_ed25519"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(ssh_key)], check=True)
        run(
            [
                "docker", "build", "--platform", "linux/arm64",
                "--build-arg", "BASE_IMAGE=debian:trixie",
                "-f", str(DOCKERFILE), "-t", image, str(ROOT),
            ],
            timeout=900,
        )
        api = None
        client = None
        peer_client = None
        traffic_generator_processes = []
        mihomo = None
        client_log = None
        client_log_file = None
        peer_client_log_file = None
        mihomo_log_file = None
        secrets_to_scrub = []
        try:
            run(
                [
                    "docker", "run", "--privileged", "--cgroupns=host",
                    "--tmpfs", "/run", "--tmpfs", "/run/lock",
                    "--name", container,
                    "-p", f"127.0.0.1:{ssh_port}:22",
                    "-p", f"127.0.0.1:{hy2_first_port}-{hy2_last_port}:{hy2_first_port}-{hy2_last_port}/udp",
                    "-d", image,
                ]
            )
            wait_for_systemd(container)
            run(["docker", "cp", str(ssh_key.with_suffix(".pub")), f"{container}:/root/.ssh/authorized_keys"])
            run(
                [
                    "docker", "exec", container, "sh", "-c",
                    "mkdir -p /run/sshd && chown -R root:root /root/.ssh && chmod 0700 /root/.ssh "
                    "&& chmod 0600 /root/.ssh/authorized_keys && ssh-keygen -A && sshd -t "
                    "&& systemctl enable --now ssh.service",
                ]
            )
            run(
                [
                    "docker", "exec", container, "mkdir", "-p", "/tmp/hysteriax-mtls-target",
                ]
            )
            run(
                [
                    "docker", "exec", container, "python3", "-c",
                    "import os; root='/tmp/hysteriax-mtls-target'; "
                    "open(root+'/payload.bin','wb').write(os.urandom(2*1024*1024)); "
                    "open(root+'/kick.bin','wb').write(os.urandom(64*1024))",
                ]
            )
            run(
                [
                    "docker", "exec", "-d", container, "python3", "-m", "http.server",
                    "18081", "--bind", "0.0.0.0", "--directory", "/tmp/hysteriax-mtls-target",
                ]
            )

            with socket.socket() as listener:
                listener.bind(("0.0.0.0", 0))
                api_port = listener.getsockname()[1]
            base = f"http://127.0.0.1:{api_port}"
            public_url = f"http://host.docker.internal:{api_port}"
            admin = "hx_" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
            master_key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
            gecko_password = base64.urlsafe_b64encode(secrets.token_bytes(24)).decode().rstrip("=")
            secrets_to_scrub.extend((admin, master_key, gecko_password))
            environment = os.environ.copy()
            environment.update(
                {
                    "DATABASE_URL": database.url,
                    "HYSTERIAX_LISTEN_ADDR": f"0.0.0.0:{api_port}",
                    "HYSTERIAX_PUBLIC_URL": public_url,
                    "HYSTERIAX_ADMIN_TOKEN": admin,
                    "HYSTERIAX_MASTER_KEY": master_key,
                    "RUST_LOG": "warn",
                }
            )
            with open(temp / "management.log", "wb") as management_log:
                api = subprocess.Popen(
                    [str(SERVER)], cwd=ROOT, env=environment,
                    stdout=management_log, stderr=management_log,
                )

                def expect(path, method="GET", payload=None, statuses=(200, 201, 202), auth=True):
                    status, body = request(base, path, admin if auth else None, method, payload)
                    if status not in statuses:
                        raise RuntimeError(
                            f"{method} {path} returned HTTP {status}: {body[:500].decode(errors='replace')}"
                        )
                    return json.loads(body) if body else None

                def wait_job(job_id, timeout=180):
                    deadline = time.time() + timeout
                    last_detail = None
                    while time.time() < deadline:
                        detail = expect(f"/api/v1/jobs/{job_id}")
                        last_detail = detail
                        if detail["job"]["status"] in ("succeeded", "failed", "cancelled", "rolled_back"):
                            return detail
                        time.sleep(0.5)
                    raise TimeoutError(f"job {job_id} did not finish; last detail: {last_detail}")

                def wait_retry_wait(job_id, timeout=20):
                    deadline = time.time() + timeout
                    while time.time() < deadline:
                        job = expect(f"/api/v1/jobs/{job_id}")["job"]
                        if job["status"] == "queued" and job["stage"] == "retry_wait":
                            return job
                        if job["status"] in ("failed", "rolled_back", "cancelled"):
                            return job
                        time.sleep(0.2)
                    raise TimeoutError(f"job {job_id} did not enter retry_wait")

                ready = False
                for _ in range(60):
                    if api.poll() is not None:
                        raise RuntimeError("temporary HysteriaX API exited")
                    try:
                        status, _ = request(base, "/readyz")
                        if status == 200:
                            ready = True
                            break
                    except (OSError, urllib.error.URLError):
                        pass
                    time.sleep(0.5)
                if not ready:
                    raise RuntimeError("temporary HysteriaX API did not become ready")

                ca_key = temp / "client-ca-key.pem"
                ca_cert = temp / "client-ca.pem"
                server_key = temp / "server-key.pem"
                server_cert = temp / "server-cert.pem"
                client_key = temp / "client-key.pem"
                client_csr = temp / "client.csr"
                client_cert = temp / "client-cert.pem"
                client_ext = temp / "client.ext"
                subprocess.run(
                    [
                        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(ca_key), "-out", str(ca_cert), "-days", "2",
                        "-subj", "/CN=HysteriaX mTLS test CA",
                        "-addext", "basicConstraints=critical,CA:TRUE",
                        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
                    ],
                    check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                subprocess.run(
                    [
                        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(server_key), "-out", str(server_cert), "-days", "2",
                        "-subj", "/CN=127.0.0.1",
                        "-addext", "subjectAltName=IP:127.0.0.1",
                    ],
                    check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                subprocess.run(
                    [
                        "openssl", "req", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(client_key), "-out", str(client_csr), "-subj", "/CN=mtls-user",
                    ],
                    check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                client_ext.write_text(
                    "basicConstraints=critical,CA:FALSE\n"
                    "keyUsage=critical,digitalSignature,keyEncipherment\n"
                    "extendedKeyUsage=clientAuth\n"
                )
                subprocess.run(
                    [
                        "openssl", "x509", "-req", "-in", str(client_csr),
                        "-CA", str(ca_cert), "-CAkey", str(ca_key), "-CAcreateserial",
                        "-out", str(client_cert), "-days", "2", "-extfile", str(client_ext),
                    ],
                    check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )

                client_binary = temp / "hysteria"
                with urllib.request.urlopen(HYSTERIA_URL, timeout=90) as response:
                    client_bytes = response.read()
                if hashlib.sha256(client_bytes).hexdigest() != HYSTERIA_SHA256:
                    raise RuntimeError("pinned Hysteria client digest mismatch")
                client_binary.write_bytes(client_bytes)
                client_binary.chmod(0o700)
                ech_key = temp / "ech.pem"
                run([
                    str(client_binary), "ech", "--public-name", "decoy.example.test",
                    "--output", str(ech_key),
                ])
                ech_pem = ech_key.read_text()
                ech_config = "".join(
                    ech_pem.split("-----BEGIN ECH CONFIGS-----", 1)[1]
                    .split("-----END ECH CONFIGS-----", 1)[0]
                    .split()
                )

                node = expect(
                    "/api/v1/nodes", "POST",
                    {
                        "name": "Debian 13 mTLS acceptance",
                        "ssh_host": "127.0.0.1", "ssh_port": ssh_port,
                        "ssh_username": "root", "ssh_auth_type": "private_key",
                        "ssh_secret": ssh_key.read_text(),
                        "public_host": "127.0.0.1", "public_port": hy2_first_port,
                        "listen_addr": f":{hy2_first_port}-{hy2_last_port}", "tls_sni": "127.0.0.1",
                        "tls_skip_verify": True,
                        "config": {
                            "acl": {"inline": ["direct(all)"]},
                            "outbounds": [{"name": "direct", "type": "direct"}],
                        },
                    },
                    (201,),
                )
                node_id = node["node"]["id"]
                node_token = node["node_auth_token"]
                secrets_to_scrub.append(node_token)
                ssh_job = expect(
                    f"/api/v1/nodes/{node_id}/ssh-test", "POST", {"expected_revision": 1}
                )["job_id"]
                ssh_result = wait_job(ssh_job, timeout=90)
                fingerprint = ((ssh_result.get("result") or {}).get("result") or {}).get("fingerprint")
                if ssh_result["job"]["status"] != "succeeded" or not fingerprint:
                    raise RuntimeError("SSH fingerprint discovery failed")
                expect(
                    f"/api/v1/nodes/{node_id}", "PATCH",
                    {"expected_revision": 1, "ssh_host_fingerprint": fingerprint},
                )

                wrong_fingerprint_node = expect(
                    "/api/v1/nodes",
                    "POST",
                    {
                        "name": "Wrong fingerprint failure probe",
                        "ssh_host": "127.0.0.1", "ssh_port": ssh_port,
                        "ssh_username": "root", "ssh_auth_type": "private_key",
                        "ssh_secret": ssh_key.read_text(),
                        "ssh_host_fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                        "public_host": "127.0.0.1", "public_port": 443,
                        "listen_addr": ":443", "config": {},
                    },
                    (201,),
                )
                wrong_fingerprint_id = wrong_fingerprint_node["node"]["id"]
                wrong_fingerprint_job = expect(
                    f"/api/v1/nodes/{wrong_fingerprint_id}/deploy",
                    "POST",
                    {"expected_revision": 1},
                )["job_id"]
                wrong_fingerprint_result = wait_job(wrong_fingerprint_job)
                if (
                    wrong_fingerprint_result["job"]["status"] != "failed"
                    or "SSH host key changed" not in (wrong_fingerprint_result["job"].get("error_message") or "")
                    or expect(f"/api/v1/nodes/{wrong_fingerprint_id}")["state"] != "fingerprint_changed"
                ):
                    raise RuntimeError("wrong SSH fingerprint was not rejected and marked on the node")

                run(["docker", "exec", container, "mkdir", "-p", "/opt/hysteriax"])
                unmanaged_node = expect(
                    "/api/v1/nodes",
                    "POST",
                    {
                        "name": "Unmanaged installation failure probe",
                        "ssh_host": "127.0.0.1", "ssh_port": ssh_port,
                        "ssh_username": "root", "ssh_auth_type": "private_key",
                        "ssh_secret": ssh_key.read_text(),
                        "ssh_host_fingerprint": fingerprint,
                        "public_host": "127.0.0.1", "public_port": 443,
                        "listen_addr": ":443", "config": {},
                    },
                    (201,),
                )
                unmanaged_id = unmanaged_node["node"]["id"]
                unmanaged_job = expect(
                    f"/api/v1/nodes/{unmanaged_id}/deploy",
                    "POST",
                    {"expected_revision": 1},
                )["job_id"]
                unmanaged_result = wait_job(unmanaged_job)
                if (
                    unmanaged_result["job"]["status"] != "failed"
                    or "unmanaged directory already exists"
                    not in (unmanaged_result["job"].get("error_message") or "")
                ):
                    raise RuntimeError("unmanaged remote installation was not rejected")
                run(["docker", "exec", container, "rmdir", "/opt/hysteriax"])

                port_probe_node = expect(
                    "/api/v1/nodes",
                    "POST",
                    {
                        "name": "Occupied UDP port preflight probe",
                        "ssh_host": "127.0.0.1", "ssh_port": ssh_port,
                        "ssh_username": "root", "ssh_auth_type": "private_key",
                        "ssh_secret": ssh_key.read_text(),
                        "ssh_host_fingerprint": fingerprint,
                        "public_host": "127.0.0.1", "public_port": hy2_first_port,
                        "listen_addr": f":{hy2_first_port}", "tls_sni": "127.0.0.1",
                        "tls_skip_verify": True,
                        "proxy_probe_url": "http://127.0.0.1:18081/kick.bin",
                        "config": {
                            "acl": {"inline": ["reject(127.0.0.1/32, tcp/9780)", "direct(all)"]},
                            "outbounds": [{"name": "direct", "type": "direct"}],
                        },
                    },
                    (201,),
                )
                port_probe_id = port_probe_node["node"]["id"]
                port_probe_token = port_probe_node["node_auth_token"]
                secrets_to_scrub.append(port_probe_token)
                port_probe_cert = expect(
                    f"/api/v1/nodes/{port_probe_id}/resources", "POST",
                    {
                        "name": "port-probe-cert.pem", "resource_kind": "certificate",
                        "content_base64": base64.b64encode(server_cert.read_bytes()).decode(),
                    },
                    (201,),
                )["reference"]
                port_probe_key = expect(
                    f"/api/v1/nodes/{port_probe_id}/resources", "POST",
                    {
                        "name": "port-probe-key.pem", "resource_kind": "private_key",
                        "content_base64": base64.b64encode(server_key.read_bytes()).decode(),
                    },
                    (201,),
                )["reference"]
                blocker_unit = f"hysteriax-port-blocker-{suffix}"
                blocker_code = (
                    "import socket,time; sock=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); "
                    f"sock.bind(('0.0.0.0',{hy2_first_port})); time.sleep(600)"
                )
                run([
                    "docker", "exec", container, "systemd-run", "--unit", blocker_unit,
                    "--property=Restart=no", "python3", "-c", blocker_code,
                ])
                run(["docker", "exec", container, "systemctl", "is-active", blocker_unit])
                blocker_ready = False
                for _ in range(30):
                    bind_check = run(
                        [
                            "docker", "exec", container, "python3", "-c",
                            f"import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(('0.0.0.0',{hy2_first_port}))",
                        ],
                        check=False,
                    )
                    if bind_check.returncode != 0 and "address already in use" in bind_check.stderr.lower():
                        blocker_ready = True
                        break
                    time.sleep(0.2)
                if not blocker_ready:
                    raise RuntimeError("UDP blocker service did not bind the probe port")
                port_probe_update = expect(
                    f"/api/v1/nodes/{port_probe_id}",
                    "PATCH",
                    {
                        "expected_revision": 1,
                        "config": {
                            "tls": {
                                "cert": port_probe_cert,
                                "key": port_probe_key,
                                "sniGuard": "disable",
                            }
                        },
                    },
                )
                port_probe_revision = port_probe_update["revision"]
                port_probe_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("node_id") == port_probe_id
                    and job.get("kind") == "sync"
                    and job.get("target_revision") == port_probe_revision
                )
                port_conflict = wait_job(port_probe_job["id"])
                if (
                    port_conflict["job"]["status"] != "failed"
                    or "configured UDP listener port is already occupied"
                    not in (port_conflict["job"].get("error_message") or "")
                ):
                    raise RuntimeError(
                        "UDP listener port conflict was not caught by preflight: "
                        + str(port_conflict)
                    )
                install_path = run(
                    ["docker", "exec", container, "test", "-e", "/opt/hysteriax"],
                    check=False,
                )
                if install_path.returncode == 0:
                    raise RuntimeError("UDP port preflight wrote remote install files before rejecting the conflict")
                run(["docker", "exec", container, "systemctl", "stop", blocker_unit], check=False)
                blocker_status = run(
                    ["docker", "exec", container, "systemctl", "is-active", blocker_unit],
                    check=False,
                )
                release_check = run(
                    [
                        "docker", "exec", container, "python3", "-c",
                        f"import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(('0.0.0.0',{hy2_first_port})); s.close()",
                    ],
                    check=False,
                )
                if blocker_status.returncode == 0 or release_check.returncode != 0:
                    raise RuntimeError(
                        "UDP blocker did not release the listener port: "
                        + blocker_status.stdout.strip()
                        + " "
                        + release_check.stderr[-500:]
                    )
                run(["docker", "exec", container, "systemctl", "reset-failed", blocker_unit], check=False)
                retry_port_job = expect(
                    f"/api/v1/nodes/{port_probe_id}/sync",
                    "POST",
                    {"expected_revision": port_probe_revision},
                )["job_id"]
                retry_port_result = wait_job(retry_port_job)
                if retry_port_result["job"]["status"] != "succeeded":
                    raise RuntimeError(
                        "deployment did not succeed after releasing the UDP port: "
                        + str(retry_port_result)
                    )
                port_proxy_probe = retry_port_result["job"]["result"]["result"].get("proxy_probe")
                if not port_proxy_probe or port_proxy_probe.get("route_check") != "custom_tcp_forwarding":
                    raise RuntimeError("configured HTTP probe URL did not pass the direct ACL/outbound TCP probe")
                restricted_update = expect(
                    f"/api/v1/nodes/{port_probe_id}",
                    "PATCH",
                    {
                        "expected_revision": port_probe_revision,
                        "config": {
                            "tls": {
                                "cert": port_probe_cert,
                                "key": port_probe_key,
                                "sniGuard": "disable",
                            },
                            "acl": {"inline": ["reject(127.0.0.1/32)", "direct(all)"]},
                            "outbounds": [{"name": "direct", "type": "direct"}],
                        },
                        "proxy_probe_url": "",
                    },
                )
                restricted_revision = restricted_update["revision"]
                restricted_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("node_id") == port_probe_id
                    and job.get("kind") == "sync"
                    and job.get("target_revision") == restricted_revision
                )
                restricted_result = wait_job(restricted_job["id"])
                restricted_proxy_probe = restricted_result["job"]["result"]["result"].get("proxy_probe")
                if (
                    restricted_result["job"]["status"] != "succeeded"
                    or not restricted_proxy_probe
                    or restricted_proxy_probe.get("route_check") != "authenticated_session"
                ):
                    raise RuntimeError("restricted ACL/outbound route did not report its authenticated client-session probe")
                port_probe_revision = restricted_revision
                port_probe_deletion = expect(
                    f"/api/v1/nodes/{port_probe_id}?expected_revision={port_probe_revision}",
                    "DELETE",
                    statuses=(202,),
                )
                port_probe_uninstall = wait_job(port_probe_deletion["job_id"])
                if port_probe_uninstall["job"]["status"] != "succeeded":
                    raise RuntimeError("UDP preflight probe node failed to uninstall")
                print("UDP port occupancy was rejected before installation; sync succeeded after releasing the port.")

                retry_node = expect(
                    "/api/v1/nodes",
                    "POST",
                    {
                        "name": "SSH interruption retry probe",
                        "ssh_host": "127.0.0.1", "ssh_port": ssh_port,
                        "ssh_username": "root", "ssh_auth_type": "private_key",
                        "ssh_secret": ssh_key.read_text(),
                        "ssh_host_fingerprint": fingerprint,
                        "public_host": "127.0.0.1", "public_port": 443,
                        "listen_addr": ":443", "config": {},
                    },
                    (201,),
                )
                retry_node_id = retry_node["node"]["id"]
                run(["docker", "exec", container, "systemctl", "stop", "ssh.service"])
                interrupted_job = expect(
                    f"/api/v1/nodes/{retry_node_id}/deploy",
                    "POST",
                    {"expected_revision": 1},
                )["job_id"]
                retry_state = wait_retry_wait(interrupted_job)
                if retry_state["status"] != "queued" or retry_state["stage"] != "retry_wait":
                    raise RuntimeError("SSH connection interruption was not retried")
                run(["docker", "exec", container, "systemctl", "start", "ssh.service"])
                expect(
                    f"/api/v1/nodes/{retry_node_id}?expected_revision=1",
                    "DELETE",
                    statuses=(204,),
                )
                cancelled_retry = expect(f"/api/v1/jobs/{interrupted_job}")["job"]
                if cancelled_retry["status"] != "cancelled":
                    raise RuntimeError("queued SSH retry did not cancel when its node was deleted")

                def upload_resource(name, kind, content):
                    return expect(
                        f"/api/v1/nodes/{node_id}/resources", "POST",
                        {
                            "name": name,
                            "resource_kind": kind,
                            "content_base64": base64.b64encode(content).decode(),
                        },
                        (201,),
                    )["reference"]

                server_cert_ref = upload_resource("server-cert.pem", "certificate", server_cert.read_bytes())
                server_key_ref = upload_resource("server-key.pem", "private_key", server_key.read_bytes())
                ca_ref = upload_resource("client-ca.pem", "certificate", ca_cert.read_bytes())
                ech_ref = upload_resource("ech.pem", "ech_key", ech_key.read_bytes())
                server_options = {
                    "tls": {
                        "cert": server_cert_ref,
                        "key": server_key_ref,
                        "clientCA": ca_ref,
                        "sniGuard": "disable",
                    },
                    "ech": {"keyPath": ech_ref},
                    "obfs": {
                        "type": "gecko",
                        "gecko": {
                            "password": gecko_password,
                            "minPacketSize": 512,
                            "maxPacketSize": 1200,
                        },
                    },
                }
                user = expect("/api/v1/users", "POST", {"name": "mTLS acceptance user"}, (201,))
                user_id = user["id"]
                assignment = expect(
                    f"/api/v1/users/{user_id}/assignments", "POST",
                    {
                        "expected_revision": 1,
                        "node_id": node_id,
                        "client_certificate": client_cert.read_text(),
                        "client_private_key": client_key.read_text(),
                    },
                    (201,),
                )
                credential = assignment["hy2_credential"]
                secrets_to_scrub.append(credential)
                # Queue the deployment while SSH is interrupted. The job must retry and resume.
                run(["docker", "exec", container, "systemctl", "stop", "ssh.service"])
                node_update = expect(
                    f"/api/v1/nodes/{node_id}", "PATCH",
                    {
                        "expected_revision": 2,
                        "config": server_options,
                    },
                )
                node_revision = node_update["revision"]
                deploy_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("node_id") == node_id
                    and job.get("kind") == "sync"
                    and job.get("target_revision") == node_revision
                )
                retrying = wait_retry_wait(deploy_job["id"])
                if retrying["status"] != "queued":
                    raise RuntimeError(
                        "SSH interruption was not scheduled for retry: "
                        + str(retrying.get("error_message"))
                    )
                run(["docker", "exec", container, "systemctl", "start", "ssh.service"])
                deployed = wait_job(deploy_job["id"], timeout=300)
                if deployed["job"]["status"] != "succeeded":
                    raise RuntimeError("deployment did not recover after SSH returned: " + str(deployed["job"].get("error_message")))
                proxy_probe = deployed["job"]["result"]["result"].get("proxy_probe")
                if (
                    not proxy_probe
                    or proxy_probe.get("status") != "passed"
                    or proxy_probe.get("route_check") != "tcp_forwarding"
                ):
                    raise RuntimeError("deployment succeeded without its mTLS+ECH Hysteria TCP forwarding probe")
                with database.connect() as connection:
                    active_probes = connection.execute(
                        "SELECT COUNT(*) FROM deployment_probe_tokens"
                    ).fetchone()[0]
                if active_probes != 0:
                    raise RuntimeError("temporary deployment probe credentials were not removed after health checks")

                subscription = expect(
                    f"/api/v1/users/{user_id}/subscription", "POST", {"expected_revision": 2}
                )
                sub_token = subscription["token"]
                secrets_to_scrub.append(sub_token)
                status, subscription_yaml = request(
                    base, f"/sub/{sub_token}/clash.yaml", method="GET"
                )
                if (
                    status != 200
                    or b"certificate:" not in subscription_yaml
                    or b"private-key:" not in subscription_yaml
                    or b"ech-opts:" not in subscription_yaml
                    or ech_config.encode() not in subscription_yaml
                ):
                    raise RuntimeError("mTLS/ECH subscription omitted the client certificate pair or ECH config")
                subscription_path = temp / "mTLS-subscription.yaml"
                subscription_path.write_bytes(subscription_yaml)
                run([str(ROOT / "scripts/verify-mihomo-config.sh"), str(subscription_path)], timeout=180)

                client_config = temp / "client.yaml"
                client_config.write_text(
                    f"server: 127.0.0.1:{hy2_first_port}-{hy2_last_port}\n"
                    f"auth: {credential}\n"
                    "tls:\n"
                    "  sni: 127.0.0.1\n"
                    "  insecure: true\n"
                    f"  ech: {ech_config}\n"
                    f"  clientCertificate: {client_cert}\n"
                    f"  clientKey: {client_key}\n"
                    "transport:\n"
                    "  udp:\n"
                    "    hopInterval: 5s\n"
                    "obfs:\n"
                    "  type: gecko\n"
                    "  gecko:\n"
                    f"    password: {gecko_password}\n"
                    "    minPacketSize: 512\n"
                    "    maxPacketSize: 1200\n"
                    "socks5:\n"
                    f"  listen: 127.0.0.1:{socks_port}\n"
                )
                client_log_file = open(temp / "client.log", "wb")
                client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(client_config)],
                    stdout=client_log_file,
                    stderr=client_log_file,
                )
                socks_ready = False
                for _ in range(30):
                    if client.poll() is not None:
                        raise RuntimeError("mTLS Hysteria client exited before SOCKS5 startup")
                    try:
                        with socket.create_connection(("127.0.0.1", socks_port), timeout=1):
                            pass
                        socks_ready = True
                        break
                    except OSError:
                        time.sleep(0.5)
                if not socks_ready:
                    raise RuntimeError("mTLS Hysteria client SOCKS5 listener did not start")
                payload_path = temp / "mTLS-payload.bin"
                transfer = run(
                    [
                        "curl", "--noproxy", "", "--fail", "--silent", "--show-error", "--limit-rate", "256k",
                        "--max-time", "25", "--socks5-hostname", f"127.0.0.1:{socks_port}",
                        "http://127.0.0.1:18081/payload.bin", "-o", str(payload_path),
                    ],
                    timeout=30,
                )
                _ = transfer
                if payload_path.stat().st_size == 0:
                    raise RuntimeError("mTLS TCP proxy returned an empty payload")

                if client.poll() is None:
                    client.terminate()
                    client.wait(timeout=5)
                if client_log_file and not client_log_file.closed:
                    client_log_file.close()
                client_log_file = None
                client = None

                mihomo_gzip = temp / "mihomo.gz"
                with urllib.request.urlopen(MIHOMO_URL, timeout=90) as response:
                    mihomo_bytes = response.read()
                if hashlib.sha256(mihomo_bytes).hexdigest() != MIHOMO_SHA256:
                    raise RuntimeError("pinned Mihomo v1.19.31 digest mismatch")
                mihomo_binary = temp / "mihomo"
                mihomo_binary.write_bytes(gzip.decompress(mihomo_bytes))
                mihomo_binary.chmod(0o700)
                mihomo_gzip.write_bytes(mihomo_bytes)
                mihomo_home = temp / "mihomo-home"
                mihomo_home.mkdir()
                rewrite_group = (
                    'require "yaml"; path=ARGV[0]; config=YAML.load_file(path); '
                    'config["mixed-port"]=Integer(ARGV[1]); '
                    'proxy=config.fetch("proxies").first.fetch("name"); '
                    'proxy_entry=config.fetch("proxies").first; '
                    'proxy_entry["hop-interval"]=5 if proxy_entry["ports"]; '
                    'group=config.fetch("proxy-groups").find { |entry| entry["type"] == "select" }; '
                    'group["proxies"]=[proxy, "DIRECT"]; '
                    'File.write(path, YAML.dump(config))'
                )
                run(["ruby", "-e", rewrite_group, str(subscription_path), str(mihomo_port)])
                mihomo_log_file = open(temp / "mihomo.log", "wb")
                mihomo = subprocess.Popen(
                    [str(mihomo_binary), "-d", str(mihomo_home), "-f", str(subscription_path)],
                    stdout=mihomo_log_file,
                    stderr=mihomo_log_file,
                )
                proxy_ready = False
                for _ in range(40):
                    if mihomo.poll() is not None:
                        raise RuntimeError("Mihomo exited before opening its mixed port")
                    try:
                        with socket.create_connection(("127.0.0.1", mihomo_port), timeout=1):
                            pass
                        proxy_ready = True
                        break
                    except OSError:
                        time.sleep(0.5)
                if not proxy_ready:
                    raise RuntimeError("Mihomo mixed port did not become ready")
                mihomo_payload = temp / "mihomo-payload.bin"
                mihomo_transfer = run(
                    [
                        "curl", "--noproxy", "", "--fail", "--silent", "--show-error", "--limit-rate", "256k",
                        "--max-time", "25", "--proxy", f"http://127.0.0.1:{mihomo_port}",
                        "http://127.0.0.1:18081/payload.bin", "-o", str(mihomo_payload),
                    ],
                    timeout=30,
                )
                _ = mihomo_transfer
                if mihomo_payload.stat().st_size == 0:
                    raise RuntimeError("Mihomo mTLS proxy returned an empty payload")
                mihomo.terminate()
                mihomo.wait(timeout=5)
                if mihomo_log_file and not mihomo_log_file.closed:
                    mihomo_log_file.close()
                mihomo = None

                install_systemctl_restart_failpoint(temp, container, fail_on=1)
                start_failure_config = expect(f"/api/v1/nodes/{node_id}")["config"]
                start_failure_config["speedTest"] = True
                start_failure_update = expect(
                    f"/api/v1/nodes/{node_id}",
                    "PATCH",
                    {"expected_revision": node_revision, "config": start_failure_config},
                )
                node_revision = start_failure_update["revision"]
                start_failure_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("node_id") == node_id
                    and job.get("kind") == "sync"
                    and job.get("target_revision") == node_revision
                )
                start_failure_result = wait_job(start_failure_job["id"], timeout=120)
                if (
                    start_failure_result["job"]["status"] != "rolled_back"
                    or start_failure_result["job"]["stage"] != "rolled_back"
                    or "previous configuration was restored"
                    not in (start_failure_result["job"].get("error_message") or "")
                ):
                    raise RuntimeError(
                        "service-start failure did not restore the previous configuration: "
                        + str(start_failure_result)
                        + "; node state: "
                        + str(expect(f"/api/v1/nodes/{node_id}").get("state"))
                    )
                if expect(f"/api/v1/nodes/{node_id}")["state"] != "rolled_back":
                    raise RuntimeError("node state did not record the successful automatic rollback")
                restore_systemctl(container)
                retry_start_sync_job = expect(
                    f"/api/v1/nodes/{node_id}/sync",
                    "POST",
                    {"expected_revision": node_revision},
                )["job_id"]
                retry_start_sync = wait_job(retry_start_sync_job)
                if retry_start_sync["job"]["status"] != "succeeded":
                    raise RuntimeError("sync did not recover after the service-start rollback")
                print("Injected service-start failure restored the prior config; retry sync succeeded.")

                # Block the health API after a new service starts, then fail the
                # automatic rollback restart itself.
                run([
                    "docker", "exec", container, "iptables", "-I", "OUTPUT", "1",
                    "-o", "lo", "-p", "tcp", "--dport", "9780", "-j", "REJECT",
                    "--reject-with", "tcp-reset",
                ])
                install_systemctl_restart_failpoint(temp, container)
                changed_config = expect(f"/api/v1/nodes/{node_id}")["config"]
                changed_config["ignoreClientBandwidth"] = True
                failed_update = expect(
                    f"/api/v1/nodes/{node_id}",
                    "PATCH",
                    {"expected_revision": node_revision, "config": changed_config},
                )
                node_revision = failed_update["revision"]
                rollback_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("node_id") == node_id
                    and job.get("kind") == "sync"
                    and job.get("target_revision") == node_revision
                )
                rollback_failure = wait_job(rollback_job["id"], timeout=120)
                if (
                    rollback_failure["job"]["status"] != "failed"
                    or rollback_failure["job"]["stage"] != "rollback_failed"
                    or "automatic rollback also failed"
                    not in (rollback_failure["job"].get("error_message") or "")
                ):
                    raise RuntimeError(
                        "forced service failure did not produce rollback_failed: "
                        + str(rollback_failure)
                        + "; node state: "
                        + str(expect(f"/api/v1/nodes/{node_id}").get("state"))
                    )
                rollback_state = expect(f"/api/v1/nodes/{node_id}")
                if rollback_state["state"] != "rollback_failed":
                    raise RuntimeError("node state did not record the failed rollback")

                restore_systemctl(container)
                run([
                    "docker", "exec", container, "iptables", "-D", "OUTPUT",
                    "-o", "lo", "-p", "tcp", "--dport", "9780", "-j", "REJECT",
                    "--reject-with", "tcp-reset",
                ])
                run(["docker", "exec", container, "systemctl", "reset-failed", "hysteriax.service"], check=False)
                run(["docker", "exec", container, "systemctl", "restart", "hysteriax.service"])
                retry_sync_job = expect(
                    f"/api/v1/nodes/{node_id}/sync",
                    "POST",
                    {"expected_revision": node_revision},
                )["job_id"]
                retry_sync = wait_job(retry_sync_job)
                if retry_sync["job"]["status"] != "succeeded":
                    raise RuntimeError(
                        "explicit sync did not recover after rollback failure: "
                        + str(retry_sync)
                        + "; node state: "
                        + str(expect(f"/api/v1/nodes/{node_id}").get("state"))
                    )
                print("Forced health-check and rollback-restart failure was recorded; sync recovered after removing the injected failure.")

                usage_path = f"/api/v1/users/{user_id}/usage"
                usage_before = None
                for _ in range(40):
                    usage_before = expect(usage_path)["usage_bytes"]
                    if usage_before > 0:
                        break
                    time.sleep(0.5)
                if usage_before is None or usage_before == 0:
                    raise RuntimeError("live traffic was not sampled before the SSH outage test")

                run(["docker", "exec", container, "systemctl", "stop", "ssh.service"])
                outage_observed = False
                for _ in range(40):
                    usage_status = expect(usage_path)
                    node_status = expect(f"/api/v1/nodes/{node_id}")
                    if (
                        usage_status["data_freshness"]["open_gaps"] > 0
                        and node_status["state"] == "unreachable"
                    ):
                        outage_observed = True
                        break
                    time.sleep(0.5)
                if not outage_observed:
                    raise RuntimeError("SSH sampling outage did not open a data gap")

                client_log_file = open(temp / "client.log", "wb")
                client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(client_config)],
                    stdout=client_log_file,
                    stderr=client_log_file,
                )
                socks_ready = False
                for _ in range(30):
                    if client.poll() is not None:
                        raise RuntimeError("Hysteria client exited during the SSH sampling outage")
                    try:
                        with socket.create_connection(("127.0.0.1", socks_port), timeout=1):
                            pass
                        socks_ready = True
                        break
                    except OSError:
                        time.sleep(0.5)
                if not socks_ready:
                    raise RuntimeError("Hysteria client SOCKS5 did not start during the SSH outage")
                outage_payload = temp / "ssh-outage-payload.bin"
                outage_transfer = run(
                    [
                        "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                        "--limit-rate", "256k", "--max-time", "25",
                        "--socks5-hostname", f"127.0.0.1:{socks_port}",
                        "http://127.0.0.1:18081/payload.bin", "-o", str(outage_payload),
                    ],
                    timeout=30,
                )
                if outage_payload.stat().st_size == 0:
                    raise RuntimeError("proxy traffic stopped when only the SSH management path was down")
                client.terminate()
                client.wait(timeout=5)
                client_log_file.close()
                client_log_file = None
                client = None

                run(["docker", "exec", container, "systemctl", "start", "ssh.service"])
                usage_recovered = None
                for _ in range(60):
                    usage_recovered = expect(usage_path)
                    node_status = expect(f"/api/v1/nodes/{node_id}")
                    if (
                        usage_recovered["usage_bytes"] > usage_before
                        and usage_recovered["data_freshness"]["open_gaps"] == 0
                        and node_status["state"] == "deployed"
                        and node_status["last_sample_at"] is not None
                        and node_status["data_freshness"] == "fresh"
                        and node_status["open_gaps"] == 0
                    ):
                        break
                    time.sleep(0.5)
                else:
                    raise RuntimeError("traffic sampling did not recover after SSH access returned")
                recovered_usage = usage_recovered["usage_bytes"]
                time.sleep(11)
                duplicate_sample = expect(usage_path)["usage_bytes"]
                if duplicate_sample != recovered_usage:
                    raise RuntimeError("repeated post-outage sampling duplicated traffic usage")

                peer_socks_port = free_port()
                peer_client_config = temp / "revocation-peer-client.yaml"
                peer_config_text = client_config.read_text().replace(
                    f"  listen: 127.0.0.1:{socks_port}\n",
                    f"  listen: 127.0.0.1:{peer_socks_port}\n",
                )
                if peer_config_text == client_config.read_text():
                    raise RuntimeError("could not create the second Hysteria client configuration")
                peer_client_config.write_text(peer_config_text)
                client_log_file = open(temp / "revocation-client-one.log", "wb")
                client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(client_config)],
                    stdout=client_log_file,
                    stderr=client_log_file,
                )
                peer_client_log_file = open(temp / "revocation-client-two.log", "wb")
                peer_client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(peer_client_config)],
                    stdout=peer_client_log_file,
                    stderr=peer_client_log_file,
                )
                socks_ready = {socks_port: False, peer_socks_port: False}
                for _ in range(40):
                    if client.poll() is not None or peer_client.poll() is not None:
                        raise RuntimeError("one of the Hysteria clients exited before revocation testing")
                    for port in socks_ready:
                        if socks_ready[port]:
                            continue
                        try:
                            with socket.create_connection(("127.0.0.1", port), timeout=1):
                                socks_ready[port] = True
                        except OSError:
                            pass
                    if all(socks_ready.values()):
                        break
                    time.sleep(0.25)
                if not all(socks_ready.values()):
                    raise RuntimeError("both Hysteria clients did not open their SOCKS5 listeners")

                deployed_config = run(
                    ["docker", "exec", container, "cat", "/etc/hysteriax/config.yaml"]
                ).stdout
                in_stats_section = False
                stats_secret = None
                for line in deployed_config.splitlines():
                    if line == "trafficStats:":
                        in_stats_section = True
                    elif in_stats_section and line and not line[0].isspace():
                        break
                    elif in_stats_section and line.strip().startswith("secret:"):
                        stats_secret = line.split(":", 1)[1].strip()
                        break
                if not stats_secret:
                    raise RuntimeError("could not read the fixture node's local stats secret")
                secrets_to_scrub.append(stats_secret)

                def stats_get(path):
                    result = run(
                        [
                            "docker", "exec", "-i", container, "sh", "-c",
                            'IFS= read -r auth; curl --fail --silent --show-error '
                            f'-H "Authorization: $auth" http://127.0.0.1:9780{path}',
                        ],
                        input_text=stats_secret + "\n",
                    )
                    return json.loads(result.stdout)

                def online_connections():
                    return stats_get("/online")

                online = {}
                for _ in range(20):
                    online = online_connections()
                    if online.get(user_id, 0) >= 2:
                        break
                    time.sleep(0.5)
                if online.get(user_id, 0) < 2:
                    raise RuntimeError("the Hysteria node did not report both user clients online")

                def user_traffic_bytes(client_id=user_id):
                    counters = stats_get("/traffic").get(client_id, {})
                    return int(counters.get("tx", 0)) + int(counters.get("rx", 0))

                traffic_before_loops = user_traffic_bytes()
                for port, output in ((socks_port, temp / "kick-client-one.bin"), (peer_socks_port, temp / "kick-client-two.bin")):
                    run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", str(output),
                        ],
                        timeout=10,
                    )
                    if output.stat().st_size == 0:
                        raise RuntimeError("an active Hysteria client failed its kick-test transfer")

                # Hysteria consumes a kick marker on the next traffic callback, so keep
                # short TCP streams active on both client instances during revocation.
                traffic_loop = (
                    "while :; do curl --noproxy '' --fail --silent --show-error --max-time 3 "
                    "--limit-rate 128k "
                    "--socks5-hostname \"127.0.0.1:$1\" "
                    "http://127.0.0.1:18081/kick.bin -o /dev/null >/dev/null 2>&1 || true; "
                    "sleep 0.05; done"
                )
                for port in (socks_port, peer_socks_port):
                    process = subprocess.Popen(
                        ["sh", "-c", traffic_loop, "hysteriax-kick-loop", str(port)],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        start_new_session=True,
                    )
                    traffic_generator_processes.append(process)
                time.sleep(0.5)
                if any(process.poll() is not None for process in traffic_generator_processes):
                    raise RuntimeError("continuous proxy traffic did not start for both active clients")
                for _ in range(30):
                    if user_traffic_bytes() > traffic_before_loops and online_connections().get(user_id, 0) >= 2:
                        break
                    time.sleep(0.2)
                else:
                    raise RuntimeError("continuous proxy traffic was not sampled from both online clients")

                current_user = expect(f"/api/v1/users/{user_id}")
                disabled_user = expect(
                    f"/api/v1/users/{user_id}",
                    "PATCH",
                    {"expected_revision": current_user["revision"], "enabled": False},
                )
                if disabled_user["enabled"]:
                    raise RuntimeError("user disable did not take effect")
                auth_status, auth_body = request(
                    base,
                    f"/hy2/auth/{node_id}/{node_token}",
                    method="POST",
                    payload={"addr": "127.0.0.1:0", "auth": credential, "tx": 0},
                )
                if auth_status != 200 or json.loads(auth_body).get("ok") is not False:
                    raise RuntimeError("disabled-user authentication was not denied before kick")
                kick_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("kind") == "kick" and job.get("node_id") == node_id
                )
                kicked = wait_job(kick_job["id"], timeout=90)
                if (
                    kicked["job"]["status"] != "succeeded"
                    or kicked.get("result", {}).get("result", {}).get("remaining_connections") != 0
                    or online_connections().get(user_id, 0) != 0
                ):
                    raise RuntimeError("disabling the user did not kick both active Hysteria clients")
                for process in traffic_generator_processes:
                    stop_process_group(process)
                traffic_generator_processes.clear()
                subscription_status, _ = request(
                    base, f"/sub/{sub_token}/clash.yaml", method="GET"
                )
                if subscription_status != 403:
                    raise RuntimeError("disabled user subscription did not return HTTP 403")
                for port, output in ((socks_port, temp / "revoked-client-one.bin"), (peer_socks_port, temp / "revoked-client-two.bin")):
                    denied = run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--max-time", "10", "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/payload.bin", "-o", str(output),
                        ],
                        check=False,
                        timeout=12,
                    )
                    if denied.returncode == 0:
                        raise RuntimeError("a disabled user's active client still forwarded traffic")
                for process, log_file in ((client, client_log_file), (peer_client, peer_client_log_file)):
                    if process and process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
                    if log_file and not log_file.closed:
                        log_file.close()
                client = None
                client_log_file = None
                peer_client = None
                peer_client_log_file = None

                current_user = expect(f"/api/v1/users/{user_id}")
                expires_at = datetime.fromtimestamp(time.time() + 45, timezone.utc)
                reenabled_user = expect(
                    f"/api/v1/users/{user_id}",
                    "PATCH",
                    {
                        "expected_revision": current_user["revision"],
                        "enabled": True,
                        "expires_at": expires_at.isoformat(timespec="seconds").replace("+00:00", "Z"),
                    },
                )
                if not reenabled_user["enabled"] or not reenabled_user["expires_at"]:
                    raise RuntimeError("could not reactivate the test user with a future expiry")
                subscription_status, _ = request(
                    base, f"/sub/{sub_token}/clash.yaml", method="GET"
                )
                if subscription_status != 200:
                    raise RuntimeError("the reactivated user's subscription did not become available")

                client_log_file = open(temp / "expiry-client-one.log", "wb")
                client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(client_config)],
                    stdout=client_log_file,
                    stderr=client_log_file,
                )
                peer_client_log_file = open(temp / "expiry-client-two.log", "wb")
                peer_client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(peer_client_config)],
                    stdout=peer_client_log_file,
                    stderr=peer_client_log_file,
                )
                expiry_socks_ready = {socks_port: False, peer_socks_port: False}
                for _ in range(40):
                    if client.poll() is not None or peer_client.poll() is not None:
                        raise RuntimeError("a Hysteria client exited before the expiry probe")
                    for port in expiry_socks_ready:
                        if expiry_socks_ready[port]:
                            continue
                        try:
                            with socket.create_connection(("127.0.0.1", port), timeout=1):
                                expiry_socks_ready[port] = True
                        except OSError:
                            pass
                    if all(expiry_socks_ready.values()):
                        break
                    time.sleep(0.25)
                if not all(expiry_socks_ready.values()):
                    raise RuntimeError("both Hysteria clients did not open for the expiry probe")

                expiry_online = {}
                for _ in range(20):
                    expiry_online = online_connections()
                    if expiry_online.get(user_id, 0) >= 2:
                        break
                    time.sleep(0.5)
                if expiry_online.get(user_id, 0) < 2:
                    raise RuntimeError("expiry probe did not establish two online client instances")
                expiry_traffic_before = user_traffic_bytes()
                for port in (socks_port, peer_socks_port):
                    run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", "/dev/null",
                        ],
                        timeout=10,
                    )
                for port in (socks_port, peer_socks_port):
                    process = subprocess.Popen(
                        ["sh", "-c", traffic_loop, "hysteriax-expiry-loop", str(port)],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        start_new_session=True,
                    )
                    traffic_generator_processes.append(process)
                for _ in range(30):
                    if user_traffic_bytes() > expiry_traffic_before and online_connections().get(user_id, 0) >= 2:
                        break
                    time.sleep(0.2)
                else:
                    raise RuntimeError("continuous traffic did not start for both expiry-probe clients")

                prior_expiry_jobs = {job["id"] for job in expect("/api/v1/jobs")}
                expiry_deadline = expires_at.timestamp() + 35
                expiry_kick_job = None
                while time.time() < expiry_deadline:
                    auth_status, auth_body = request(
                        base,
                        f"/hy2/auth/{node_id}/{node_token}",
                        method="POST",
                        payload={"addr": "127.0.0.1:0", "auth": credential, "tx": 0},
                    )
                    auth_denied = auth_status == 200 and json.loads(auth_body).get("ok") is False
                    if auth_denied:
                        expiry_kick_job = next(
                            (
                                job for job in expect("/api/v1/jobs")
                                if job.get("id") not in prior_expiry_jobs
                                and job.get("kind") == "kick"
                                and job.get("node_id") == node_id
                            ),
                            None,
                        )
                        if expiry_kick_job:
                            break
                    time.sleep(0.5)
                if expiry_kick_job is None:
                    raise RuntimeError("natural user expiry did not deny auth and enqueue a kick")
                expiry_kick = wait_job(expiry_kick_job["id"], timeout=90)
                if (
                    expiry_kick["job"]["status"] != "succeeded"
                    or expiry_kick.get("result", {}).get("result", {}).get("remaining_connections") != 0
                    or online_connections().get(user_id, 0) != 0
                ):
                    raise RuntimeError("natural user expiry did not kick both active Hysteria clients")
                for process in traffic_generator_processes:
                    stop_process_group(process)
                traffic_generator_processes.clear()
                subscription_status, _ = request(
                    base, f"/sub/{sub_token}/clash.yaml", method="GET"
                )
                if subscription_status != 403:
                    raise RuntimeError("an expired user's subscription did not return HTTP 403")
                for port in (socks_port, peer_socks_port):
                    denied = run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--max-time", "10", "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", "/dev/null",
                        ],
                        check=False,
                        timeout=12,
                    )
                    if denied.returncode == 0:
                        raise RuntimeError("an expired user's active client still forwarded proxy traffic")
                for process, log_file in ((client, client_log_file), (peer_client, peer_client_log_file)):
                    if process and process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
                    if log_file and not log_file.closed:
                        log_file.close()
                client = None
                client_log_file = None
                peer_client = None
                peer_client_log_file = None

                quota_user = expect(
                    "/api/v1/users", "POST", {"name": "Active over-quota revocation probe"}, (201,)
                )
                quota_user_id = quota_user["id"]
                quota_credential = expect(
                    f"/api/v1/users/{quota_user_id}/assignments",
                    "POST",
                    {
                        "expected_revision": 1,
                        "node_id": node_id,
                        "client_certificate": client_cert.read_text(),
                        "client_private_key": client_key.read_text(),
                    },
                    (201,),
                )["hy2_credential"]
                secrets_to_scrub.append(quota_credential)
                quota_subscription = expect(
                    f"/api/v1/users/{quota_user_id}/subscription",
                    "POST",
                    {"expected_revision": 2},
                )
                quota_subscription_token = quota_subscription["token"]
                secrets_to_scrub.append(quota_subscription_token)
                quota_client_config = temp / "over-quota-client-one.yaml"
                quota_peer_config = temp / "over-quota-client-two.yaml"
                main_quota_config = client_config.read_text().replace(
                    f"auth: {credential}\n", f"auth: {quota_credential}\n"
                )
                peer_quota_config = peer_client_config.read_text().replace(
                    f"auth: {credential}\n", f"auth: {quota_credential}\n"
                )
                if main_quota_config == client_config.read_text() or peer_quota_config == peer_client_config.read_text():
                    raise RuntimeError("could not prepare client configs for the over-quota user")
                quota_client_config.write_text(main_quota_config)
                quota_peer_config.write_text(peer_quota_config)
                client_log_file = open(temp / "over-quota-client-one.log", "wb")
                client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(quota_client_config)],
                    stdout=client_log_file,
                    stderr=client_log_file,
                )
                peer_client_log_file = open(temp / "over-quota-client-two.log", "wb")
                peer_client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(quota_peer_config)],
                    stdout=peer_client_log_file,
                    stderr=peer_client_log_file,
                )
                for _ in range(40):
                    if client.poll() is not None or peer_client.poll() is not None:
                        raise RuntimeError("Hysteria clients did not start for the over-quota probe")
                    try:
                        with socket.create_connection(("127.0.0.1", socks_port), timeout=1):
                            pass
                        with socket.create_connection(("127.0.0.1", peer_socks_port), timeout=1):
                            pass
                        break
                    except OSError:
                        time.sleep(0.25)
                else:
                    raise RuntimeError("SOCKS5 listeners did not start for the over-quota probe")
                quota_online = {}
                for _ in range(20):
                    quota_online = online_connections()
                    if quota_online.get(quota_user_id, 0) >= 2:
                        break
                    time.sleep(0.25)
                if quota_online.get(quota_user_id, 0) < 2:
                    raise RuntimeError("over-quota probe did not establish two online client instances")
                for port in (socks_port, peer_socks_port):
                    run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", "/dev/null",
                        ],
                        timeout=10,
                    )
                quota_traffic_before = user_traffic_bytes(quota_user_id)
                for port in (socks_port, peer_socks_port):
                    process = subprocess.Popen(
                        ["sh", "-c", traffic_loop, "hysteriax-quota-loop", str(port)],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        start_new_session=True,
                    )
                    traffic_generator_processes.append(process)
                for _ in range(30):
                    if user_traffic_bytes(quota_user_id) > quota_traffic_before and online_connections().get(quota_user_id, 0) >= 2:
                        break
                    time.sleep(0.2)
                else:
                    raise RuntimeError("continuous traffic did not start for both over-quota clients")

                prior_jobs = {job["id"] for job in expect("/api/v1/jobs")}
                quota_user_state = expect(f"/api/v1/users/{quota_user_id}")
                limited_user = expect(
                    f"/api/v1/users/{quota_user_id}",
                    "PATCH",
                    {
                        "expected_revision": quota_user_state["revision"],
                        "quota_bytes": 0,
                    },
                )
                if limited_user["quota_bytes"] != 0:
                    raise RuntimeError("zero-byte quota did not apply to the user")
                auth_status, auth_body = request(
                    base,
                    f"/hy2/auth/{node_id}/{node_token}",
                    method="POST",
                    payload={"addr": "127.0.0.1:0", "auth": quota_credential, "tx": 0},
                )
                if auth_status != 200 or json.loads(auth_body).get("ok") is not False:
                    raise RuntimeError("over-quota authentication was not denied before kick")
                quota_kick_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("id") not in prior_jobs
                    and job.get("kind") == "kick"
                    and job.get("node_id") == node_id
                )
                quota_kick = wait_job(quota_kick_job["id"], timeout=90)
                if (
                    quota_kick["job"]["status"] != "succeeded"
                    or quota_kick.get("result", {}).get("result", {}).get("remaining_connections") != 0
                    or online_connections().get(quota_user_id, 0) != 0
                ):
                    raise RuntimeError("over-quota update did not kick both active Hysteria clients")
                for process in traffic_generator_processes:
                    stop_process_group(process)
                traffic_generator_processes.clear()
                for port in (socks_port, peer_socks_port):
                    denied = run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--max-time", "10", "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", "/dev/null",
                        ],
                        check=False,
                        timeout=12,
                    )
                    if denied.returncode == 0:
                        raise RuntimeError("an over-quota client still forwarded proxy traffic")
                quota_subscription_status, _ = request(
                    base, f"/sub/{quota_subscription_token}/clash.yaml", method="GET"
                )
                if quota_subscription_status != 403:
                    raise RuntimeError("over-quota subscription did not return HTTP 403")
                for process, log_file in ((client, client_log_file), (peer_client, peer_client_log_file)):
                    if process and process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
                    if log_file and not log_file.closed:
                        log_file.close()
                client = None
                client_log_file = None
                peer_client = None
                peer_client_log_file = None

                quota_state = expect(f"/api/v1/users/{quota_user_id}")
                quota_cleared = expect(
                    f"/api/v1/users/{quota_user_id}",
                    "PATCH",
                    {"expected_revision": quota_state["revision"], "quota_bytes": None},
                )
                quota_reset = expect(
                    f"/api/v1/users/{quota_user_id}/quota/reset",
                    "POST",
                    {"expected_revision": quota_cleared["revision"]},
                )
                if quota_reset["usage_bytes"] != 0:
                    raise RuntimeError("quota reset did not start a zero-usage rotation probe period")

                client_log_file = open(temp / "rotation-client-one.log", "wb")
                client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(quota_client_config)],
                    stdout=client_log_file,
                    stderr=client_log_file,
                )
                peer_client_log_file = open(temp / "rotation-client-two.log", "wb")
                peer_client = subprocess.Popen(
                    [str(client_binary), "--disable-update-check", "client", "-c", str(quota_peer_config)],
                    stdout=peer_client_log_file,
                    stderr=peer_client_log_file,
                )
                for _ in range(40):
                    if client.poll() is not None or peer_client.poll() is not None:
                        raise RuntimeError("Hysteria clients did not start for the credential-rotation probe")
                    try:
                        with socket.create_connection(("127.0.0.1", socks_port), timeout=1):
                            pass
                        with socket.create_connection(("127.0.0.1", peer_socks_port), timeout=1):
                            pass
                        break
                    except OSError:
                        time.sleep(0.25)
                else:
                    raise RuntimeError("SOCKS5 listeners did not start for the credential-rotation probe")
                rotation_online = {}
                for _ in range(20):
                    rotation_online = online_connections()
                    if rotation_online.get(quota_user_id, 0) >= 2:
                        break
                    time.sleep(0.25)
                if rotation_online.get(quota_user_id, 0) < 2:
                    raise RuntimeError("credential-rotation probe did not establish two online clients")
                rotation_traffic_before = user_traffic_bytes(quota_user_id)
                for port in (socks_port, peer_socks_port):
                    run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", "/dev/null",
                        ],
                        timeout=10,
                    )
                for port in (socks_port, peer_socks_port):
                    process = subprocess.Popen(
                        ["sh", "-c", traffic_loop, "hysteriax-rotation-loop", str(port)],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        start_new_session=True,
                    )
                    traffic_generator_processes.append(process)
                for _ in range(30):
                    if user_traffic_bytes(quota_user_id) > rotation_traffic_before and online_connections().get(quota_user_id, 0) >= 2:
                        break
                    time.sleep(0.2)
                else:
                    raise RuntimeError("continuous proxy traffic did not start for both rotation clients")

                prior_rotation_jobs = {job["id"] for job in expect("/api/v1/jobs")}
                credentials_revision = expect(f"/api/v1/users/{quota_user_id}")["revision"]
                rotated = expect(
                    f"/api/v1/users/{quota_user_id}/credentials/rotate",
                    "POST",
                    {"expected_revision": credentials_revision},
                )
                replacement_credential = next(
                    item["credential"]
                    for item in rotated["credentials"]
                    if item["node_id"] == node_id
                )
                secrets_to_scrub.append(replacement_credential)
                old_auth_status, old_auth_body = request(
                    base,
                    f"/hy2/auth/{node_id}/{node_token}",
                    method="POST",
                    payload={"addr": "127.0.0.1:0", "auth": quota_credential, "tx": 0},
                )
                new_auth_status, new_auth_body = request(
                    base,
                    f"/hy2/auth/{node_id}/{node_token}",
                    method="POST",
                    payload={"addr": "127.0.0.1:0", "auth": replacement_credential, "tx": 0},
                )
                if (
                    old_auth_status != 200
                    or json.loads(old_auth_body).get("ok") is not False
                    or new_auth_status != 200
                    or json.loads(new_auth_body).get("id") != quota_user_id
                ):
                    raise RuntimeError("credential rotation did not reject the old password and accept the replacement")
                rotation_kick_job = next(
                    job for job in expect("/api/v1/jobs")
                    if job.get("id") not in prior_rotation_jobs
                    and job.get("kind") == "kick"
                    and job.get("node_id") == node_id
                )
                rotation_kick = wait_job(rotation_kick_job["id"], timeout=90)
                if (
                    rotation_kick["job"]["status"] != "succeeded"
                    or rotation_kick.get("result", {}).get("result", {}).get("remaining_connections") != 0
                    or online_connections().get(quota_user_id, 0) != 0
                ):
                    raise RuntimeError("credential rotation did not kick both old Hysteria sessions")
                for process in traffic_generator_processes:
                    stop_process_group(process)
                traffic_generator_processes.clear()
                for port in (socks_port, peer_socks_port):
                    denied = run(
                        [
                            "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                            "--max-time", "10", "--socks5-hostname", f"127.0.0.1:{port}",
                            "http://127.0.0.1:18081/kick.bin", "-o", "/dev/null",
                        ],
                        check=False,
                        timeout=12,
                    )
                    if denied.returncode == 0:
                        raise RuntimeError("a rotated-out client still forwarded proxy traffic")
                rotation_subscription_status, rotation_subscription_body = request(
                    base, f"/sub/{quota_subscription_token}/clash.yaml", method="GET"
                )
                if (
                    rotation_subscription_status != 200
                    or replacement_credential.encode() not in rotation_subscription_body
                    or quota_credential.encode() in rotation_subscription_body
                ):
                    raise RuntimeError("subscription did not publish only the replacement Hysteria credential")
                for process, log_file in ((client, client_log_file), (peer_client, peer_client_log_file)):
                    if process and process.poll() is None:
                        process.terminate()
                        process.wait(timeout=5)
                    if log_file and not log_file.closed:
                        log_file.close()
                client = None
                client_log_file = None
                peer_client = None
                peer_client_log_file = None

                deletion = expect(
                    f"/api/v1/nodes/{node_id}?expected_revision={node_revision}",
                    "DELETE",
                    statuses=(202,),
                )
                uninstalled = wait_job(deletion["job_id"])
                if uninstalled["job"]["status"] != "succeeded":
                    raise RuntimeError("mTLS node remote uninstall failed")
                print(
                    f"Live mTLS+ECH+Gecko+port-hopping passed: Hysteria client and Mihomo v1.19.31 each transferred "
                    f"{payload_path.stat().st_size} TCP bytes across the configured UDP port range."
                )
                print(
                    "Deployment failure probes passed: wrong SSH fingerprint, unmanaged directory, "
                    "and SSH interruption during deployment followed by retry and recovery."
                )
                print(
                    "Forced health-check and automatic rollback restart failure were recorded; "
                    "an explicit sync recovered after removing the injected failure."
                )
                print(
                    "Traffic sampling recovered after SSH interruption: missed traffic was charged once, "
                    f"with a {outage_payload.stat().st_size}-byte transfer during the outage."
                )
                print(
                    "Expiry, disable, over-quota, and credential rotation each kicked two active Hysteria clients, "
                    "returned zero online clients, and denied further proxy traffic and subscription access."
                )
        except Exception as error:
            message = str(error)
            for secret in secrets_to_scrub:
                if secret:
                    message = message.replace(secret, "[redacted]")
            print("MTLS_LIVE_FAILURE:", message)
            for process in traffic_generator_processes:
                stop_process_group(process)
            if client and client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait()
            if peer_client and peer_client.poll() is None:
                peer_client.terminate()
                try:
                    peer_client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    peer_client.kill()
                    peer_client.wait()
            if client_log_file:
                client_log_file.close()
                log_path = temp / "client.log"
                if log_path.exists():
                    client_log = log_path.read_text(errors="replace")[-1500:]
                    for secret in secrets_to_scrub:
                        if secret:
                            client_log = client_log.replace(secret, "[redacted]")
                    if client_log:
                        print("client_log_tail:", client_log)
            if peer_client_log_file:
                peer_client_log_file.close()
                log_path = temp / "revocation-client-two.log"
                if log_path.exists():
                    peer_client_log = log_path.read_text(errors="replace")[-1500:]
                    for secret in secrets_to_scrub:
                        if secret:
                            peer_client_log = peer_client_log.replace(secret, "[redacted]")
                    if peer_client_log:
                        print("peer_client_log_tail:", peer_client_log)
            if mihomo and mihomo.poll() is None:
                mihomo.terminate()
                try:
                    mihomo.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    mihomo.kill()
                    mihomo.wait()
            if mihomo_log_file:
                mihomo_log_file.close()
                log_path = temp / "mihomo.log"
                if log_path.exists():
                    mihomo_log = log_path.read_text(errors="replace")[-1500:]
                    for secret in secrets_to_scrub:
                        if secret:
                            mihomo_log = mihomo_log.replace(secret, "[redacted]")
                    if mihomo_log:
                        print("mihomo_log_tail:", mihomo_log)
            management_log_path = temp / "management.log"
            if management_log_path.exists():
                management_log = management_log_path.read_text(errors="replace")[-2500:]
                for secret in secrets_to_scrub:
                    if secret:
                        management_log = management_log.replace(secret, "[redacted]")
                if management_log:
                    print("management_log_tail:", management_log)
            raise
        finally:
            for process in traffic_generator_processes:
                stop_process_group(process)
            if client and client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait()
            if peer_client and peer_client.poll() is None:
                peer_client.terminate()
                try:
                    peer_client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    peer_client.kill()
                    peer_client.wait()
            if client_log_file and not client_log_file.closed:
                client_log_file.close()
            if peer_client_log_file and not peer_client_log_file.closed:
                peer_client_log_file.close()
            if mihomo and mihomo.poll() is None:
                mihomo.terminate()
                try:
                    mihomo.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    mihomo.kill()
                    mihomo.wait()
            if mihomo_log_file and not mihomo_log_file.closed:
                mihomo_log_file.close()
            if api:
                api.terminate()
                try:
                    api.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    api.kill()
                    api.wait()
            run(["docker", "rm", "-f", container], check=False)
            run(["docker", "image", "rm", image], check=False)


if __name__ == "__main__":
    main()
