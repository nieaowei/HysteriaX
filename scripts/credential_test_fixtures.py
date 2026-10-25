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


def grant_group(expect, user_id, node_ids, name=None, *, api_prefix="/api/v2"):
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
    status,body = raw_request(base,"/api/v2/authorization-groups/preview",token,"POST",draft)
    if status!=200:return status,body
    draft.pop("action")
    draft["preview_token"]=json.loads(body)["preview_token"]
    return raw_request(base,"/api/v2/authorization-groups",token,"POST",draft)


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
