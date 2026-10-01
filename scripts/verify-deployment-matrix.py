#!/usr/bin/env python3
"""Deploy the pinned server across disposable systemd distro/architecture containers."""

import base64
import argparse
import hashlib
import json
import os
import pathlib
import platform
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


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


def request(base, path, token=None, method="GET", payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    headers = {}
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
    selected = set(parser.parse_args().only or [])
    available = {
        f"{distribution}{version.replace('.', '')}-{architecture}"
        for distribution, version, _ in MATRIX
        for architecture, _ in ARCHITECTURES
    }
    if unknown := selected - available:
        parser.error(f"unknown matrix entries: {', '.join(sorted(unknown))}")
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)

    built_images = []
    containers = []
    with tempfile.TemporaryDirectory(prefix="hysteriax-deployment-matrix-") as temporary:
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
                    image = f"hysteriax-node-matrix:{suffix}"
                    container = f"hysteriax-matrix-{suffix}"
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
                    containers.append(container)
                    container_command = [
                        "docker",
                        "run",
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
                        "-d",
                        image,
                    ]
                    if platform.system() == "Linux":
                        container_command[3:3] = ["--add-host", "host.docker.internal:host-gateway"]
                    run(container_command)
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
                            "mkdir -p /run/sshd && chown -R root:root /root/.ssh && chmod 0700 /root/.ssh "
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
                    "DATABASE_URL": f"sqlite://{temp / 'service.db'}?mode=rwc",
                    "HYSTERIAX_LISTEN_ADDR": f"0.0.0.0:{api_port}",
                    "HYSTERIAX_PUBLIC_URL": public_url,
                    "HYSTERIAX_ADMIN_TOKEN": admin,
                    "HYSTERIAX_MASTER_KEY": master_key,
                    "RUST_LOG": "warn",
                }
            )

            target_server = None
            with open(temp / "server.log", "wb") as log:
                service = subprocess.Popen(
                    [str(SERVER)], cwd=ROOT, env=environment, stdout=log, stderr=log
                )

                def expect(path, method="GET", payload=None, expected_status=(200, 201, 202)):
                    status, body = request(base, path, admin, method, payload)
                    if status not in expected_status:
                        raise RuntimeError(f"{method} {path} returned HTTP {status}: {body[:500]!r}")
                    return json.loads(body) if body else None

                def wait_job(job_id, timeout=300):
                    deadline = time.time() + timeout
                    while time.time() < deadline:
                        detail = expect(f"/api/v1/jobs/{job_id}")
                        if detail["job"]["status"] in ("succeeded", "failed", "cancelled", "rolled_back"):
                            return detail
                        time.sleep(1)
                    raise TimeoutError(f"job {job_id} did not finish")

                def transfer_via_node(node, credential, index, client_binary, target_port):
                    socks_port = free_port()
                    client_config = temp / f"matrix-client-{index}.yaml"
                    client_config.write_text(
                        f"server: 127.0.0.1:{node['hy2_port']}\n"
                        f"auth: {credential}\n"
                        "tls:\n"
                        "  sni: matrix.example.test\n"
                        "  insecure: true\n"
                        "socks5:\n"
                        f"  listen: 127.0.0.1:{socks_port}\n"
                    )
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
                                raise RuntimeError(f"Hysteria client for {node['name']} exited early")
                            try:
                                with socket.create_connection(("127.0.0.1", socks_port), timeout=1):
                                    pass
                                ready = True
                                break
                            except OSError:
                                time.sleep(0.5)
                        if not ready:
                            raise RuntimeError(f"Hysteria client for {node['name']} did not open SOCKS5")
                        output = temp / f"matrix-proxy-{index}.bin"
                        run(
                            [
                                "curl", "--noproxy", "", "--fail", "--silent", "--show-error",
                                "--max-time", "25", "--socks5-hostname", f"127.0.0.1:{socks_port}",
                                f"http://host.docker.internal:{target_port}/payload.bin", "-o", str(output),
                            ],
                            timeout=30,
                        )
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
                        "/api/v1/nodes",
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
                        f"/api/v1/nodes/{node['node_id']}/ssh-test",
                        "POST",
                        {"expected_revision": 1},
                        (202,),
                    )["job_id"]
                    ssh_result = wait_job(ssh_job, timeout=90)
                    fingerprint = ((ssh_result.get("result") or {}).get("result") or {}).get("fingerprint")
                    if ssh_result["job"]["status"] != "succeeded" or not fingerprint:
                        raise RuntimeError(f"SSH fingerprint discovery failed for {node['name']}")
                    expect(
                        f"/api/v1/nodes/{node['node_id']}",
                        "PATCH",
                        {"expected_revision": 1, "ssh_host_fingerprint": fingerprint},
                    )

                    def upload_resource(name, kind, content):
                        return expect(
                            f"/api/v1/nodes/{node['node_id']}/resources",
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
                        f"/api/v1/nodes/{node['node_id']}",
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

                    for node in matrix_nodes:
                        prepare_node(node)

                    jobs = expect("/api/v1/jobs")
                    for node in matrix_nodes:
                        deployment = next(
                            job
                            for job in jobs
                            if job.get("node_id") == node["node_id"]
                            and job.get("kind") == "sync"
                            and job.get("target_revision") == node["revision"]
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
                        user_one = expect(
                            "/api/v1/users", "POST", {"name": "Matrix node one only"}, (201,)
                        )["id"]
                        user_two = expect(
                            "/api/v1/users", "POST", {"name": "Matrix node two only"}, (201,)
                        )["id"]
                        shared_user = expect(
                            "/api/v1/users", "POST", {"name": "Matrix shared user"}, (201,)
                        )["id"]

                        def assign_matrix_user(user_id, node, revision):
                            return expect(
                                f"/api/v1/users/{user_id}/assignments",
                                "POST",
                                {"expected_revision": revision, "node_id": node["node_id"]},
                                (201,),
                            )["hy2_credential"]

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
                            f"/api/v1/users/{shared_user}/subscription",
                            "POST",
                            {"expected_revision": 3},
                        )
                        subscription_token = subscription["token"]
                        status, subscription_yaml = request(
                            base,
                            f"/sub/{subscription_token}/clash.yaml",
                            method="GET",
                        )
                        if status != 200 or subscription_yaml.count(b"type: hysteria2") != 2:
                            raise RuntimeError("shared-user subscription did not contain both deployed nodes")
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
                        first_bytes = transfer_via_node(node_one, shared_one, 1, client_binary, target_port)
                        second_bytes = transfer_via_node(node_two, shared_two, 2, client_binary, target_port)
                        usage = None
                        expected_node_ids = {node_one["node_id"], node_two["node_id"]}
                        for _ in range(35):
                            usage = expect(f"/api/v1/users/{shared_user}/usage")
                            sampled_nodes = {entry["node_id"] for entry in usage.get("by_node", [])}
                            if usage.get("usage_bytes", 0) > 0 and expected_node_ids.issubset(sampled_nodes):
                                break
                            time.sleep(1)
                        sampled_nodes = {entry["node_id"] for entry in (usage or {}).get("by_node", [])}
                        if not usage or not expected_node_ids.issubset(sampled_nodes):
                            raise RuntimeError("shared-user traffic was not collected from both deployed nodes")

                        shared_summary = expect(f"/api/v1/users/{shared_user}")
                        expect(
                            f"/api/v1/users/{shared_user}",
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
                            f"/sub/{subscription_token}/clash.yaml",
                            method="GET",
                        )
                        if sub_status != 403:
                            raise RuntimeError("over-quota shared subscription did not return HTTP 403")

                        for node in (node_one, node_two):
                            kick = next(
                                job
                                for job in expect("/api/v1/jobs")
                                if job.get("kind") == "kick" and job.get("node_id") == node["node_id"]
                            )
                            kick_result = wait_job(kick["id"], timeout=90)
                            if kick_result["job"]["status"] != "succeeded":
                                raise RuntimeError("over-quota device kick failed on a deployed node")
                        print(
                            f"Two-node proxy sampling and quota enforcement passed ({first_bytes} + {second_bytes} bytes).",
                            flush=True,
                        )

                    for node in matrix_nodes:
                        deletion = expect(
                            f"/api/v1/nodes/{node['node_id']}?expected_revision={node['revision']}",
                            "DELETE",
                            expected_status=(202,),
                        )
                        result = wait_job(deletion["job_id"])
                        if result["job"]["status"] != "succeeded":
                            raise RuntimeError(f"remote uninstall failed for {node['name']}")

                    for node in ssh_auth_nodes:
                        prepare_node(node)
                        queued_jobs = expect("/api/v1/jobs")
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
                            f"/api/v1/nodes/{node['node_id']}?expected_revision={node['revision']}",
                            "DELETE",
                            expected_status=(202,),
                        )
                        result = wait_job(deletion["job_id"])
                        if result["job"]["status"] != "succeeded":
                            raise RuntimeError(f"remote uninstall failed for {node['name']}")
                        print(f"Deployed and uninstalled {node['name']} successfully", flush=True)

                    print(
                        f"All {len(matrix_nodes)} selected distro/architecture deployments and "
                        f"{len(ssh_auth_nodes)} extra SSH-auth deployments passed.",
                        flush=True,
                    )
                finally:
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
            for container in containers:
                run(["docker", "rm", "-f", container], check=False)
            for image in built_images:
                run(["docker", "image", "rm", image], check=False)


if __name__ == "__main__":
    main()
