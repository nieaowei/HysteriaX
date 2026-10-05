#!/usr/bin/env python3
"""Compare an untouched database copy with a --migrate-only copy, without workers.

Set HYSTERIAX_MIGRATION_BEFORE_URL, HYSTERIAX_MIGRATION_AFTER_URL and
HYSTERIAX_MASTER_KEY. Output contains counts only, never credential values.
"""
import base64
import hashlib
import json
import os

import psycopg
from psycopg.rows import dict_row
from psycopg import sql
from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_decrypt


def decode(value):
    return base64.b64decode(value + "=" * (-len(value) % 4))


def main():
    key = decode(os.environ["HYSTERIAX_MASTER_KEY"])
    assert len(key) == 32, "invalid migration verification key"

    def decrypt(cipher):
        data = decode(cipher)
        return crypto_aead_xchacha20poly1305_ietf_decrypt(data[24:], None, data[:24], key)

    with psycopg.connect(os.environ["HYSTERIAX_MIGRATION_BEFORE_URL"], row_factory=dict_row) as old, \
         psycopg.connect(os.environ["HYSTERIAX_MIGRATION_AFTER_URL"], row_factory=dict_row) as new:
        credentials = {row["id"]: row for row in new.execute("SELECT * FROM credentials")}
        versions = {(row["credential_id"], row["version"]): row for row in new.execute("SELECT * FROM credential_versions")}

        def payload(identity, version):
            return json.loads(decrypt(versions[(identity, version)]["payload_enc"]))

        def original_config(value):
            if isinstance(value, str) and value.startswith("credential://"):
                identity, version, field = value.removeprefix("credential://").split("/")
                record = versions[(identity, int(version))]
                if credentials[identity]["kind"] == "dns":
                    return payload(identity, int(version))["config"][field]
                return "resource://" + record["artifact_names"][field]
            if isinstance(value, dict):
                return {k: original_config(v) for k, v in value.items()}
            if isinstance(value, list):
                return [original_config(v) for v in value]
            return value

        nodes_before = {row["id"]: row for row in old.execute("SELECT * FROM nodes")}
        nodes_after = {row["id"]: row for row in new.execute("SELECT * FROM nodes")}
        assert nodes_before.keys() == nodes_after.keys(), "node identities changed"
        for identity, before in nodes_before.items():
            after = nodes_after[identity]
            material = payload(after["ssh_credential_id"], after["ssh_credential_version"])
            assert material["secret"].encode() == decrypt(before["ssh_secret_enc"]), "SSH secret changed"
            phrase = decrypt(before["ssh_passphrase_enc"]).decode() if before["ssh_passphrase_enc"] is not None else None
            assert material.get("passphrase") == phrase, "SSH passphrase changed"
            expected_kind = "ssh_password" if before["ssh_auth_type"] == "password" else "ssh_private_key"
            assert credentials[after["ssh_credential_id"]]["kind"] == expected_kind, "SSH authentication kind changed"
            for field in before.keys() & after.keys():
                if field in ("desired_config_enc", "deployed_config_enc"):
                    if before[field] is None:
                        assert after[field] is None, "deployed configuration appeared unexpectedly"
                    else:
                        assert json.loads(decrypt(before[field])) == original_config(json.loads(decrypt(after[field]))), "node configuration changed"
                else:
                    assert before[field] == after[field], f"node {field} changed"

        # Business credentials, hashes, revocations, quotas, counters and cursors
        # remain identical; schema changes only remove assignment PEM columns.
        existing = {r["tablename"] for r in old.execute("SELECT tablename FROM pg_tables WHERE schemaname=current_schema()")}
        tables = sorted(existing - {"nodes", "config_versions", "config_resources", "_sqlx_migrations"})
        for table in tables:
            before = old.execute(sql.SQL("SELECT * FROM {}").format(sql.Identifier(table))).fetchall()
            after = new.execute(sql.SQL("SELECT * FROM {}").format(sql.Identifier(table))).fetchall()
            assert len(before) == len(after), f"{table} row count changed"
            if not before:
                continue
            common = sorted(before[0].keys() & after[0].keys())
            normalized = lambda rows: sorted(json.dumps({k: r[k] for k in common}, sort_keys=True, default=str) for r in rows)
            assert normalized(before) == normalized(after), f"{table} values changed"
        assignments = {(r["user_id"], r["node_id"]): r for r in new.execute("SELECT * FROM node_assignments")}
        for before in old.execute("SELECT * FROM node_assignments"):
            after = assignments[(before["user_id"], before["node_id"])]
            if before["client_certificate_enc"] is not None:
                material = payload(after["mtls_credential_id"], after["mtls_credential_version"])
                assert material["certificate"].encode() == decrypt(before["client_certificate_enc"]), "mTLS certificate changed"
                assert material["private_key"].encode() == decrypt(before["client_private_key_enc"]), "mTLS private key changed"
                assert credentials[after["mtls_credential_id"]]["owner_user_id"] == before["user_id"], "mTLS ownership changed"
        snapshots = {r["id"]: r for r in new.execute("SELECT * FROM config_versions")}
        count = 0
        for before in old.execute("SELECT * FROM config_versions"):
            after = snapshots[before["id"]]
            plain = decrypt(after["config_enc"])
            assert json.loads(decrypt(before["config_enc"])) == original_config(json.loads(plain)), "historical configuration changed"
            assert hashlib.sha256(plain).hexdigest() == after["content_sha256"], "historical snapshot digest is invalid"
            for field in before.keys() & after.keys() - {"config_enc", "content_sha256"}:
                assert before[field] == after[field], "historical snapshot metadata changed"
            count += 1
        resources = old.execute("SELECT * FROM config_resources WHERE resource_kind IN ('certificate','private_key','ech_key')").fetchall()
        for resource in resources:
            matches = [(r,field) for r in versions.values() for field,filename in r["artifact_names"].items() if filename == resource["id"]]
            if not matches and resource["resource_kind"] in ("certificate","private_key"):
                # The explicit destructive consolidation drops unreferenced old
                # standalone material. Referenced material must still be pinned.
                uri = "resource://" + resource["id"]
                source_configs = [json.loads(decrypt(n[f])) for n in nodes_before.values() for f in ("desired_config_enc","deployed_config_enc") if n[f] is not None]
                source_configs += [json.loads(decrypt(r["config_enc"])) for r in old.execute("SELECT config_enc FROM config_versions")]
                assert not any(uri in json.dumps(c) for c in source_configs), "referenced legacy material disappeared"
                continue
            assert matches, "legacy resource mapping is missing"
            for version,field in matches:
                assert payload(version["credential_id"], version["version"])[field].encode() == decrypt(resource["content_enc"]), "resource bytes changed"
        ordinary_before=old.execute("SELECT * FROM config_resources WHERE resource_kind NOT IN ('certificate','private_key','ech_key') ORDER BY id").fetchall()
        ordinary_after=new.execute("SELECT * FROM config_resources ORDER BY id").fetchall()
        assert ordinary_before==ordinary_after, "ordinary config resources changed"
        assert not new.execute("SELECT 1 FROM config_resources WHERE resource_kind IN ('certificate','private_key','ech_key')").fetchone(), "legacy secret resources remain"
        print(f"Migration preservation passed: {len(nodes_before)} nodes, {len(assignments)} assignments, {count} pinned snapshots, {len(resources)} secret resources; tokens, quotas, counters and jobs unchanged.")


if __name__ == "__main__":
    main()
