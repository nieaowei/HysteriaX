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
import re


def request_with_group_assignment(raw_request, base, path, token, method, payload):
    """Translate legacy fixture descriptions into actual group-only authorization.

    Tests of the retired endpoint itself must use raw_request directly.
    Each fixture grant creates a single-pair group, preserving independent grants.
    """
    match = re.fullmatch(r"/api/v1/users/([^/]+)/assignments(?:/([^/]+))?", path)
    if not match or method not in ("POST", "DELETE"):
        return raw_request(base, path, token, method, payload)
    user_id, removal_node = match.groups()
    status, raw_user = raw_request(base, f"/api/v1/users/{user_id}", token, "GET", None)
    if status != 200:
        return status, raw_user
    user = json.loads(raw_user)
    if user["revision"] != payload["expected_revision"]:
        return 409, json.dumps({"error": {"code": "revision_conflict", "message": "fixture user revision changed"}}).encode()
    node_id = removal_node or payload["node_id"]
    if method == "DELETE":
        status, raw_groups = raw_request(base, "/api/v1/authorization-groups", token, "GET", None)
        if status != 200:
            return status, raw_groups
        candidates = [group for group in json.loads(raw_groups)
                      if group["user_ids"] == [user_id] and group["node_ids"] == [node_id]]
        if len(candidates) != 1:
            raise RuntimeError("fixture removal requires exactly one single-pair authorization group")
        group = candidates[0]
        group_path = f"/api/v1/authorization-groups/{group['id']}"
        draft = {"action": "delete", "expected_revision": group["revision"]}
        status, raw_preview = raw_request(base, group_path + "/preview", token, "POST", draft)
        if status != 200:
            return status, raw_preview
        preview = json.loads(raw_preview)
        status, result = raw_request(base, group_path, token, "DELETE", {
            "expected_revision": group["revision"], "preview_token": preview["preview_token"]})
        if status != 200:
            return status, result
        return 200, json.dumps({"user_id": user_id, "node_id": node_id,
                                "revision": user["revision"] + 1, "kick_queued": True}).encode()
    if any(item["node_id"] == node_id for item in user["assignments"]):
        return 409, json.dumps({"error": {"code": "already_assigned", "message": "fixture grant already exists"}}).encode()
    bindings = []
    if payload.get("mtls_credential_id"):
        bindings.append({"user_id": user_id, "node_id": node_id,
                         "credential_id": payload["mtls_credential_id"],
                         "credential_version": payload["mtls_credential_version"]})
    draft = {"action": "create", "name": "Fixture grant " + uuid.uuid4().hex[:12],
             "user_ids": [user_id], "node_ids": [node_id], "mtls_bindings": bindings}
    status, raw_preview = raw_request(base, "/api/v1/authorization-groups/preview", token, "POST", draft)
    if status != 200:
        return status, raw_preview
    preview = json.loads(raw_preview)
    if preview["missing_mtls"]:
        return 400, json.dumps({"error": {"code": "invalid_request", "message": "fixture requires user-owned mTLS binding"}}).encode()
    draft.pop("action")
    draft["preview_token"] = preview["preview_token"]
    status, result = raw_request(base, "/api/v1/authorization-groups", token, "POST", draft)
    if status != 201:
        return status, result
    receipt = json.loads(result)
    credential = next(item["hy2_credential"] for item in receipt["created_credentials"]
                      if item["user_id"] == user_id and item["node_id"] == node_id)
    return 201, json.dumps({"user_id": user_id, "node_id": node_id,
                           "revision": user["revision"] + 1, "hy2_credential": credential}).encode()


def request_with_credentials(raw_request, base, path, token=None, method="GET", payload=None):
    if method == "DELETE" and "/assignments/" in path:
        return request_with_group_assignment(raw_request, base, path, token, method, payload)
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

    if path.startswith("/api/v1/users/") and "/assignments" in path and "client_certificate" in payload:
        user = path.split("/")[4]
        material = {"certificate": payload.pop("client_certificate"), "private_key": payload.pop("client_private_key", "")}
        receipt, error = create("Fixture mTLS", "tls_identity", material, user)
        if error:
            return error
        payload.update(mtls_credential_id=receipt["id"], mtls_credential_version=receipt["version"])
    return request_with_group_assignment(raw_request, base, path, token, method, payload)
