#!/usr/bin/env python3
"""Deploy the pinned server across disposable systemd distro/architecture containers."""

import base64
import argparse
import hashlib
import json
import yaml
import os
import pathlib
import platform
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import uuid
import urllib.request

from postgres_test import PostgresTestSchema
from credential_test_fixtures import grant_group
from multi_instance_test_fixture import MultiInstanceServer
from process_fault_test_fixture import run_installation_pause
from live_database_diagnostics import DatabaseDiagnostics


ROOT = pathlib.Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "hysteriax-server"
DOCKERFILE = ROOT / "scripts" / "fixtures" / "systemd-node.Dockerfile"
MATRIX = [
    ("debian", "12", "debian:bookworm"),
    ("debian", "13", "debian:trixie"),
    ("ubuntu", "22.04", "ubuntu:22.04"),
    ("ubuntu", "24.04", "ubuntu:24.04"),
]
ARCHITECTURES = [("amd64", "linux/amd64"), ("arm64", "linux/arm64")]
HYSTERIA_CLIENT_ASSETS = {
    ("Darwin", "arm64"): (
        "hysteria-darwin-arm64",
        "9065dc5dc9cd75f7ba881f481e8cb77e7eae17139460ca09d399682ca6fad443",
    ),
    ("Linux", "x86_64"): (
        "hysteria-linux-amd64",
        "8c7a68a906998b747a0db87586e364f995fbfddb95693ae6e2fdb68a6e920d3e",
    ),
    ("Linux", "aarch64"): (
        "hysteria-linux-arm64",
        "c8dc653c3ba0a28d29a26b8fa52d2086f27c0927afddce95c09965e7174e78b0",
    ),
}


def run(command, check=True, timeout=300):
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {' '.join(map(str, command))}\n"
            + result.stdout[-1200:]
            + result.stderr[-1200:]
        )
    return result


def run_input(command, input_text, timeout=30):
    result = subprocess.run(
        command, input=input_text, capture_output=True, text=True, timeout=timeout
    )
    if result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {' '.join(map(str, command))}\n"
            + result.stderr[-1000:]
        )
    return result


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


def request(base, path, token=None, method="GET", payload=None):
    from credential_test_fixtures import request_with_credentials
    return request_with_credentials(raw_request, base, path, token, method, payload)


