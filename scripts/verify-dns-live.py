#!/usr/bin/env python3
"""Verify DNS management against a chosen Cloudflare zone and disposable SSH node.

The credential stays in the service vault. Only this run's node/user/SSH key and
prefixed DNS records are removed. A useful DNS connection/zone remains enabled.
"""
import argparse
import ast
import hashlib
import platform
import json
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import yaml

ROOT = Path(__file__).resolve().parent.parent


def environment():
    result = {}
    for line in (ROOT / ".env").read_text().splitlines():
        if "=" in line and not line.startswith("#"):
            key, value = line.split("=", 1)
            result[key] = value.strip().strip('"').strip("'")
    return result


class API:
    def __init__(self, base, token):
        self.base, self.token = base.rstrip("/"), token

    def request(self, method, path, body=None, missing=False, raw=False):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.base + path, data=data, method=method,
            headers={"Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                data = response.read()
                return data if raw else (json.loads(data) if data else None)
        except urllib.error.HTTPError as error:
            if error.code == 404 and missing:
                return None
            payload = error.read()
            try:
                message = json.loads(payload).get("message", "request failed")
            except (ValueError, AttributeError):
                message = "request failed"
            route = "/sub/[redacted]" if path.startswith("/sub/") else path
            raise RuntimeError(f"{method} {route}: HTTP {error.code}: {message}") from None

    def get(self, path, **kwargs):
        if path == "/api/v1/jobs":
            items, page = [], 1
            while True:
                result = self.request("GET", f"{path}?page={page}&page_size=200", **kwargs)
                items.extend(result["items"])
                if result["page"] * result["page_size"] >= result["total"]:
                    return items
                page += 1
        return self.request("GET", path, **kwargs)

    def write(self, method, path, body):
        return self.request(method, path, body)

    def job(self, identifier, timeout=360, allow_failure=False):
        print("Waiting for job", identifier, flush=True)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            detail = self.get("/api/v1/jobs/" + identifier)
            job = detail["job"]
            if job["status"] in ("succeeded", "failed", "rolled_back", "cancelled"):
                if job["status"] != "succeeded" and not allow_failure:
                    raise RuntimeError(f"job {identifier}: {job['status']}: {job.get('error_message')}")
                return job
            time.sleep(2)
        raise RuntimeError("job did not finish: " + identifier)


def action(revision):
    return {"expected_revision": revision, "idempotency_key": str(uuid.uuid4())}


def download_client(destination):
    # Reuse the repository's pinned asset table without loading its DB test runner.
    tree = ast.parse((ROOT / "scripts/verify-deployment-matrix.py").read_text())
    table = next(node.value for node in tree.body if isinstance(node, ast.Assign)
                 and any(isinstance(target, ast.Name) and target.id == "HYSTERIA_CLIENT_ASSETS" for target in node.targets))
    assets = ast.literal_eval(table)
    asset, expected = assets[(platform.system(), platform.machine())]
    with urllib.request.urlopen("https://github.com/apernet/hysteria/releases/download/app/v2.12.3/" + asset, timeout=90) as response:
        content = response.read()
    if hashlib.sha256(content).hexdigest() != expected:
        raise RuntimeError("pinned Hysteria client digest mismatch")
    destination.write_bytes(content)
    destination.chmod(0o700)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="https://hysteriax.cc")
    parser.add_argument("--zone", required=True)
    parser.add_argument("--credential", required=True)
    parser.add_argument("--ssh-host", required=True)
    parser.add_argument("--ssh-port", type=int, default=22)
    parser.add_argument("--ssh-key", type=Path, required=True)
    parser.add_argument("--ssh-fingerprint", required=True)
    parser.add_argument("--public-ip", required=True)
    parser.add_argument("--public-port", type=int, default=443)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--resume", action="store_true", help="resume proxy/switch checks after setup completed")
    args = parser.parse_args()
    api = API(args.base_url, environment()["HYSTERIAX_ADMIN_TOKEN"])
    prefix = "hysteriax-dns-test"
    run = uuid.uuid4().hex[:8]
    manual = f"{prefix}-{run}-manual.{args.zone}"
    alias = f"{prefix}-{run}-cname.{args.zone}"
    manifest = {"run": run, "zone": args.zone, "record_ids": [], "node_ids": [], "user_ids": [], "credential_ids": [], "checks": []}

    if args.resume:
        manifest = json.loads(args.manifest.read_text())
        run = manifest["run"]
        manual = f"{prefix}-{run}-manual.{args.zone}"
        alias = f"{prefix}-{run}-cname.{args.zone}"
    elif args.manifest.exists():
        raise RuntimeError("manifest already exists; use --resume or choose a new manifest path")

    def save():
        args.manifest.write_text(json.dumps(manifest, indent=2))
        args.manifest.chmod(0o600)

    def passed(message):
        manifest["checks"].append(message)
        save()
        print("PASS:", message, flush=True)

    def write_record(name, kind, content):
        request = {"zone_id": zone["id"], "idempotency_key": str(uuid.uuid4()),
                   "record": {"name": name, "record_type": kind, "content": content, "ttl": 1, "proxied": False}}
        receipt = api.write("POST", "/api/v1/dns/records", request)
        manifest["record_ids"].append(receipt["resource_id"])
        save()
        duplicate = api.write("POST", "/api/v1/dns/records", request)
        assert duplicate["resource_id"] == receipt["resource_id"]
        api.job(receipt["job_id"])
        record = api.get("/api/v1/dns/records/" + receipt["resource_id"])
        return record

    def check(record):
        receipt = api.write("POST", "/api/v1/dns/records/" + record["id"] + "/check", action(record["revision"]))
        api.job(receipt["job_id"], timeout=660)
        current = api.get("/api/v1/dns/records/" + record["id"])
        assert current["resolution_status"] == "verified", current["resolution_status"]
        return current

    def wait_node(node, revision, failure=False):
        deadline = time.monotonic() + 450
        while time.monotonic() < deadline:
            jobs = [job for job in api.get("/api/v1/jobs") if job.get("node_id") == node and job.get("target_revision") == revision and job["kind"] in ("deploy", "sync", "rollback")]
            if jobs:
                return api.job(jobs[0]["id"], allow_failure=failure)
            time.sleep(1)
        raise RuntimeError("node configuration did not queue deployment")

    def transfer(proxy, directory, binary):
        configuration = {"server": f"{proxy['server']}:{proxy['port']}", "auth": proxy["password"],
                         "tls": {"sni": proxy.get("sni", proxy['server']), "insecure": False},
                         "socks5": {"listen": "127.0.0.1:19839"}}
        path = directory / "client.yaml"
        path.write_text(yaml.safe_dump(configuration))
        path.chmod(0o600)
        with (directory / "client.log").open("w") as log:
            process = subprocess.Popen([str(binary), "client", "-c", str(path)], stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 45
                while time.monotonic() < deadline:
                    result = subprocess.run(["curl", "--silent", "--show-error", "--max-time", "5", "--proxy", "socks5h://127.0.0.1:19839", args.base_url + "/readyz"], capture_output=True)
                    if result.returncode == 0 and json.loads(result.stdout).get("status") in ("ok", "ready"):
                        return
                    if process.poll() is not None:
                        raise RuntimeError("pinned Hysteria client exited before the proxy probe succeeded")
                    time.sleep(1)
                raise RuntimeError("public DNS/TLS/UDP proxy transfer did not succeed")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()

    save()
    if not args.resume:
        credentials = api.get("/api/v1/credentials")
        credential = next(entry for entry in credentials if entry.get("name") == args.credential and entry.get("kind") == "dns" and entry.get("metadata", {}).get("provider") == "cloudflare")
        connections = api.get("/api/v1/dns/connections")
        connection = next((entry for entry in connections if entry["credential_id"] == credential["id"]), None)
        if connection is None:
            connection = api.write("POST", "/api/v1/dns/connections", {"name": args.credential, "credential_id": credential["id"], "credential_version": credential["latest_version"]})
        manifest["connection_id"] = connection["id"]
        save()
        receipt = api.write("POST", "/api/v1/dns/connections/" + connection["id"] + "/verify", action(connection["revision"]))
        api.job(receipt["job_id"])
        zone = next(entry for entry in api.get("/api/v1/dns/zones") if entry["connection_id"] == connection["id"] and entry["name"] == args.zone)
        if not zone["enabled"]:
            zone = api.write("PATCH", "/api/v1/dns/zones/" + zone["id"], {"expected_revision": zone["revision"], "enabled": True})
        receipt = api.write("POST", "/api/v1/dns/zones/" + zone["id"] + "/refresh", action(zone["revision"]))
        api.job(receipt["job_id"])
        passed("Cloudflare credential/zone discovery and record refresh")
    
        record = check(write_record(manual, "A", args.public_ip))
        ipv6 = check(write_record(f"{prefix}-{run}-aaaa.{args.zone}", "AAAA", "2606:4700:4700::1111"))
        cname = check(write_record(alias, "CNAME", manual))
        assert ipv6["content"] == "2606:4700:4700::1111"
        passed("real A/AAAA/CNAME creation, idempotency and authoritative/recursive DNS checks")
        request = {"expected_revision": record["revision"], "idempotency_key": str(uuid.uuid4()),
                   "record": {"name": record["name"], "record_type": "A", "content": args.public_ip, "ttl": 300, "proxied": False}}
        receipt = api.write("PATCH", "/api/v1/dns/records/" + record["id"], request)
        api.job(receipt["job_id"])
        record = api.get("/api/v1/dns/records/" + record["id"])
        assert record["ttl"] == 300
        passed("real Cloudflare record update")
    
        ssh = api.write("POST", "/api/v1/credentials", {"name": f"DNS test SSH {run}", "kind": "ssh_private_key", "payload": {"secret": args.ssh_key.read_text()}})
        manifest["credential_ids"].append(ssh["id"])
        save()
        allocation = {"idempotency_key": str(uuid.uuid4()), "zone_id": zone["id"], "mode": "auto", "prefix": prefix, "ipv4": args.public_ip}
        request = {"name": f"DNS live {run}", "ssh_host": args.ssh_host, "ssh_port": args.ssh_port, "ssh_username": "root", "ssh_credential_id": ssh["id"], "ssh_credential_version": ssh.get("version", 1), "ssh_host_fingerprint": args.ssh_fingerprint,
                   "public_port": args.public_port, "listen_addr": ":443", "config": {}, "dns_allocation": allocation}
        created = api.write("POST", "/api/v1/nodes", request)
        node_id = created["node"]["id"]
        manifest["node_ids"].append(node_id)
        manifest["record_ids"].extend(created["dns_allocation"]["record_ids"])
        save()
        duplicate = api.write("POST", "/api/v1/nodes", request)
        assert duplicate["node"]["id"] == node_id and "node_auth_token" not in duplicate
        for job in created["dns_allocation"]["job_ids"]:
            api.job(job)
        node = api.get("/api/v1/nodes/" + node_id)
        auto_host = node["public"]["host"]
        assert node["config"].get("acme") is None
        for item in node["dns_binding"]["records"]:
            check(item)
        passed("automatic node allocation is idempotent and does not enable ACME")
    
        config = {"acme": {"domains": [auto_host, manual], "ca": "letsencrypt", "type": "dns", "dns": {"name": "cloudflare", "config": {"cloudflare_api_token": f"credential://{credential['id']}/{credential['latest_version']}/cloudflare_api_token"}}}}
        updated = api.write("PATCH", "/api/v1/nodes/" + node_id, {"expected_revision": node["revision"], "config": config})
        wait_node(node_id, updated["revision"])
        node = api.get("/api/v1/nodes/" + node_id)
        assert node["published_connection"]["public_host"] == auto_host
        passed("separate ACME proxy configuration, first deployment and endpoint publication")
    
        user = api.write("POST", "/api/v1/users", {"name": f"DNS live user {run}"})
        manifest["user_ids"].append(user["id"])
        save()
        api.write("POST", "/api/v1/users/" + user["id"] + "/assignments", {"expected_revision": user["revision"], "node_id": node_id})
        current_user = api.get("/api/v1/users/" + user["id"])
        subscription = api.write("POST", "/api/v1/users/" + user["id"] + "/subscription", {"expected_revision": current_user["revision"]})
        token = subscription["token"]
    else:
        node_id = manifest["node_ids"][0]
        node = api.get("/api/v1/nodes/" + node_id)
        auto_host = node["dns_binding"]["hostname"]
        record = api.get("/api/v1/dns/records/" + manifest["record_ids"][0])
        cname = api.get("/api/v1/dns/records/" + manifest["record_ids"][2])
        zone = next(entry for entry in api.get("/api/v1/dns/zones") if entry["name"] == args.zone)
        current = api.get("/api/v1/users/" + manifest["user_ids"][0] + "/subscription")
        token = current["active"]["token"]

    def proxy():
        content = api.get("/sub/" + token + "/clash.yaml", raw=True)
        return yaml.safe_load(content)["proxies"][0]

    with tempfile.TemporaryDirectory(prefix="hysteriax-dns-client-") as folder:
        directory = Path(folder)
        binary = directory / "hysteria"
        download_client(binary)
        assert proxy()["server"] == auto_host
        transfer(proxy(), directory, binary)
        passed("subscription and real public UDP/TLS proxy transfer on allocated hostname")
        api.write("PUT", "/api/v1/nodes/" + node_id + "/dns-binding", {"expected_revision": node["revision"], "allocation": {"idempotency_key": str(uuid.uuid4()), "zone_id": zone["id"], "mode": "existing", "record_ids": [record["id"]]}})
        changed = api.get("/api/v1/nodes/" + node_id)
        wait_node(node_id, changed["revision"])
        assert proxy()["server"] == manual
        transfer(proxy(), directory, binary)
        passed("manual existing-record reassignment and published subscription transfer")
        historical = api.get("/api/v1/nodes/" + node_id)
        receipt = api.write("POST", "/api/v1/nodes/" + node_id + "/rollback", {"expected_revision": historical["revision"]})
        api.job(receipt["job_id"])
        assert proxy()["server"] == auto_host
        transfer(proxy(), directory, binary)
        passed("explicit rollback publishes the historical hostname and remains reachable")
        historical = api.get("/api/v1/nodes/" + node_id)
        receipt = api.write("POST", "/api/v1/nodes/" + node_id + "/sync", {"expected_revision": historical["revision"]})
        api.job(receipt["job_id"])
        assert proxy()["server"] == manual
        passed("sync restores the desired domain after rollback")
        changed = api.get("/api/v1/nodes/" + node_id)
        api.write("PUT", "/api/v1/nodes/" + node_id + "/dns-binding", {"expected_revision": changed["revision"], "allocation": {"idempotency_key": str(uuid.uuid4()), "zone_id": zone["id"], "mode": "existing", "record_ids": [cname["id"]]}})
        failed_target = api.get("/api/v1/nodes/" + node_id)
        failure = wait_node(node_id, failed_target["revision"], failure=True)
        assert failure["status"] in ("failed", "rolled_back")
        assert proxy()["server"] == manual
        transfer(proxy(), directory, binary)
        passed("certificate-mismatched domain switch retains working published subscription")
        failed_target = api.get("/api/v1/nodes/" + node_id)
        api.write("PUT", "/api/v1/nodes/" + node_id + "/dns-binding", {"expected_revision": failed_target["revision"], "allocation": {"idempotency_key": str(uuid.uuid4()), "zone_id": zone["id"], "mode": "existing", "record_ids": [record["id"]]}})
        restored = api.get("/api/v1/nodes/" + node_id)
        wait_node(node_id, restored["revision"])
        passed("domain recovery after failed switch")

    manifest["verification_complete"] = True
    save()
    if not args.keep:
        cleanup(api, manifest, args.manifest)


def cleanup(api, manifest, path):
    for identifier in manifest["user_ids"]:
        user = api.get("/api/v1/users/" + identifier, missing=True)
        if user:
            api.request("DELETE", f"/api/v1/users/{identifier}?expected_revision={user['revision']}")
    for identifier in manifest["node_ids"]:
        node = api.get("/api/v1/nodes/" + identifier, missing=True)
        if node:
            receipt = api.request("DELETE", f"/api/v1/nodes/{identifier}?expected_revision={node['revision']}")
            if receipt:
                api.job(receipt["job_id"])
        assert api.get("/api/v1/nodes/" + identifier, missing=True) is None
    for identifier in manifest["record_ids"]:
        record = api.get("/api/v1/dns/records/" + identifier, missing=True)
        if record and record["state"] != "deleted":
            receipt = api.write("DELETE", "/api/v1/dns/records/" + identifier, action(record["revision"]))
            api.job(receipt["job_id"])
        current = api.get("/api/v1/dns/records/" + identifier, missing=True)
        assert current is None or current["state"] == "deleted"
    for identifier in manifest["credential_ids"]:
        credential = api.get("/api/v1/credentials/" + identifier, missing=True)
        if credential:
            api.request("DELETE", f"/api/v1/credentials/{identifier}?expected_revision={credential['revision']}")
    manifest["cleanup_complete"] = True
    path.write_text(json.dumps(manifest, indent=2))
    path.chmod(0o600)
    print("PASS: removed only this run's nodes/users/SSH credential/DNS records", flush=True)


if __name__ == "__main__":
    main()
