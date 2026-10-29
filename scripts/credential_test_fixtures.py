"""Create credential objects for the existing live-test fixture descriptions.

Credential fixture writes use IDs and pinned versions. Authorization fixtures
use the real direct group creation protocol; retired assignment routes are never
translated.
"""
import base64
import copy
import hashlib
import uuid
from urllib.parse import quote

_STAGED_TLS = {}
import json



def request_with_credentials(raw_request, base, path, token=None, method="GET", payload=None):
    if not path.startswith("/api/v2/") or payload is None or method not in ("POST", "PATCH", "PUT"):
        return raw_request(base, path, token, method, payload)
    payload = copy.deepcopy(payload)

    def create(name, kind, material):
        body = {"name": name, "kind": kind, "payload": material}
        prefix = "/api/v2"
        status, result = raw_request(base, prefix + "/credentials", token, "POST", body)
        if status != 201:
            return None, (status, result)
        return json.loads(result), None

    if path == "/api/v2/nodes" or (path.startswith("/api/v2/nodes/") and method == "PATCH"):
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


def wait_authorization_job(expect, receipt, *, api_prefix="/api/v2", timeout=120):
    """Wait for the authorization task; signing and disconnects remain separate."""
    import time
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        detail = expect(api_prefix + "/jobs/" + receipt["job_id"])["job"]
        if detail["status"] == "succeeded":
            return detail
        if detail["status"] in {"failed", "cancelled", "rolled_back"}:
            raise RuntimeError("authorization task failed: " + str(detail.get("error_message")))
        time.sleep(0.1)
    raise RuntimeError("authorization task timed out")


def user_connection_password(database, master_key, user_id):
    """Read only this disposable fixture's shared user password; never print it."""
    from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_decrypt
    with database.connect() as connection:
        encrypted = connection.execute("SELECT connection_credential_enc FROM users WHERE id=%s", (user_id,)).fetchone()[0]
    raw = base64.b64decode(encrypted + "=" * (-len(encrypted) % 4))
    key = base64.b64decode(master_key + "=" * (-len(master_key) % 4))
    return crypto_aead_xchacha20poly1305_ietf_decrypt(raw[24:], None, raw[:24], key).decode()


def grant_group(expect, user_id, node_ids, name=None, *, api_prefix="/api/v2"):
    """Create, grant, and await a ready live-test fixture using async tasks."""
    import time
    name = name or "Live grant " + uuid.uuid4().hex[:12]
    path = api_prefix + "/authorization-groups"
    created = expect(path, "POST", {"name": name}, (202,))
    wait_authorization_job(expect, created, api_prefix=api_prefix)
    group = expect(path + "/" + created["resource_id"])
    patch = {"expected_revision": group["revision"], "add_user_ids": [user_id]}
    if node_ids: patch["add_node_ids"] = list(node_ids)
    receipt = expect(path + "/" + group["id"], "PATCH", patch, (202,))
    wait_authorization_job(expect, receipt, api_prefix=api_prefix)
    # Traffic fixtures additionally await initial signing, without changing the
    # production authorization receipt into a synchronous signing protocol.
    deadline = time.monotonic() + 120
    while True:
        user = expect(api_prefix + "/users/" + user_id)
        from datetime import datetime, timezone
        now = datetime.now(timezone.utc)
        eligible = user.get("enabled", True) and (user.get("quota_bytes") is None or user.get("usage_bytes", 0) < user["quota_bytes"])
        if user.get("expires_at"):
            eligible = eligible and datetime.fromisoformat(user["expires_at"].replace("Z", "+00:00")) > now
        identities = [i for i in user.get("managed_certificates", []) if not i.get("archived")]
        def ready(identity):
            return any(v["released"] and not v["discarded"] and v["ca_version"] == identity["published_ca_version"]
                and datetime.fromisoformat(v["not_before"].replace("Z", "+00:00")) <= now
                and datetime.fromisoformat(v["expires_at"].replace("Z", "+00:00")) > now for v in identity["versions"])
        if not eligible or all(ready(i) for i in identities):
            return receipt
        if time.monotonic() >= deadline or any(i.get("maintenance_error") for i in identities):
            raise RuntimeError("fixture managed identity did not become ready")
        time.sleep(0.1)


def request_group(raw_request, base, token, user_ids, node_ids, name=None):
    """Return the accepted receipt, then await its applied fixture relations."""
    def expect(path, method="GET", payload=None, statuses=(200,)):
        status, body = raw_request(base, path, token, method, payload)
        if status not in statuses:
            raise RuntimeError(f"{method} {path}: HTTP {status}")
        return json.loads(body) if body else None
    path = "/api/v2/authorization-groups"
    created = expect(path, "POST", {"name": name or "Live grant " + uuid.uuid4().hex[:12]}, (202,))
    wait_authorization_job(expect, created)
    if not user_ids and not node_ids:
        return 202, json.dumps(created).encode()
    patch = {"expected_revision": 1, "add_user_ids": list(user_ids), "add_node_ids": list(node_ids)}
    receipt = expect(path + "/" + created["resource_id"], "PATCH", patch, (202,))
    wait_authorization_job(expect, receipt)
    return 202, json.dumps(receipt).encode()


def mark_fixture_nodes_deployed(connection, node_ids):
    """Clone already indexed desired snapshots in the caller's isolated fixture.

    This simulates a deployed snapshot for renderer/sampler tests, not successful
    SSH installation. Both reference projections commit with the source update;
    no trigger or constraint is disabled and no maintenance bypass is enabled.
    """
    node_ids = sorted(set(node_ids))
    rows = connection.execute(
        "SELECT id FROM nodes WHERE id=ANY(%s) ORDER BY id FOR UPDATE", (node_ids,)
    ).fetchall()
    if len(rows) != len(node_ids):
        raise ValueError("fixture node is missing")
    if connection.execute(
        "SELECT EXISTS(SELECT 1 FROM credential_reference_dirty WHERE kind='node' AND source_id=ANY(%s))",
        (node_ids,),
    ).fetchone()[0]:
        raise ValueError("fixture desired references must already be indexed")
    connection.execute(
        "UPDATE nodes SET deployed_revision=desired_revision, deployed_config_enc=desired_config_enc, "
        "state='deployed' WHERE id=ANY(%s)", (node_ids,)
    )
    projections = (
        ("credential_reference_edges", "field,credential_id,version"),
        ("config_resource_references", "resource_id"),
    )
    for table, fields in projections:
        connection.execute(
            f"DELETE FROM {table} WHERE kind='node' AND source_id=ANY(%s) AND source='deployed'",
            (node_ids,),
        )
        connection.execute(
            f"INSERT INTO {table}(kind,source_id,node_id,config_version_id,config_revision,source,{fields}) "
            f"SELECT kind,source_id,node_id,config_version_id,config_revision,'deployed',{fields} "
            f"FROM {table} WHERE kind='node' AND source_id=ANY(%s) AND source='desired'",
            (node_ids,),
        )
    pending = connection.execute(
        "SELECT source_id,generation FROM credential_reference_dirty WHERE kind='node' AND source_id=ANY(%s)",
        (node_ids,),
    ).fetchall()
    for node, generation in pending:
        connection.execute(
            "DELETE FROM credential_reference_dirty WHERE kind='node' AND source_id=%s AND generation=%s",
            (node, generation),
        )