def raw_request(base, path, token=None, method="GET", payload=None):
    identity = None
    if method != "GET" and path.startswith("/api/v2/"):
        identity = str(payload.get("idempotency_key")) if isinstance(payload, dict) and payload.get("idempotency_key") else str(uuid.uuid4())
        def clean(value):
            if isinstance(value, dict): return {key: clean(item) for key, item in value.items() if key != "idempotency_key"}
            if isinstance(value, list): return [clean(item) for item in value]
            return value
        payload = clean(payload)
    body = None if payload is None else json.dumps(payload).encode()
    headers = {"Idempotency-Key": identity} if identity else {}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=15) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def free_port(kind=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, kind) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def download_hysteria_client(destination):
    key = (platform.system(), platform.machine())
    try:
        asset, expected_digest = HYSTERIA_CLIENT_ASSETS[key]
    except KeyError as error:
        raise RuntimeError(f"no pinned Hysteria client asset for host {key}") from error
    url = f"https://github.com/apernet/hysteria/releases/download/app/v2.12.3/{asset}"
    with urllib.request.urlopen(url, timeout=90) as response:
        content = response.read()
    if hashlib.sha256(content).hexdigest() != expected_digest:
        raise RuntimeError("pinned Hysteria client digest mismatch")
    destination.write_bytes(content)
    destination.chmod(0o700)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--only",
        action="append",
        help="test one matrix entry, for example debian13-amd64 (may be repeated)",
    )
    parser.add_argument("--instances", type=int, choices=(1, 3), default=1)
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--pause-during-install", action="store_true")
    arguments = parser.parse_args()
    selected = set(arguments.only or [])
    run_id = uuid.uuid4().hex[:10]
    verification = {"passed": False, "instances": arguments.instances}

    available = {
        f"{distribution}{version.replace('.', '')}-{architecture}"
        for distribution, version, _ in MATRIX
        for architecture, _ in ARCHITECTURES
    }
    if unknown := selected - available:
        parser.error(f"unknown matrix entries: {', '.join(sorted(unknown))}")
    if arguments.pause_during_install and (arguments.instances != 3 or len(selected or available) < 2):
        parser.error("--pause-during-install requires three instances and at least two matrix nodes")
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)

    built_images = []
    containers = []
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-deployment-matrix-") as temporary:
        temp = pathlib.Path(temporary)
        ssh_key = temp / "id_ed25519"
        subprocess.run(
            ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(ssh_key)],
            check=True,
        )

        try:
            matrix_nodes = []
            for distribution, version, base_image in MATRIX:
                for architecture, docker_platform in ARCHITECTURES:
                    suffix = f"{distribution}{version.replace('.', '')}-{architecture}"
                    if selected and suffix not in selected:
                        continue
                    image = f"hysteriax-node-matrix:{run_id}-{suffix}"
                    container = f"hysteriax-matrix-{run_id}-{suffix}"
                    ssh_port = free_port()
                    hy2_port = free_port(socket.SOCK_DGRAM)
                    print(f"Building {distribution} {version} {architecture} image", flush=True)
                    run(
                        [
                            "docker",
                            "build",
                            "--platform",
                            docker_platform,
                            "--build-arg",
                            f"BASE_IMAGE={base_image}",
                            "-f",
                            str(DOCKERFILE),
                            "-t",
                            image,
                            str(ROOT),
                        ],
                        timeout=900,
                    )
                    built_images.append(image)
                    container_command = [
                        "docker",
                        "create",
                        "--privileged",
                        "--cgroupns=host",
                        "--tmpfs",
                        "/run",
                        "--tmpfs",
                        "/run/lock",
                        "--name",
                        container,
                        "-p",
                        f"127.0.0.1:{ssh_port}:22",
                        "-p",
                        f"127.0.0.1:{hy2_port}:443/udp",
                        image,
                    ]
                    if platform.system() == "Linux":
                        container_command[3:3] = ["--add-host", "host.docker.internal:host-gateway"]
                    run(container_command)
                    containers.append(container)
                    run(["docker", "start", container])
                    wait_for_systemd(container)
                    run(
                        [
                            "docker",
                            "cp",
                            str(ssh_key.with_suffix(".pub")),
                            f"{container}:/root/.ssh/authorized_keys",
                        ]
                    )
                    run(
                        [
                            "docker",
                            "exec",
                            container,
                            "sh",
                            "-c",
                            "mkdir -p /run/sshd && chown root:root /run/sshd && chmod 0755 /run/sshd && chown -R root:root /root/.ssh && chmod 0700 /root/.ssh "
                            "&& chmod 0600 /root/.ssh/authorized_keys && ssh-keygen -A "
                            "&& sshd -t && systemctl enable --now ssh.service",
                        ]
                    )
                    matrix_nodes.append(
                        {
                            "name": f"{distribution.title()} {version} {architecture}",
                            "distribution": distribution,
                            "version": version,
                            "architecture": architecture,
                            "container": container,
                            "ssh_port": ssh_port,
                            "hy2_port": hy2_port,
                            "image": image,
                        }
                    )
                    print(f"Prepared {distribution} {version} {architecture} systemd container", flush=True)

            with socket.socket() as listener:
                listener.bind(("0.0.0.0", 0))
                api_port = listener.getsockname()[1]
            base = f"http://127.0.0.1:{api_port}"
            public_url = f"http://host.docker.internal:{api_port}"
            admin = "hx_" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
            master_key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
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

            target_server = None
            with open(temp / "server.log", "wb") as log:
                def observe_instances():
                    with database.connect() as connection:
                        return {row[0] for row in connection.execute("SELECT instance_id FROM service_instances")}
                service = MultiInstanceServer(SERVER, environment, temp / "services", instances=arguments.instances, migration_log=log, observe_instances=observe_instances)
                diagnostics = DatabaseDiagnostics(database.url, temp / "services" / "database-diagnostics.jsonl") if arguments.report else None

                def expect(path, method="GET", payload=None, expected_status=(200, 201, 202)):
                    if path == "/api/v2/jobs" and method == "GET":
                        items, page = [], 1
                        while True:
                            result = expect(f"{path}?page={page}&page_size=200")
                            items.extend(result["items"])
                            if result["page"] * result["page_size"] >= result["total"]:
                                return items
                            page += 1
                    status, body = request(base, path, admin, method, payload)
                    if status not in expected_status:
                        raise RuntimeError(f"{method} {path} returned HTTP {status}: {body[:500]!r}")
                    return json.loads(body) if body else None

                def wait_job(job_id, timeout=300):
                    deadline = time.time() + timeout
                    while time.time() < deadline:
                        detail = expect(f"/api/v2/jobs/{job_id}")
                        if detail["job"]["status"] in ("succeeded", "failed", "cancelled", "rolled_back", "waiting_confirmation"):
                            return detail
                        time.sleep(1)
                    raise TimeoutError(f"job {job_id} did not finish")

                def transfer_via_node(node, credential, index, client_binary, target_port, material=None, expect_rejection=False):
                    socks_port = free_port()
                    client_config = temp / f"matrix-client-{index}.yaml"
                    client_config.write_text(
                        f"server: 127.0.0.1:{node['hy2_port']}\n"
                        f"auth: {credential}\n"
                        "tls:\n"
                        "  sni: matrix.example.test\n"
                        "  insecure: true\n"
                        + (f"  clientCertificate: {material[0]}\n  clientKey: {material[1]}\n" if material else "")
                        + "socks5:\n"
                        + f"  listen: 127.0.0.1:{socks_port}\n"
                    )
                    client_config.chmod(0o600)
                    log_file = open(temp / f"matrix-client-{index}.log", "wb")
                    client = subprocess.Popen(
                        [str(client_binary), "--disable-update-check", "client", "-c", str(client_config)],
                        stdout=log_file,
                        stderr=log_file,
                    )
                    try:
                        ready = False
                        for _ in range(30):
                            if client.poll() is not None:
                                if expect_rejection and "certificate" in pathlib.Path(log_file.name).read_text(errors="replace").lower(): return 0
                                raise RuntimeError(f"Hysteria client for {node['name']} exited early")
                            try:
                                with socket.create_connection(("127.0.0.1", socks_port), timeout=1):
                                    pass
                                ready = True
                                break
                            except OSError:
                                time.sleep(0.5)
                        if not ready:
                            if expect_rejection and "certificate" in pathlib.Path(log_file.name).read_text(errors="replace").lower():return 0
                            raise RuntimeError(f"Hysteria client for {node['name']} did not open SOCKS5")
                        output = temp / f"matrix-proxy-{index}.bin"
                        transferred = run(
                            [
                                "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                                "--max-time", "25", "--socks5-hostname", f"127.0.0.1:{socks_port}",
                                f"http://host.docker.internal:{target_port}/payload.bin", "-o", str(output),
                            ],
                            timeout=30, check=not expect_rejection,
                        )
                        if expect_rejection:
                            if transferred.returncode==0 or "certificate" not in pathlib.Path(log_file.name).read_text(errors="replace").lower():
                                raise RuntimeError("old or outside-issuer certificate was not rejected by TLS")
                            return 0
                        if output.stat().st_size == 0:
                            raise RuntimeError(f"Hysteria TCP transfer through {node['name']} was empty")
                        return output.stat().st_size
                    finally:
                        if client.poll() is None:
                            client.terminate()
                            try:
                                client.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                client.kill()
                                client.wait()
                        log_file.close()

                def prepare_node(node):
                    ssh_username = node.get("ssh_username", "root")
                    ssh_auth_type = node.get("ssh_auth_type", "private_key")
                    ssh_secret = node.get("ssh_secret", ssh_key.read_text())
                    created = expect(
                        "/api/v2/nodes",
                        "POST",
                        {
                            "name": node["name"],
                            "ssh_host": "127.0.0.1",
                            "ssh_port": node["ssh_port"],
                            "ssh_username": ssh_username,
                            "ssh_auth_type": ssh_auth_type,
                            "ssh_secret": ssh_secret,
                            "public_host": "127.0.0.1",
                            "public_port": node["hy2_port"],
                            "listen_addr": ":443",
                            "tls_sni": "matrix.example.test",
                            "config": {},
                        },
                        (201,),
                    )
                    node["node_id"] = created["node"]["id"]
                    node["node_token"] = created["node_auth_token"]
                    ssh_job = expect(
                        f"/api/v2/nodes/{node['node_id']}/ssh-test",
                        "POST",
                        {"expected_revision": 1},
                        (202,),
                    )["job_id"]
                    ssh_result = wait_job(ssh_job, timeout=90)
                    fingerprint = ((ssh_result.get("result") or {}).get("result") or {}).get("fingerprint")
                    if ssh_result["job"]["status"] != "waiting_confirmation" or not fingerprint:
                        raise RuntimeError(f"SSH fingerprint discovery failed for {node['name']}")
                    expect(
                        f"/api/v2/jobs/{ssh_job}/confirm-fingerprint",
                        "POST",
                        {"expected_revision": 1, "fingerprint": fingerprint},
                        (202,),
                    )
                    if wait_job(ssh_job)["job"]["status"] != "succeeded":
                        raise RuntimeError("SSH test did not continue successfully on its original job")

                    def upload_resource(name, kind, content):
                        return expect(
                            f"/api/v2/nodes/{node['node_id']}/resources",
                            "POST",
                            {
                                "name": name,
                                "resource_kind": kind,
                                "content_base64": base64.b64encode(content).decode(),
                            },
                            (201,),
                        )["reference"]

                    cert_ref = upload_resource("matrix-cert.pem", "certificate", cert.read_bytes())
                    key_ref = upload_resource("matrix-key.pem", "private_key", private_key.read_bytes())
                    updated = expect(
                        f"/api/v2/nodes/{node['node_id']}",
                        "PATCH",
                        {
                            "expected_revision": 2,
                            "config": {
                                "tls": {
                                    "cert": cert_ref,
                                    "key": key_ref,
                                    "sniGuard": "disable",
                                }
                            },
                        },
                    )
                    node["revision"] = updated["revision"]

                try:
                    ready = False
                    for _ in range(60):
                        if service.poll() is not None:
                            raise RuntimeError("temporary HysteriaX service exited")
                        try:
                            status, _ = request(base, "/readyz")
                            if status == 200:
                                ready = True
                                break
                        except (OSError, urllib.error.URLError):
                            pass
                        time.sleep(0.5)
                    if not ready:
                        raise RuntimeError("temporary HysteriaX service did not become ready")

                    cert = temp / "matrix-cert.pem"
                    private_key = temp / "matrix-key.pem"
                    subprocess.run(
                        [
                            "openssl",
                            "req",
                            "-x509",
                            "-newkey",
                            "rsa:2048",
                            "-nodes",
                            "-keyout",
                            str(private_key),
                            "-out",
                            str(cert),
                            "-days",
                            "2",
                            "-subj",
                            "/CN=matrix.example.test",
                            "-addext",
                            "subjectAltName=DNS:matrix.example.test",
                        ],
                        check=True,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )

                    ssh_auth_nodes = []
                    for matrix_node in matrix_nodes:
                        container = matrix_node["container"]
                        sudo_key_user = "hysteriax-sudo-key"
                        sudo_password_user = "hysteriax-sudo-password"
                        password = secrets.token_urlsafe(32)
                        run(
                            [
                                "docker",
                                "exec",
                                container,
                                "sh",
                                "-c",
                                f"useradd -m -s /bin/bash {sudo_key_user} && usermod -aG sudo {sudo_key_user} "
                                f"&& printf '{sudo_key_user} ALL=(ALL) NOPASSWD:ALL\\n' > /etc/sudoers.d/{sudo_key_user} "
                                f"&& chmod 0440 /etc/sudoers.d/{sudo_key_user} "
                                f"&& mkdir -p /home/{sudo_key_user}/.ssh "
                                f"&& cp /root/.ssh/authorized_keys /home/{sudo_key_user}/.ssh/authorized_keys "
                                f"&& chown -R {sudo_key_user}:{sudo_key_user} /home/{sudo_key_user}/.ssh "
                                f"&& chmod 0700 /home/{sudo_key_user}/.ssh "
                                f"&& chmod 0600 /home/{sudo_key_user}/.ssh/authorized_keys "
                                f"&& useradd -m -s /bin/bash {sudo_password_user} "
                                f"&& usermod -aG sudo {sudo_password_user} "
                                f"&& printf '{sudo_password_user} ALL=(ALL) NOPASSWD:ALL\\n' > /etc/sudoers.d/{sudo_password_user} "
                                f"&& chmod 0440 /etc/sudoers.d/{sudo_password_user} "
                                "&& sed -i 's/^PasswordAuthentication no/PasswordAuthentication yes/' "
                                "/etc/ssh/sshd_config.d/99-hysteriax-test.conf "
                                "&& visudo -c "
                                "&& sshd -t && systemctl reload ssh.service",
                            ]
                        )
                        run_input(
                            ["docker", "exec", "-i", container, "chpasswd"],
                            f"{sudo_password_user}:{password}\n",
                        )
                        ssh_auth_nodes.extend(
                            [
                                {
                                    **matrix_node,
                                    "name": f"{matrix_node['name']} sudo with private key",
                                    "ssh_username": sudo_key_user,
                                    "ssh_auth_type": "private_key",
                                },
                                {
                                    **matrix_node,
                                    "name": f"{matrix_node['name']} sudo with password",
                                    "ssh_username": sudo_password_user,
                                    "ssh_auth_type": "password",
                                    "ssh_secret": password,
                                },
                            ]
                        )

                    if arguments.pause_during_install:
                        verification["installation_process_fault"] = run_installation_pause(database, service, matrix_nodes, prepare_node, expect, wait_job)
                    for node in matrix_nodes:
                        if "node_id" not in node:
                            prepare_node(node)

                    jobs = expect("/api/v2/jobs")
                    for node in matrix_nodes:
                        deployment = next(
                            job
                            for job in jobs
                            if job.get("node_id") == node["node_id"]
                            and job.get("kind") == "sync"
                            and job.get("target_revision") == node["revision"]
                            and (not node.get("accepted_deployment_job") or job["id"] == node["accepted_deployment_job"])
                        )
                        result = wait_job(deployment["id"])
                        if result["job"]["status"] != "succeeded":
                            journal = run(
                                [
                                    "docker",
                                    "exec",
                                    node["container"],
                                    "journalctl",
                                    "-u",
                                    "hysteriax.service",
                                    "--since",
                                    "10 minutes ago",
                                    "--no-pager",
                                ],
                                check=False,
                            )
                            container_log = run(
                                ["docker", "logs", "--tail", "100", node["container"]],
                                check=False,
                            )
                            raise RuntimeError(
                                f"deployment failed for {node['name']}: {result['job'].get('error_message')}\n"
                                + journal.stdout[-1500:]
                                + container_log.stdout[-2500:]
                                + container_log.stderr[-1000:]
                            )
                        observed = result["job"]["result"]["result"].get("architecture")
                        if observed != node["architecture"]:
                            raise RuntimeError(
                                f"{node['name']} reported architecture {observed!r}"
                            )
                        proxy_probe = result["job"]["result"]["result"].get("proxy_probe")
                        if (
                            not proxy_probe
                            or proxy_probe.get("status") != "passed"
                            or proxy_probe.get("route_check") != "tcp_forwarding"
                        ):
                            raise RuntimeError(f"{node['name']} completed without a successful Hysteria TCP forwarding probe")
                        run(
                            ["docker", "exec", node["container"], "systemctl", "is-active", "hysteriax.service"]
                        )
                        print(f"Deployed {node['name']} successfully", flush=True)

                    distinct_nodes = []
                    seen_containers = set()
                    for node in matrix_nodes:
                        if node["container"] not in seen_containers:
                            distinct_nodes.append(node)
                            seen_containers.add(node["container"])
                    if len(distinct_nodes) >= 2:
                        node_one, node_two = distinct_nodes[:2]
                        authority = expect("/api/v2/credentials/ca-identities","POST",{"name":"Shared matrix CA","source":"generated","idempotency_key":"matrix-shared-ca"},(201,))
                        ca_id = authority["id"]
                        def deploy_settings(node,settings):
                            current = expect(f"/api/v2/nodes/{node['node_id']}")
                            options = current["config"]
                            if "clientCA" in options.get("tls",{}):raise RuntimeError("public config contains generated clientCA")
                            options["mtls"] = settings
                            update = expect(f"/api/v2/nodes/{node['node_id']}","PATCH",{"expected_revision":current["revision"],"config":options})
                            node["revision"] = update["revision"]
                            job = next(j for j in expect("/api/v2/jobs") if j.get("node_id")==node["node_id"] and j.get("target_revision")==node["revision"] and j["kind"]=="sync")
                            result = wait_job(job["id"])
                            if result["job"]["status"]!="succeeded":raise RuntimeError("managed matrix deployment failed: "+str(result["job"].get("error_message")))
                        for node in (node_one,node_two):
                            deploy_settings(node,{"mode":"managed","ca_credential_id":ca_id,"ca_credential_version":1})
                        print("Two shared-CA managed nodes deployed with independent probes and no users",flush=True)
                        user_one = expect(
                            "/api/v2/users", "POST", {"name": "Matrix node one only"}, (201,)
                        )["id"]
                        user_two = expect(
                            "/api/v2/users", "POST", {"name": "Matrix node two only"}, (201,)
                        )["id"]
                        shared_user = expect(
                            "/api/v2/users", "POST", {"name": "Matrix shared user"}, (201,)
                        )["id"]

                        def assign_matrix_user(user_id, node, revision):
                            current = expect(f"/api/v2/users/{user_id}")
                            if current["revision"] != revision:
                                raise RuntimeError("matrix authorization business revision changed")
                            receipt = grant_group(expect, user_id, [node["node_id"]], api_prefix="/api/v2")
                            return next(item["hy2_credential"] for item in receipt["created_credentials"] if item["node_id"] == node["node_id"])

                        credential_one = assign_matrix_user(user_one, node_one, 1)
                        credential_two = assign_matrix_user(user_two, node_two, 1)
                        shared_one = assign_matrix_user(shared_user, node_one, 1)
                        shared_two = assign_matrix_user(shared_user, node_two, 2)

                        def auth_matrix_user(node, credential):
                            status, body = request(
                                base,
                                f"/hy2/auth/{node['node_id']}/{node['node_token']}",
                                method="POST",
                                payload={"addr": "127.0.0.1:0", "auth": credential, "tx": 0},
                            )
                            if status != 200:
                                raise RuntimeError("deployed node auth callback returned an unexpected status")
                            return json.loads(body)

                        matrix_auth_checks = [
                            (auth_matrix_user(node_one, credential_one), True, user_one),
                            (auth_matrix_user(node_two, credential_one), False, None),
                            (auth_matrix_user(node_one, credential_two), False, None),
                            (auth_matrix_user(node_two, credential_two), True, user_two),
                            (auth_matrix_user(node_one, shared_one), True, shared_user),
                            (auth_matrix_user(node_two, shared_two), True, shared_user),
                        ]
                        for result, expected_ok, expected_id in matrix_auth_checks:
                            if result.get("ok") is not expected_ok:
                                raise RuntimeError("deployed two-node user isolation check failed")
                            if expected_ok and result.get("id") != expected_id:
                                raise RuntimeError("deployed node returned an unstable user identity")
                        print("Deployed two-node/three-user auth isolation passed", flush=True)

                        subscription = expect(
                            f"/api/v2/users/{shared_user}/subscription",
                            "POST",
                            {"expected_revision": 3},
                        )
                        subscription_token = subscription["token"]
                        status, subscription_yaml = request(
                            base,
                            f"/sub/{subscription_token}?format=mihomo&platform=linux",
                            method="GET",
                        )
                        if status != 200 or subscription_yaml.count(b"type: hysteria2") != 2:
                            raise RuntimeError("shared-user subscription did not contain both deployed nodes")
                        proxies = yaml.safe_load(subscription_yaml)["proxies"]
                        shared_material = (proxies[0]["certificate"],proxies[0]["private-key"])
                        if any((p["certificate"],p["private-key"])!=shared_material for p in proxies):raise RuntimeError("same user/CA did not share identical PEM across nodes")
                        def save_material(material,label):
                            cert_path,key_path=temp/(label+".pem"),temp/(label+"-key.pem")
                            cert_path.write_text(material[0]);key_path.write_text(material[1]);cert_path.chmod(0o600);key_path.chmod(0o600)
                            return cert_path,key_path
                        shared_files = save_material(shared_material,"shared-generation-1")
                        user_one_sub = expect(f"/api/v2/users/{user_one}/subscription","POST",{"expected_revision":expect(f"/api/v2/users/{user_one}")["revision"]})["token"]
                        status,one_yaml = request(base,f"/sub/{user_one_sub}?format=mihomo&platform=linux",method="GET")
                        one_proxy = yaml.safe_load(one_yaml)["proxies"][0]
                        if one_proxy["certificate"]==shared_material[0] or one_proxy["private-key"]==shared_material[1]:raise RuntimeError("different matrix users share client material")
                        subscription_path = temp / "matrix-shared-subscription.yaml"
                        subscription_path.write_bytes(subscription_yaml)
                        run(
                            [str(ROOT / "scripts/verify-mihomo-config.sh"), str(subscription_path)],
                            timeout=180,
                        )

                        target_directory = temp / "matrix-proxy-target"
                        target_directory.mkdir()
                        (target_directory / "payload.bin").write_bytes(b"multi-node quota payload " * 2048)
                        target_port = free_port()
                        target_server = subprocess.Popen(
                            [
                                sys.executable,
                                "-m",
                                "http.server",
                                str(target_port),
                                "--bind",
                                "0.0.0.0",
                                "--directory",
                                str(target_directory),
                            ],
                            stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                        )
                        target_ready = False
                        for _ in range(30):
                            try:
                                with urllib.request.urlopen(
                                    f"http://127.0.0.1:{target_port}/payload.bin", timeout=2
                                ) as response:
                                    if response.status == 200:
                                        target_ready = True
                                        break
                            except (OSError, urllib.error.URLError):
                                time.sleep(0.2)
                        if not target_ready:
                            raise RuntimeError("multi-node proxy target did not become ready")

                        client_binary = temp / "matrix-hysteria-client"
                        download_hysteria_client(client_binary)
                        first_bytes = transfer_via_node(node_one, shared_one, 1, client_binary, target_port, shared_files)
                        second_bytes = transfer_via_node(node_two, shared_two, 2, client_binary, target_port, shared_files)
                        usage = None
                        expected_node_ids = {node_one["node_id"], node_two["node_id"]}
                        for _ in range(35):
                            usage = expect(f"/api/v2/users/{shared_user}/usage")
                            sampled_nodes = {entry["node_id"] for entry in usage.get("by_node", [])}
                            if usage.get("usage_bytes", 0) > 0 and expected_node_ids.issubset(sampled_nodes):
                                break
                            time.sleep(1)
                        sampled_nodes = {entry["node_id"] for entry in (usage or {}).get("by_node", [])}
                        if not usage or not expected_node_ids.issubset(sampled_nodes):
                            raise RuntimeError("shared-user traffic was not collected from both deployed nodes")

                        shared_summary = expect(f"/api/v2/users/{shared_user}")
                        expect(
                            f"/api/v2/users/{shared_user}",
                            "PATCH",
                            {
                                "expected_revision": shared_summary["revision"],
                                "quota_bytes": usage["usage_bytes"],
                            },
                        )
                        if auth_matrix_user(node_one, shared_one).get("ok") is not False:
                            raise RuntimeError("over-quota shared user was accepted on node one")
                        if auth_matrix_user(node_two, shared_two).get("ok") is not False:
                            raise RuntimeError("over-quota shared user was accepted on node two")
                        sub_status, _ = request(
                            base,
                            f"/sub/{subscription_token}?format=mihomo&platform=linux",
                            method="GET",
                        )
                        if sub_status != 403:
                            raise RuntimeError("over-quota shared subscription did not return HTTP 403")

                        for node in (node_one, node_two):
                            kick = next(
                                job
                                for job in expect("/api/v2/jobs")
                                if job.get("kind") == "kick" and job.get("node_id") == node["node_id"]
                            )
                            kick_result = wait_job(kick["id"], timeout=90)
                            if kick_result["job"]["status"] != "succeeded":
                                raise RuntimeError("over-quota device kick failed on a deployed node")
                        print(
                            f"Two-node proxy sampling and quota enforcement passed ({first_bytes} + {second_bytes} bytes).",
                            flush=True,
                        )

                        # Restore account access before exercising issuer maintenance.
                        current_user=expect(f"/api/v2/users/{shared_user}")
                        expect(f"/api/v2/users/{shared_user}","PATCH",{"expected_revision":current_user["revision"],"quota_bytes":None})
                        revisions={u:expect(f"/api/v2/users/{u}")["revision"] for u in (user_one,user_two,shared_user)}
                        def shared_proxies():
                            status,body=request(base,f"/sub/{subscription_token}?format=mihomo&platform=linux",method="GET")
                            if status!=200:raise RuntimeError("matrix managed subscription unavailable")
                            return yaml.safe_load(body)["proxies"]
                        def wait_rotation(receipt):
                            previous=None; deadline=time.monotonic()+360
                            while time.monotonic()<deadline:
                                batch=expect(f"/api/v2/credential-batches/{receipt['batch_id']}")
                                rotation=batch["ca_rotation"]
                                if rotation["phase"]!=previous:
                                    print(f"Matrix CA v{receipt['version']}: {rotation['phase']}",flush=True);previous=rotation["phase"]
                                if rotation["phase"]=="complete":return rotation
                                if any(n.get("status") in ("failed","rolled_back","cancelled") for n in rotation["nodes"]):
                                    raise RuntimeError("matrix CA node task failed: "+json.dumps(rotation))
                                time.sleep(1)
                            raise RuntimeError("matrix CA rotation timed out: "+json.dumps(rotation))
                        run(["docker","exec",node_two["container"],"systemctl","stop","ssh.service"])
                        try:
                            receipt=expect(f"/api/v2/credentials/{ca_id}/ca-versions","POST",{"expected_revision":1,"idempotency_key":"matrix-ca-two"},(202,))
                            deadline=time.monotonic()+240
                            while time.monotonic()<deadline:
                                rotation=expect(f"/api/v2/credential-batches/{receipt['batch_id']}")["ca_rotation"]
                                first=next((n for n in rotation["nodes"] if n["node_id"]==node_one["node_id"]),None)
                                second=next((n for n in rotation["nodes"] if n["node_id"]==node_two["node_id"]),None)
                                if first is None or second is None:
                                    time.sleep(1)
                                    continue
                                if first["trust_prepared"] and second.get("status")=="failed":break
                                time.sleep(1)
                            else:raise RuntimeError("offline-node trust barrier did not become observable")
                            first_job=first["job_id"]
                            if expect(f"/api/v2/credentials/{ca_id}")["ca"]["published_version"]!=1:raise RuntimeError("offline node was ignored by CA publication")
                            if any((p["certificate"],p["private-key"])!=shared_material for p in shared_proxies()):raise RuntimeError("unprepared generation escaped into subscriptions")
                            transfer_via_node(node_one,shared_one,"partial-trust",client_binary,target_port,shared_files)
                            print("Offline node blocked shared publication; prepared peer retained old-client traffic",flush=True)
                        finally:run(["docker","exec",node_two["container"],"systemctl","start","ssh.service"])
                        expect(f"/api/v2/credential-batches/{receipt['batch_id']}/retry","POST",{})
                        rotation=wait_rotation(receipt)
                        if next(n for n in rotation["nodes"] if n["node_id"]==node_one["node_id"])["job_id"]!=first_job:raise RuntimeError("retry redeployed an already prepared peer")
                        receipt=expect(f"/api/v2/credentials/{ca_id}/ca-versions","POST",{"expected_revision":2,"idempotency_key":"matrix-ca-three"},(202,))
                        wait_rotation(receipt)
                        latest=shared_proxies(); current_material=(latest[0]["certificate"],latest[0]["private-key"])
                        if any((p["certificate"],p["private-key"])!=current_material for p in latest):raise RuntimeError("shared publication diverged across nodes")
                        current_files=save_material(current_material,"shared-generation-3")
                        for index,(node,password) in enumerate(((node_one,shared_one),(node_two,shared_two))):
                            transfer_via_node(node,password,f"generation-3-{index}",client_binary,target_port,current_files)
                            transfer_via_node(node,password,f"history-1-{index}",client_binary,target_port,shared_files)
                        if any(expect(f"/api/v2/users/{u}")["revision"]!=revision for u,revision in revisions.items()):raise RuntimeError("CA maintenance changed business user revision")
                        # Explicit database time fault: exercise real cleanup deployment,
                        # while generation 2's still-valid history must remain trusted.
                        with database.connect() as connection:
                            connection.execute("UPDATE mtls_user_versions SET not_before=now()-interval '2 days',expires_at=now()-interval '6 minutes' WHERE ca_id=%s AND ca_version=1",(ca_id,))
                            connection.execute("UPDATE mtls_probe_versions SET expires_at=now()-interval '6 minutes' WHERE ca_id=%s AND ca_version=1",(ca_id,))
                            connection.execute("UPDATE mtls_trust_cleanup SET next_action_at=now() WHERE ca_id=%s AND ca_version=1",(ca_id,))
                        deadline=time.monotonic()+360
                        while time.monotonic()<deadline:
                            cleanup=next((c for c in expect(f"/api/v2/credentials/{ca_id}")["ca"]["cleanup"] if c["version"]==1),None)
                            if cleanup is None:
                                time.sleep(1)
                                continue
                            if cleanup["completed_at"] is not None:break
                            if any(n.get("status")=="failed" for n in cleanup["nodes"]):raise RuntimeError("matrix old trust cleanup failed: "+json.dumps(cleanup))
                            time.sleep(1)
                        else:raise RuntimeError("matrix old trust cleanup timed out")
                        with database.connect() as connection:
                            for node in (node_one,node_two):
                                versions=[r[0] for r in connection.execute("SELECT ca_version FROM mtls_node_trust WHERE node_id=%s AND ca_id=%s AND deployed ORDER BY ca_version",(node["node_id"],ca_id))]
                                if versions!=[2,3]:raise RuntimeError("cleanup overwrote newer trust")
                        transfer_via_node(node_one,shared_one,"cleaned-current",client_binary,target_port,current_files)
                        transfer_via_node(node_one,shared_one,"cleaned-old-rejected",client_binary,target_port,shared_files,True)
                        # Select an intermediate CA as a different identity. Its root
                        # proves import validity but must not become a node trust anchor.
                        root_key,root_cert=temp/"parent-key.pem",temp/"parent.pem"
                        int_key,int_csr,int_cert=temp/"issuer-key.pem",temp/"issuer.csr",temp/"issuer.pem"
                        run(["openssl","req","-x509","-newkey","ec","-pkeyopt","ec_paramgen_curve:P-256","-nodes","-keyout",str(root_key),"-out",str(root_cert),"-days","3650","-subj","/CN=Matrix parent CA","-addext","basicConstraints=critical,CA:TRUE,pathlen:1","-addext","keyUsage=critical,keyCertSign,cRLSign"])
                        run(["openssl","req","-newkey","ec","-pkeyopt","ec_paramgen_curve:P-256","-nodes","-keyout",str(int_key),"-out",str(int_csr),"-subj","/CN=Matrix selected issuer"])
                        extensions=temp/"issuer.ext";extensions.write_text("basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n")
                        run(["openssl","x509","-req","-in",str(int_csr),"-CA",str(root_cert),"-CAkey",str(root_key),"-CAcreateserial","-out",str(int_cert),"-days","365","-extfile",str(extensions)])
                        other=expect("/api/v2/credentials/ca-identities","POST",{"name":"Selected intermediate matrix CA","source":"imported","certificate":int_cert.read_text()+root_cert.read_text(),"private_key":int_key.read_text(),"idempotency_key":"matrix-intermediate"},(201,))
                        deploy_settings(node_one,{"mode":"managed","ca_credential_id":other["id"],"ca_credential_version":1})
                        switched=shared_proxies(); switched_proxy=next(p for p in switched if p["port"]==node_one["hy2_port"])
                        switched_material=(switched_proxy["certificate"],switched_proxy["private-key"])
                        if switched_material==current_material:raise RuntimeError("different CA identities share client material")
                        switched_files=save_material(switched_material,"selected-intermediate")
                        transfer_via_node(node_one,shared_one,"intermediate-current",client_binary,target_port,switched_files)
                        transfer_via_node(node_one,shared_one,"old-domain-rejected",client_binary,target_port,current_files,True)
                        remote_config=yaml.safe_load(run(["docker","exec",node_one["container"],"cat","/etc/hysteriax/config.yaml"]).stdout)
                        anchors=run(["docker","exec",node_one["container"],"cat",remote_config["tls"]["clientCA"]]).stdout
                        if anchors.strip()!=int_cert.read_text().strip() or root_cert.read_text().strip() in anchors:raise RuntimeError("ancestor root was added to selected issuer trust")
                        outsider_key,outsider_csr,outsider_cert=temp/"outsider-key.pem",temp/"outsider.csr",temp/"outsider.pem"
                        run(["openssl","req","-newkey","ec","-pkeyopt","ec_paramgen_curve:P-256","-nodes","-keyout",str(outsider_key),"-out",str(outsider_csr),"-subj","/CN=Root-signed outside issuer"])
                        extensions.write_text("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n")
                        run(["openssl","x509","-req","-in",str(outsider_csr),"-CA",str(root_cert),"-CAkey",str(root_key),"-CAcreateserial","-out",str(outsider_cert),"-days","7","-extfile",str(extensions)])
                        transfer_via_node(node_one,shared_one,"ancestor-outside-rejected",client_binary,target_port,(outsider_cert,outsider_key),True)
                        print("Shared CA, offline barrier/retry, consecutive publication, real cleanup with metadata time fault, separate CA and selected-intermediate trust boundary passed",flush=True)

                    for node in matrix_nodes:
                        node["revision"] = expect(f"/api/v2/nodes/{node['node_id']}")["revision"]
                        deletion = expect(
                            f"/api/v2/nodes/{node['node_id']}?expected_revision={node['revision']}",
                            "DELETE",
                            expected_status=(202,),
                        )
                        result = wait_job(deletion["job_id"])
                        if result["job"]["status"] != "succeeded":
                            raise RuntimeError(f"remote uninstall failed for {node['name']}")

                    for node in ssh_auth_nodes:
                        prepare_node(node)
                        queued_jobs = expect("/api/v2/jobs")
                        deployment = next(
                            job
                            for job in queued_jobs
                            if job.get("node_id") == node["node_id"]
                            and job.get("kind") == "sync"
                            and job.get("target_revision") == node["revision"]
                        )
                        result = wait_job(deployment["id"])
                        if result["job"]["status"] != "succeeded":
                            raise RuntimeError(
                                f"deployment failed for {node['name']}: {result['job'].get('error_message')}"
                            )
                        proxy_probe = result["job"]["result"]["result"].get("proxy_probe")
                        if (
                            not proxy_probe
                            or proxy_probe.get("status") != "passed"
                            or proxy_probe.get("route_check") != "tcp_forwarding"
                        ):
                            raise RuntimeError(f"{node['name']} completed without a successful Hysteria TCP forwarding probe")
                        run(
                            ["docker", "exec", node["container"], "systemctl", "is-active", "hysteriax.service"]
                        )
                        deletion = expect(
                            f"/api/v2/nodes/{node['node_id']}?expected_revision={node['revision']}",
                            "DELETE",
                            expected_status=(202,),
                        )
                        result = wait_job(deletion["job_id"])
                        if result["job"]["status"] != "succeeded":
                            raise RuntimeError(f"remote uninstall failed for {node['name']}")
                        print(f"Deployed and uninstalled {node['name']} successfully", flush=True)

                    verification.update(**service.report(), matrix_nodes=len(matrix_nodes), extra_ssh_nodes=len(ssh_auth_nodes))
                    if arguments.instances == 3 and len(verification["round_robin_hits"]) != 3:
                        raise RuntimeError("not all three service processes received live business requests")
                    verification["passed"] = True
                    print(
                        f"All {len(matrix_nodes)} selected distro/architecture deployments and "
                        f"{len(ssh_auth_nodes)} extra SSH-auth deployments passed.",
                        flush=True,
                    )
                finally:
                    if diagnostics:
                        verification["database_diagnostics"] = diagnostics.stop()
                    if target_server and target_server.poll() is None:
                        target_server.terminate()
                        try:
                            target_server.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            target_server.kill()
                            target_server.wait()
                    service.terminate()
                    try:
                        service.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        service.kill()
                        service.wait()
        finally:
            if arguments.report:
                arguments.report.parent.mkdir(parents=True, exist_ok=True)
                saved = pathlib.Path(tempfile.mkdtemp(prefix=arguments.report.stem+"-logs-", dir=arguments.report.parent))
                verification["logs"] = str(saved)
                for name in ("services", "server.log"):
                    source = temp / name
                    if source.is_dir(): shutil.copytree(source, saved / name)
                    elif source.is_file(): shutil.copy2(source, saved / name)
                try:
                    with database.connect() as connection:
                        verification["registered_instances"] = connection.execute("SELECT count(*) FROM service_instances").fetchone()[0]
                        verification["operation_states"] = dict(connection.execute("SELECT state,count(*) FROM execution_operations GROUP BY state").fetchall())
                        verification["execution_owners"] = [row[0] for row in connection.execute("SELECT DISTINCT owner FROM execution_operations ORDER BY owner").fetchall()]
                        verification["jobs_by_status"] = dict(connection.execute("SELECT status,count(*) FROM jobs GROUP BY status").fetchall())
                        verification["blocked_resources"] = connection.execute("SELECT count(*) FROM resource_leases WHERE blocked").fetchone()[0]
                except Exception:
                    verification["database_snapshot_unavailable"] = True
            for container in containers:
                run(["docker", "rm", "-f", container], check=False)
            for image in built_images:
                run(["docker", "image", "rm", image], check=False)
            if arguments.report:
                arguments.report.parent.mkdir(parents=True, exist_ok=True)
                arguments.report.write_text(json.dumps(verification, indent=2)+"\n")


if __name__ == "__main__":
    main()
