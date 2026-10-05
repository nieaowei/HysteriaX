"""Additional acceptance coverage used by verify-subscription.sh (isolated test schema)."""
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import pathlib
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent


def fetch(base, path, ua=None):
    headers = {} if ua is None else {"User-Agent": ua}
    req = urllib.request.Request(base + path, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            return response.status, response.read(), response.headers
    except urllib.error.HTTPError as error:
        return error.code, error.read(), error.headers


def run(base, admin, database, temp, advanced_token, revoked_token, advanced_user_id, empty_token):
    def api(path, payload=None, method=None, expected=200):
        data = None if payload is None else json.dumps(payload).encode()
        req = urllib.request.Request(base + path, data=data, method=method,
                                     headers={"Authorization": f"Bearer {admin}", "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=10) as response:
            if response.status != expected:
                raise RuntimeError("fixture API returned an unexpected status")
            return json.load(response)

    def expect(token, query="", ua=None, status=200, mime=None):
        code, body, headers = fetch(base, f"/sub/{token}" + query, ua)
        if code != status:
            raise RuntimeError(f"subscription case {query!r}, UA {ua!r}: expected {status}, got {code}")
        if headers.get("Cache-Control") != "private, no-store" or headers.get("Vary") != "User-Agent":
            raise RuntimeError("subscription cache isolation headers missing")
        if headers.get("Referrer-Policy") != "no-referrer" or "noindex" not in headers.get("X-Robots-Tag", ""):
            raise RuntimeError("subscription privacy headers missing")
        if mime and headers.get_content_type() != mime:
            raise RuntimeError("subscription MIME type mismatch")
        return body, headers

    active = api(f"/api/v1/users/{advanced_user_id}/subscription")["active"]
    if not active.get("auto_url", "").endswith(f"/sub/{advanced_token}") or not active["url"].endswith("/clash.yaml"):
        raise RuntimeError("management subscription URL compatibility failed")
    for ua in [None, "", "curl/8.0", "Mozilla/5.0", "Clash/1.0", "unrecognized/1.0"]:
        page, _ = expect(advanced_token, ua=ua, mime="text/html")
        for format_ in ["mihomo", "singbox", "base64", "uri"]:
            if f"?format={format_}".encode() not in page:
                raise RuntimeError("selection page is missing a format link")
        if b"input.select()" not in page or b"navigator.clipboard" not in page or b"{{AUTO_URL}}" in page:
            raise RuntimeError("selection page copy/manual fallback failed")
    expect(advanced_token, "?format=invalid", status=400)
    expect(advanced_token, "?format=uri&format=singbox", status=400)
    expect("invalid-token", status=404)
    expect(revoked_token, status=404)
    expect(advanced_token, "?format=uri", status=422)
    expect(advanced_token, "?format=singbox", ua="Mozilla/5.0", status=422)
    expect(advanced_token, ua="sing-box/1.13.0", status=422)
    advanced, _ = expect(advanced_token, ua="sing-box/1.14.2", mime="application/json")
    advanced_config = json.loads(advanced)
    hy2 = [o for o in advanced_config["outbounds"] if o["type"] == "hysteria2"]
    if len(hy2) != 2 or not any("realm" in o for o in hy2) or not any("ech" in o["tls"] for o in hy2):
        raise RuntimeError("advanced sing-box fields were lost")
    binary = temp / "sing-box"
    subprocess.run([str(ROOT / "scripts/verify-singbox-config.sh"), "--binary", str(binary)], check=True)
    config_path = temp / "singbox-advanced.json"
    config_path.write_bytes(advanced)
    subprocess.run([str(binary), "check", "-c", str(config_path)], check=True)

    # A basic fixture is usable in every export format and can forward real traffic.
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        node_port = sock.getsockname()[1]
    cert, key = temp / "local-cert.pem", temp / "local-key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key),
                    "-out", str(cert), "-days", "2", "-subj", "/CN=localhost"], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ssh = api("/api/v1/credentials", {"name":"Basic subscription SSH","kind":"ssh_password","payload":{"secret":"fixture-only"}}, expected=201)
    identity = api("/api/v1/credentials", {"name":"Basic subscription TLS","kind":"tls_identity","payload":{"certificate":cert.read_text(),"private_key":key.read_text()}}, expected=201)
    node = api("/api/v1/nodes", {
        "name": "東京: edge #&", "ssh_host": "127.0.0.1", "ssh_port": 22, "ssh_username": "root",
        "ssh_credential_id": ssh["id"], "ssh_credential_version": 1, "public_host": "127.0.0.1",
        "public_port": node_port, "listen_addr": f":{node_port}", "tls_sni": "localhost", "tls_skip_verify": True,
        "config": {"tls": {"cert": f"credential://{identity['id']}/1/certificate", "key": f"credential://{identity['id']}/1/private_key"}},
    }, expected=201)["node"]["id"]
    user = api("/api/v1/users", {"name": "Basic subscription fixture"}, expected=201)["id"]
    credential = api(f"/api/v1/users/{user}/assignments", {"expected_revision": 1, "node_id": node}, expected=201)["hy2_credential"]
    receipt = api(f"/api/v1/users/{user}/subscription", {"expected_revision": 2})
    token = receipt["token"]
    if not receipt.get("auto_url", "").endswith(f"/sub/{token}"):
        raise RuntimeError("rotation response did not expose auto_url")
    with database.connect() as connection:
        connection.execute("UPDATE nodes SET deployed_revision = desired_revision, deployed_config_enc = desired_config_enc, state = 'deployed' WHERE id = %s", (node,))
    api(f"/api/v1/users/{advanced_user_id}/assignments", {"expected_revision": 5, "node_id": node}, expected=201)
    filtered, headers = expect(advanced_token, "?format=uri", mime="text/plain")
    if headers.get("X-HysteriaX-Filtered-Nodes") != "2" or filtered.count(b"hysteria2://") != 1:
        raise RuntimeError("partial compatibility filtering failed")
    filtered, headers = expect(advanced_token, "?format=singbox", mime="application/json")
    if headers.get("X-HysteriaX-Filtered-Nodes") != "2":
        raise RuntimeError("unknown-version sing-box did not conservatively filter advanced nodes")
    for ua, mime in [("MIHOMO/v1.19.31", "application/yaml"), ("clash.meta/1.19.31", "application/yaml"),
                     ("clashmeta 1.19.31", "application/yaml"), ("SingBox/1.14.2", "application/json"),
                     ("sing-box/1.11.0", "application/json"), ("Shadowrocket/2.2.0", "text/plain"),
                     ("v2rayN/7.0", "text/plain"), ("v2rayNG/1.9", "text/plain")]:
        expect(token, ua=ua, mime=mime)
    expect(token, "?format=mihomo", ua="sing-box/1.14.2", mime="application/yaml")
    expect(token, "?format=singbox", ua="mihomo/1.19.31", mime="application/json")
    expect(token, ua="sing-box/1.10.0", status=422)
    code, body, headers = fetch(base, f"/sub/{token}/clash.yaml?format=uri", "sing-box/1.14.2")
    if code != 200 or headers.get_content_type() != "application/yaml":
        raise RuntimeError("legacy route negotiated another format")
    uri, _ = expect(token, "?format=uri", mime="text/plain")
    encoded, _ = expect(token, "?format=base64", mime="text/plain")
    if base64.b64decode(encoded) != uri:
        raise RuntimeError("URI/Base64 outputs differ")
    parsed = urllib.parse.urlsplit(uri.decode().strip())
    if urllib.parse.unquote(parsed.username) != credential or "東京: edge #&" not in urllib.parse.unquote(parsed.fragment):
        raise RuntimeError("URI credentials or Unicode node names were corrupted")
    client_bytes, _ = expect(token, "?format=singbox", mime="application/json")
    for format_ in ["mihomo", "singbox", "base64", "uri"]:
        expect(empty_token, f"?format={format_}")
    empty_json, _ = expect(empty_token, "?format=singbox")
    empty_config = temp / "singbox-empty.json"
    empty_config.write_bytes(empty_json)
    subprocess.run([str(binary), "check", "-c", str(empty_config)], check=True)

    # Public page and downloads enforce the same account eligibility checks.
    cases = [("enabled = false", "enabled = true"),
             ("expires_at = CURRENT_TIMESTAMP - INTERVAL '1 day'", "expires_at = NULL"),
             ("quota_bytes = 1, usage_bytes = 1", "quota_bytes = NULL, usage_bytes = 0")]
    for change, restore in cases:
        with database.connect() as connection:
            connection.execute(f"UPDATE users SET {change} WHERE id = %s", (user,))
        for query in ["", "?format=auto", "?format=mihomo", "?format=singbox", "?format=base64", "?format=uri"]:
            expect(token, query, status=403)
        code, _, headers = fetch(base, f"/sub/{token}/clash.yaml")
        if code != 403 or headers.get("Cache-Control") != "private, no-store":
            raise RuntimeError("legacy subscription eligibility/caching regression")
        with database.connect() as connection:
            connection.execute(f"UPDATE users SET {restore} WHERE id = %s", (user,))

    verify_connection(binary, temp, client_bytes, credential, node_port, cert, key)
    print("Auto negotiation, selection page, filtering, eligibility, URI/Base64, and sing-box v1.14.2 parsing/traffic passed.")


def verify_connection(binary, temp, client_bytes, credential, node_port, cert, key):
    payload = b"hysteriax-subscription-forwarding\n" * 4096

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *_args):
            pass

    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=http.serve_forever, daemon=True)
    thread.start()
    config = {"inbounds": [{"type": "hysteria2", "listen": "127.0.0.1", "listen_port": node_port,
                            "users": [{"name": "fixture", "password": credential}],
                            "tls": {"enabled": True, "certificate_path": str(cert), "key_path": str(key)}}],
              "outbounds": [{"type": "direct", "tag": "DIRECT"}], "route": {"final": "DIRECT"}}
    server_config = temp / "singbox-server.json"
    server_config.write_text(json.dumps(config))
    client_config = json.loads(client_bytes)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        proxy_port = sock.getsockname()[1]
    client_config["inbounds"][0]["listen_port"] = proxy_port
    client_path = temp / "singbox-client.json"
    client_path.write_text(json.dumps(client_config))
    processes = []
    try:
        with (temp / "singbox-runtime.log").open("wb") as log:
            for path in [server_config, client_path]:
                processes.append(subprocess.Popen([str(binary), "run", "-c", str(path)], stdout=log, stderr=log))
            for _ in range(50):
                if any(process.poll() is not None for process in processes):
                    raise RuntimeError("sing-box runtime fixture exited before forwarding")
                try:
                    with socket.create_connection(("127.0.0.1", proxy_port), timeout=0.2):
                        break
                except OSError:
                    time.sleep(0.1)
            result = subprocess.run(["curl", "--fail", "--silent", "--show-error", "--noproxy", "",
                                     "--proxy", f"socks5h://127.0.0.1:{proxy_port}", "--max-time", "20",
                                     f"http://127.0.0.1:{http.server_address[1]}/fixture"], capture_output=True)
            if result.returncode != 0 or result.stdout != payload:
                raise RuntimeError("generated sing-box subscription did not forward the expected payload")
    finally:
        for process in processes:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        http.shutdown()
        http.server_close()
        thread.join(timeout=5)
