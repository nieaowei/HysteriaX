"""Create credential objects for the existing live-test fixture descriptions.

Credential fixture writes use IDs and pinned versions. Authorization fixtures
use the real group preview/commit protocol; retired assignment routes are never
translated.
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

    def create(name, kind, material):
        body = {"name": name, "kind": kind, "payload": material}
        prefix = "/api/v2" if path.startswith("/api/v2/") else "/api/v1"
        status, result = raw_request(base, prefix + "/credentials", token, "POST", body)
        if status != 201:
            return None, (status, result)
        return json.loads(result), None

    if path in ("/api/v1/nodes", "/api/v2/nodes") or (path.startswith(("/api/v1/nodes/", "/api/v2/nodes/")) and method == "PATCH"):
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
            if "clientCA" in tls:
                raise RuntimeError("Legacy clientCA fixtures must select config.mtls managed CA identity")
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
            # Some older fixtures construct a credential URI from the returned
            # resource ID instead of using its explicit reference.
            _STAGED_TLS[f"credential://{identity}/1/content"] = (kind, content.decode())
        else:
            receipt, error = create(payload["name"], kind, {"content": content.decode()})
            if error: return error
            identity = receipt["id"]
            reference = f"credential://{identity}/{receipt['version']}/content"
        return 201, json.dumps({"id": identity, "resource_kind": kind, "reference": reference,
            "content_sha256": hashlib.sha256(content).hexdigest(), "size_bytes": len(content)}).encode()

    return raw_request(base, path, token, method, payload)


def grant_group(expect, user_id, node_ids, name=None, *, api_prefix="/api/v1"):
    """Exercise the actual group preview/commit protocol; no retired assignment route."""
    draft = {"action": "create", "name": name or "Live grant " + uuid.uuid4().hex[:12],
             "user_ids": [user_id], "node_ids": list(node_ids)}
    preview = expect(api_prefix + "/authorization-groups/preview", "POST", draft)
    draft.pop("action")
    draft["preview_token"] = preview["preview_token"]
    return expect(api_prefix + "/authorization-groups", "POST", draft, (201,))


def request_group(raw_request, base, token, user_ids, node_ids, name=None):
    """Return the actual group receipt, preserving real HTTP failure statuses."""
    draft = {"action":"create", "name":name or "Live grant " + uuid.uuid4().hex[:12],
             "user_ids":list(user_ids), "node_ids":list(node_ids)}
    status,body = raw_request(base,"/api/v1/authorization-groups/preview",token,"POST",draft)
    if status!=200:return status,body
    draft.pop("action")
    draft["preview_token"]=json.loads(body)["preview_token"]
    return raw_request(base,"/api/v1/authorization-groups",token,"POST",draft)
