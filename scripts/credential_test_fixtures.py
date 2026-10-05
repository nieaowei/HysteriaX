"""Create v1 credential objects for the existing live-test fixture descriptions.

This is test authoring convenience, not server compatibility: every write sent
to a node/assignment contains IDs and pinned versions, never inline secrets.
"""
import base64
import copy
import hashlib
import uuid

_STAGED_TLS = {}
import json


def request_with_credentials(raw_request, base, path, token=None, method="GET", payload=None):
    if payload is None or method not in ("POST", "PATCH", "PUT"):
        return raw_request(base, path, token, method, payload)
    payload = copy.deepcopy(payload)

    def create(name, kind, material, owner=None):
        body = {"name": name, "kind": kind, "payload": material}
        if owner is not None:
            body["owner_user_id"] = owner
        status, result = raw_request(base, "/api/v1/credentials", token, "POST", body)
        if status != 201:
            return None, (status, result)
        return json.loads(result), None

    if path == "/api/v1/nodes" or (path.startswith("/api/v1/nodes/") and method == "PATCH"):
        if "ssh_secret" in payload:
            kind = "ssh_password" if payload.pop("ssh_auth_type", "private_key") == "password" else "ssh_private_key"
            material = {"secret": payload.pop("ssh_secret")}
            phrase = payload.pop("ssh_passphrase", None)
            if phrase is not None:
                material["passphrase"] = phrase
            receipt, error = create(payload.get("name", "Fixture") + " · SSH", kind, material)
            if error:
                return error
            payload.update(ssh_credential_id=receipt["id"], ssh_credential_version=receipt["version"])
        tls = payload.get("config", {}).get("tls")
        if tls:
            cert, key = tls.get("cert"), tls.get("key")
            if cert in _STAGED_TLS or key in _STAGED_TLS:
                if cert not in _STAGED_TLS or key not in _STAGED_TLS:
                    return 400, b'{"error":"TLS fixture requires a certificate pair"}'
                receipt, error = create("Fixture TLS pair", "tls_identity", {"certificate": _STAGED_TLS[cert][1], "private_key": _STAGED_TLS[key][1]})
                if error: return error
                prefix = f"credential://{receipt['id']}/{receipt['version']}/"
                tls.update(cert=prefix+"certificate", key=prefix+"private_key")
            ca = tls.get("clientCA")
            if ca in _STAGED_TLS:
                receipt, error = create("Fixture CA", "ca_certificate", {"content": _STAGED_TLS[ca][1]})
                if error: return error
                tls["clientCA"] = f"credential://{receipt['id']}/{receipt['version']}/content"
        dns = payload.get("config", {}).get("acme", {}).get("dns")
        if dns and dns.get("config") and any(not str(v).startswith("credential://") for v in dns["config"].values()):
            receipt, error = create("Fixture DNS", "dns", {"provider": dns["name"], "config": dns["config"]})
            if error:
                return error
            dns["config"] = {key: f"credential://{receipt['id']}/{receipt['version']}/{key}" for key in dns["config"]}

    if path.endswith("/resources") and method == "POST" and payload.get("resource_kind") in ("certificate", "private_key", "ech_key"):
        content = base64.b64decode(payload["content_base64"])
        kind = payload["resource_kind"]
        if kind in ("certificate", "private_key"):
            # Old fixture descriptions upload two files; stage them locally and
            # send only a validated certificate-pair write when config is authored.
            identity = str(uuid.uuid4())
            reference = "fixture-tls://" + identity
            _STAGED_TLS[reference] = (kind, content.decode())
        else:
            receipt, error = create(payload["name"], kind, {"content": content.decode()})
            if error: return error
            identity = receipt["id"]
            reference = f"credential://{identity}/{receipt['version']}/content"
        return 201, json.dumps({"id": identity, "resource_kind": kind, "reference": reference,
            "content_sha256": hashlib.sha256(content).hexdigest(), "size_bytes": len(content)}).encode()

    if path.startswith("/api/v1/users/") and "/assignments" in path and "client_certificate" in payload:
        user = path.split("/")[4]
        material = {"certificate": payload.pop("client_certificate"), "private_key": payload.pop("client_private_key", "")}
        receipt, error = create("Fixture mTLS", "tls_identity", material, user)
        if error:
            return error
        payload.update(mtls_credential_id=receipt["id"], mtls_credential_version=receipt["version"])
    return raw_request(base, path, token, method, payload)
