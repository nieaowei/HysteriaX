#!/usr/bin/env python3
"""Verify an existing credential database copy against its TLS-pair migration.

Uses HYSTERIAX_MIGRATION_BEFORE_URL/AFTER_URL and HYSTERIAX_MASTER_KEY.
Reports counts only; never prints credential bytes or database passwords.
"""
import os
import json
import base64
import hashlib
import psycopg
from psycopg.rows import dict_row
from psycopg import sql
from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_decrypt


def main():
    decode = lambda s: base64.b64decode(s + "=" * (-len(s) % 4))
    key = decode(os.environ["HYSTERIAX_MASTER_KEY"])
    def decrypt(s):
        value = decode(s)
        return crypto_aead_xchacha20poly1305_ietf_decrypt(value[24:], None, value[:24], key)
    with psycopg.connect(os.environ["HYSTERIAX_MIGRATION_BEFORE_URL"], row_factory=dict_row) as before, psycopg.connect(os.environ["HYSTERIAX_MIGRATION_AFTER_URL"], row_factory=dict_row) as after:
        def renderer(connection):
            material = {(r["credential_id"], r["version"]): r for r in connection.execute("SELECT v.*,c.kind FROM credential_versions v JOIN credentials c ON c.id=v.credential_id")}
            def runtime(value, files):
                if isinstance(value, str) and value.startswith("credential://"):
                    identity, version, field = value.removeprefix("credential://").split("/")
                    record = material[(identity, int(version))]
                    payload = json.loads(decrypt(record["payload_enc"]))
                    if record["kind"] == "dns": return payload["config"][field]
                    filename = record["artifact_names"].get(field, f"{identity}-{version}-{field}")
                    files[filename] = hashlib.sha256(payload[field].encode()).hexdigest()
                    return "/etc/hysteriax/resources/" + filename
                if isinstance(value, dict): return {k: runtime(v, files) for k,v in value.items()}
                if isinstance(value, list): return [runtime(v, files) for v in value]
                return value
            def render(cipher):
                if cipher is None: return None
                files = {}
                return runtime(json.loads(decrypt(cipher)), files), files
            return render
        old_render, new_render = renderer(before), renderer(after)
        old_nodes = {r["id"]: r for r in before.execute("SELECT * FROM nodes")}
        new_nodes = {r["id"]: r for r in after.execute("SELECT * FROM nodes")}
        assert old_nodes.keys() == new_nodes.keys(), "node IDs changed"
        for identity, old in old_nodes.items():
            new = new_nodes[identity]
            for field in old:
                if field in ("desired_config_enc", "deployed_config_enc"):
                    assert old_render(old[field]) == new_render(new[field]), "runtime node configuration or artifacts changed"
                else: assert old[field] == new[field], f"node {field} changed"
        old_versions = {r["id"]: r for r in before.execute("SELECT * FROM config_versions")}
        new_versions = {r["id"]: r for r in after.execute("SELECT * FROM config_versions")}
        assert old_versions.keys() == new_versions.keys()
        for identity, old in old_versions.items():
            new = new_versions[identity]
            assert old_render(old["config_enc"]) == new_render(new["config_enc"]), "historical runtime or artifacts changed"
            assert hashlib.sha256(decrypt(new["config_enc"])).hexdigest() == new["content_sha256"]
            for field in old.keys() - {"config_enc", "content_sha256"}: assert old[field] == new[field]
        tables = {r["tablename"] for r in before.execute("SELECT tablename FROM pg_tables WHERE schemaname=current_schema()")}
        excluded = {"nodes", "config_versions", "credentials", "credential_versions", "credential_migrations", "_sqlx_migrations", "credential_batches", "credential_batch_items"}
        for table in sorted(tables - excluded):
            query = sql.SQL("SELECT * FROM {}").format(sql.Identifier(table))
            normalize = lambda rows: sorted(json.dumps(r, sort_keys=True, default=str) for r in rows)
            assert normalize(before.execute(query).fetchall()) == normalize(after.execute(query).fetchall()), f"{table} changed"
        assert not after.execute("SELECT 1 FROM credentials WHERE kind IN ('certificate','private_key')").fetchone()
        for node in new_nodes.values():
            for field in ("desired_config_enc", "deployed_config_enc"):
                if node[field] is None: continue
                value = json.loads(decrypt(node[field]))
                if "tls" in value:
                    cert = value["tls"]["cert"].removeprefix("credential://").split("/")
                    private = value["tls"]["key"].removeprefix("credential://").split("/")
                    assert cert[:2] == private[:2] and cert[2] == "certificate" and private[2] == "private_key"
        print(f"TLS pair migration preserved {len(old_nodes)} nodes, {len(old_versions)} snapshots, runtime artifacts and business tables; removed standalone TLS types")


if __name__ == "__main__": main()
