#!/usr/bin/env python3
"""Verify Mihomo v1.19.31 Realm connections with a delayed STUN response."""

import gzip
from functools import partial
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import pathlib
import platform
import secrets
import shutil
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.request


HYSTERIA_ASSETS = {
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
MIHOMO_ASSETS = {
    ("Darwin", "arm64"): (
        "mihomo-darwin-arm64-v1.19.31.gz",
        "d131f44b3deb2a8356f7ac75048ad67a10d53243323951c4f3cda7b672922963",
    ),
    ("Linux", "x86_64"): (
        "mihomo-linux-amd64-compatible-v1.19.31.gz",
        "04cf9f09671704f839ddbee2e93069dc831a4123a75281e725d1d96ab9ac1afc",
    ),
    ("Linux", "aarch64"): (
        "mihomo-linux-arm64-v1.19.31.gz",
        "9e0f11afbf38426b8bd88fdc594678f8161c57eccb4e1b77acb12b493904f1d4",
    ),
}
STUN_COOKIE = 0x2112A442
STUN_DELAY_SECONDS = 5.6
PAYLOAD_SIZE = 2 * 1024 * 1024


def download(url, digest, destination):
    with urllib.request.urlopen(url, timeout=90) as response:
        contents = response.read()
    if hashlib.sha256(contents).hexdigest() != digest:
        raise RuntimeError(f"pinned release asset digest mismatch: {url.rsplit('/', 1)[-1]}")
    destination.write_bytes(contents)
    destination.chmod(0o700)


def free_port(kind=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, kind) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def encode_stun_response(request, address):
    if len(request) < 20 or request[:2] != b"\x00\x01":
        return None
    if request[4:8] != struct.pack("!I", STUN_COOKIE):
        return None
    transaction_id = request[8:20]
    host = address[0]
    port = address[1]
    try:
        packed_address = socket.inet_pton(socket.AF_INET, host)
        family = 0x01
        xor_mask = struct.pack("!I", STUN_COOKIE)
    except OSError:
        packed_address = socket.inet_pton(socket.AF_INET6, host)
        family = 0x02
        xor_mask = struct.pack("!I", STUN_COOKIE) + transaction_id

    xor_address = bytes(left ^ right for left, right in zip(packed_address, xor_mask))
    mapped = b"\x00" + bytes([family]) + struct.pack("!H", port ^ (STUN_COOKIE >> 16)) + xor_address
    attribute = struct.pack("!HH", 0x0020, len(mapped)) + mapped
    header = struct.pack("!HHI12s", 0x0101, len(attribute), STUN_COOKIE, transaction_id)
    return header + attribute


class StunResponder:
    def __init__(self, port, delay_seconds):
        self.port = port
        self.delay_seconds = delay_seconds
        self.sockets = []
        self.threads = []
        self.stopping = threading.Event()
        for family, host in ((socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")):
            sock = None
            try:
                sock = socket.socket(family, socket.SOCK_DGRAM)
                if family == socket.AF_INET6:
                    sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
                sock.bind((host, port))
                sock.settimeout(0.2)
                self.sockets.append(sock)
            except OSError:
                if sock is not None:
                    sock.close()
        if not self.sockets:
            raise RuntimeError(f"could not bind local STUN port {port}")

    def start(self):
        for sock in self.sockets:
            thread = threading.Thread(target=self._serve, args=(sock,), daemon=True)
            thread.start()
            self.threads.append(thread)

    def _serve(self, sock):
        while not self.stopping.is_set():
            try:
                request, address = sock.recvfrom(2048)
            except socket.timeout:
                continue
            except OSError:
                return
            response = encode_stun_response(request, address)
            if response is not None:
                threading.Thread(
                    target=self._reply,
                    args=(sock, address, response),
                    daemon=True,
                ).start()

    def _reply(self, sock, address, response):
        if self.delay_seconds:
            time.sleep(self.delay_seconds)
        if not self.stopping.is_set():
            try:
                sock.sendto(response, address)
            except OSError:
                pass

    def close(self):
        self.stopping.set()
        for sock in self.sockets:
            sock.close()
        for thread in self.threads:
            thread.join(timeout=1)


def stop_process(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def redacted_log_tail(path, secrets_to_redact):
    text = pathlib.Path(path).read_text(errors="replace")[-1600:]
    for secret in secrets_to_redact:
        if secret:
            text = text.replace(secret, "[redacted]")
    return text


def wait_for_tcp(port, process, label, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{label} exited before opening its TCP port")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"{label} did not open TCP port {port}")


def client_config(mixed_port, rendezvous_port, token, realm_id, stun_port, handshake_timeout):
    proxy = {
        "name": "realm-test",
        "type": "hysteria2",
        "server": "127.0.0.1",
        "port": 443,
        "password": "realm-hy2-test-password",
        "sni": "127.0.0.1",
        "skip-cert-verify": True,
        "ip-version": "ipv4",
        "realm-opts": {
            "enable": True,
            "server-url": f"http://127.0.0.1:{rendezvous_port}",
            "token": token,
            "realm-id": realm_id,
            "stun-servers": [f"127.0.0.1:{stun_port}"],
        },
    }
    if handshake_timeout is not None:
        proxy["handshake-timeout"] = handshake_timeout
    return {
        "mixed-port": mixed_port,
        "allow-lan": False,
        "bind-address": "127.0.0.1",
        "mode": "rule",
        "log-level": "debug",
        "ipv6": False,
        "proxies": [proxy],
        "rules": ["MATCH,realm-test"],
    }


def run_curl(proxy_port, destination, output, timeout):
    return subprocess.run(
        [
            "curl", "--noproxy", "", "--silent", "--show-error", "--fail",
            "--proxy", f"http://127.0.0.1:{proxy_port}",
            "--max-time", str(timeout), destination, "-o", str(output),
        ],
        capture_output=True,
        text=True,
        timeout=timeout + 5,
    )


def main():
    host = (platform.system(), platform.machine())
    if host not in HYSTERIA_ASSETS or host not in MIHOMO_ASSETS:
        raise RuntimeError(f"no pinned Hysteria/Mihomo live-test assets for host {host}")
    if not all(shutil.which(command) for command in ("curl", "openssl")):
        raise RuntimeError("curl and OpenSSL are required")

    with tempfile.TemporaryDirectory(prefix="hysteriax-realm-live-") as folder:
        temp = pathlib.Path(folder)
        hysteria = temp / "hysteria"
        mihomo_gzip = temp / "mihomo.gz"
        mihomo = temp / "mihomo"
        hysteria_asset, hysteria_digest = HYSTERIA_ASSETS[host]
        mihomo_asset, mihomo_digest = MIHOMO_ASSETS[host]
        download(
            f"https://github.com/apernet/hysteria/releases/download/app/v2.12.3/{hysteria_asset}",
            hysteria_digest,
            hysteria,
        )
        download(
            f"https://github.com/MetaCubeX/mihomo/releases/download/v1.19.31/{mihomo_asset}",
            mihomo_digest,
            mihomo_gzip,
        )
        mihomo.write_bytes(gzip.decompress(mihomo_gzip.read_bytes()))
        mihomo.chmod(0o700)

        rendezvous_port = free_port()
        rendezvous_mixed_port = free_port()
        server_stun_port = free_port(socket.SOCK_DGRAM)
        client_stun_port = free_port(socket.SOCK_DGRAM)
        server_lport = free_port(socket.SOCK_DGRAM)
        http_port = free_port()
        baseline_mixed_port = free_port()
        workaround_mixed_port = free_port()
        token = "hx" + secrets.token_urlsafe(30).replace("-", "a").replace("_", "b")
        realm_id = "hx-test-" + secrets.token_hex(8)
        rendezvous_log = temp / "rendezvous.log"
        server_log = temp / "hysteria-server.log"
        baseline_log = temp / "mihomo-no-timeout.log"
        workaround_log = temp / "mihomo-handshake-timeout.log"

        rendezvous_config = {
            "mixed-port": rendezvous_mixed_port,
            "allow-lan": False,
            "bind-address": "127.0.0.1",
            "mode": "rule",
            "log-level": "info",
            "ipv6": False,
            "listeners": [{
                "name": "local Realm rendezvous",
                "type": "hysteria2-realm",
                "port": rendezvous_port,
                "listen": "127.0.0.1",
                "token": token,
                "realm-name-pattern": "^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$",
            }],
            "rules": ["MATCH,DIRECT"],
        }
        rendezvous_path = temp / "rendezvous.json"
        rendezvous_path.write_text(json.dumps(rendezvous_config, indent=2) + "\n")
        rendezvous_home = temp / "rendezvous-home"
        rendezvous_home.mkdir()
        with rendezvous_log.open("wb") as rendezvous_output:
            validation = subprocess.run(
                [str(mihomo), "-t", "-d", str(rendezvous_home), "-f", str(rendezvous_path)],
                capture_output=True,
                text=True,
                timeout=30,
            )
            if validation.returncode:
                raise RuntimeError("pinned Mihomo rejected the local rendezvous config: " + validation.stderr[-1200:])
            rendezvous = subprocess.Popen(
                [str(mihomo), "-d", str(rendezvous_home), "-f", str(rendezvous_path)],
                stdout=rendezvous_output,
                stderr=rendezvous_output,
                start_new_session=True,
            )

            web_root = temp / "web"
            web_root.mkdir()
            payload = web_root / "payload.bin"
            payload.write_bytes(os.urandom(PAYLOAD_SIZE))
            class QuietHandler(SimpleHTTPRequestHandler):
                def log_message(self, *_args):
                    pass

            handler = partial(QuietHandler, directory=str(web_root))
            http_server = ThreadingHTTPServer(("127.0.0.1", http_port), handler)
            http_thread = threading.Thread(target=http_server.serve_forever, daemon=True)
            http_thread.start()

            server_stun = StunResponder(server_stun_port, delay_seconds=0.02)
            client_stun = StunResponder(client_stun_port, delay_seconds=STUN_DELAY_SECONDS)
            server_stun.start()
            client_stun.start()

            server_process = None
            server_output = None
            baseline_process = None
            workaround_process = None
            try:
                wait_for_tcp(rendezvous_port, rendezvous, "Mihomo rendezvous")
                certificate = temp / "server.crt"
                private_key = temp / "server.key"
                subprocess.run(
                    [
                        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(private_key), "-out", str(certificate),
                        "-days", "1", "-subj", "/CN=127.0.0.1",
                        "-addext", "subjectAltName=IP:127.0.0.1",
                    ],
                    check=True,
                    capture_output=True,
                    timeout=30,
                )
                server_config = {
                    "listen": f"realm+http://{token}@127.0.0.1:{rendezvous_port}/{realm_id}?lport={server_lport}",
                    "auth": {"type": "password", "password": "realm-hy2-test-password"},
                    "tls": {"cert": str(certificate), "key": str(private_key)},
                    "realm": {
                        "stunServers": [f"127.0.0.1:{server_stun_port}"],
                        "stunTimeout": "5s",
                        "punchTimeout": "30s",
                        "ipMode": "v4",
                    },
                }
                server_config_path = temp / "server.json"
                server_config_path.write_text(json.dumps(server_config, indent=2) + "\n")
                server_output = server_log.open("wb")
                server_process = subprocess.Popen(
                    [str(hysteria), "--disable-update-check", "server", "-c", str(server_config_path)],
                    stdout=server_output,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
                time.sleep(1.0)
                if server_process.poll() is not None:
                    raise RuntimeError("pinned Hysteria server exited before registering its Realm")

                destination = f"http://127.0.0.1:{http_port}/payload.bin"
                baseline_path = temp / "baseline.yaml"
                baseline_path.write_text(json.dumps(client_config(
                    baseline_mixed_port, rendezvous_port, token, realm_id,
                    client_stun_port, handshake_timeout=None,
                ), indent=2) + "\n")
                validation = subprocess.run(
                    [str(mihomo), "-t", "-d", str(temp / "baseline-home"), "-f", str(baseline_path)],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                if validation.returncode:
                    raise RuntimeError("pinned Mihomo rejected the baseline Realm client config: " + validation.stderr[-1200:])
                baseline_home = temp / "baseline-home"
                baseline_home.mkdir(exist_ok=True)
                with baseline_log.open("wb") as log:
                    baseline_process = subprocess.Popen(
                        [str(mihomo), "-d", str(baseline_home), "-f", str(baseline_path)],
                        stdout=log,
                        stderr=log,
                        start_new_session=True,
                    )
                    wait_for_tcp(baseline_mixed_port, baseline_process, "baseline Mihomo")
                    baseline = run_curl(
                        baseline_mixed_port, destination, temp / "baseline.bin", timeout=9
                    )
                    if baseline.returncode == 0:
                        raise RuntimeError(
                            "the delayed-STUN baseline unexpectedly transferred without a handshake-timeout override"
                        )
                    stop_process(baseline_process)
                    baseline_process = None
                baseline_details = baseline_log.read_text(errors="replace")
                if "context deadline" not in baseline_details.lower():
                    raise RuntimeError(
                        "the baseline did not fail on the expected short context timeout: "
                        + redacted_log_tail(
                            baseline_log, (token, "realm-hy2-test-password")
                        )
                    )

                workaround_path = temp / "workaround.json"
                workaround_path.write_text(json.dumps(client_config(
                    workaround_mixed_port, rendezvous_port, token, realm_id,
                    client_stun_port, handshake_timeout=30,
                ), indent=2) + "\n")
                validation = subprocess.run(
                    [str(mihomo), "-t", "-d", str(temp / "workaround-home"), "-f", str(workaround_path)],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                if validation.returncode:
                    raise RuntimeError("pinned Mihomo rejected handshake-timeout for Realm: " + validation.stderr[-1200:])
                workaround_home = temp / "workaround-home"
                workaround_home.mkdir(exist_ok=True)
                with workaround_log.open("wb") as log:
                    workaround_process = subprocess.Popen(
                        [str(mihomo), "-d", str(workaround_home), "-f", str(workaround_path)],
                        stdout=log,
                        stderr=log,
                        start_new_session=True,
                    )
                    wait_for_tcp(workaround_mixed_port, workaround_process, "Realm workaround Mihomo")
                    first_attempt = run_curl(
                        workaround_mixed_port, destination, temp / "first-attempt.bin", timeout=9
                    )
                    if first_attempt.returncode == 0:
                        payload_result = temp / "first-attempt.bin"
                    else:
                        time.sleep(2.0)
                        retry = run_curl(
                            workaround_mixed_port, destination, temp / "workaround.bin", timeout=20
                        )
                        if retry.returncode:
                            tail = redacted_log_tail(
                                workaround_log, (token, "realm-hy2-test-password")
                            )
                            raise RuntimeError(
                                "Mihomo handshake-timeout did not recover the Realm transfer: "
                                + tail
                            )
                        payload_result = temp / "workaround.bin"
                    if payload_result.stat().st_size != PAYLOAD_SIZE:
                        raise RuntimeError("Realm workaround returned an incomplete TCP payload")
                    print(
                        "Realm workaround passed: pinned Mihomo v1.19.31 transferred "
                        f"{payload_result.stat().st_size} bytes after a {STUN_DELAY_SECONDS:.1f}s STUN response; "
                        "the same delayed-STUN attempt failed without handshake-timeout."
                    )
            finally:
                stop_process(workaround_process)
                stop_process(baseline_process)
                stop_process(server_process)
                if server_output is not None:
                    server_output.close()
                stop_process(rendezvous)
                client_stun.close()
                server_stun.close()
                http_server.shutdown()
                http_server.server_close()
                http_thread.join(timeout=2)


if __name__ == "__main__":
    main()
