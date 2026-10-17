#!/usr/bin/env python3
"""Verify a pre-upgrade copy against --migrate-only, including destructive mTLS reset.

Set HYSTERIAX_MIGRATION_BEFORE_URL, HYSTERIAX_MIGRATION_AFTER_URL and
HYSTERIAX_MASTER_KEY. Read-only; prints counts, never credential values. Supports
both inline legacy SSH/resources and the previous credential-center schema.
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
        old.execute("SET TRANSACTION READ ONLY")
        new.execute("SET TRANSACTION READ ONLY")
        def tables(connection):
            return {r["tablename"] for r in connection.execute("SELECT tablename FROM pg_tables WHERE schemaname=current_schema()")}
        before_tables = tables(old)
        def rows(connection, table):
            return connection.execute(sql.SQL("SELECT * FROM {}").format(sql.Identifier(table))).fetchall()
        old_credentials = {r["id"]:r for r in rows(old,"credentials")} if "credentials" in before_tables else {}
        credentials = {r["id"]:r for r in rows(new,"credentials")}
        old_versions = {(r["credential_id"],r["version"]):r for r in rows(old,"credential_versions")} if "credential_versions" in before_tables else {}
        versions = {(r["credential_id"],r["version"]):r for r in rows(new,"credential_versions")}
        old_resources = {(r["node_id"],r["id"]):r for r in rows(old,"config_resources")}
        new_resources = {(r["node_id"],r["id"]):r for r in rows(new,"config_resources")}
        def payload(version_map, identity, version):
            return json.loads(decrypt(version_map[(identity,version)]["payload_enc"]))
        def normalize(value, node, version_map, credential_map, resource_map):
            # Resolve in memory only. Assertion failures never render these values.
            if isinstance(value,str) and value.startswith("credential://"):
                identity,version,field = value.removeprefix("credential://").split("/")
                material = payload(version_map,identity,int(version))
                return material["config"][field] if credential_map[identity]["kind"]=="dns" else material[field]
            if isinstance(value,str) and value.startswith("resource://"):
                resource = resource_map[(node,value.removeprefix("resource://"))]
                if resource["resource_kind"] in ("certificate","private_key","ech_key"):
                    return decrypt(resource["content_enc"]).decode()
                return value
            if isinstance(value,dict): return {k:normalize(v,node,version_map,credential_map,resource_map) for k,v in value.items() if k!="legacy_mtls_reset"}
            if isinstance(value,list): return [normalize(v,node,version_map,credential_map,resource_map) for v in value]
            return value
        def strip_legacy(value):
            value = json.loads(json.dumps(value))
            config = value.get("server_config",value)
            ca = config.get("tls",{}).get("clientCA")
            ca_present = "clientCA" in config.get("tls",{})
            mode = config.get("mtls",{}).get("mode")
            legacy = "mtls" in config and mode not in ("off","managed")
            enabled = (ca is not None and ca!="") or (legacy and mode not in (None,"disabled"))
            if ca_present: config["tls"].pop("clientCA")
            if legacy or enabled: config["mtls"]={"mode":"off"}
            return value,enabled
        before_nodes = {r["id"]:r for r in rows(old,"nodes")}
        after_nodes = {r["id"]:r for r in rows(new,"nodes")}
        assert before_nodes.keys()==after_nodes.keys(), "node identities changed"
        blocked = set()
        for identity,before in before_nodes.items():
            after = after_nodes[identity]
            current = payload(versions,after["ssh_credential_id"],after["ssh_credential_version"])
            if "ssh_secret_enc" in before:
                expected = {"secret":decrypt(before["ssh_secret_enc"]).decode()}
                if before["ssh_passphrase_enc"] is not None: expected["passphrase"]=decrypt(before["ssh_passphrase_enc"]).decode()
            else: expected = payload(old_versions,before["ssh_credential_id"],before["ssh_credential_version"])
            assert current==expected, "retained SSH credential changed"
            for field in before.keys() & after.keys():
                if field in ("desired_config_enc","deployed_config_enc"):
                    if before[field] is None:
                        assert after[field] is None, "unexpected deployed config appeared"
                    else:
                        original,enabled = strip_legacy(json.loads(decrypt(before[field])))
                        if enabled: blocked.add(identity)
                        upgraded = json.loads(decrypt(after[field]))
                        assert normalize(original,identity,old_versions,old_credentials,old_resources)==normalize(upgraded,identity,versions,credentials,new_resources), "non-mTLS node config material changed"
                elif field in ("ssh_credential_id","ssh_credential_version") and "ssh_secret_enc" in before:
                    # The inline-to-vault conversion creates these references;
                    # their exact secret/passphrase and kind are checked above.
                    continue
                elif not field.startswith("mtls_"):
                    assert before[field]==after[field], "retained node metadata changed: "+field
            if identity in blocked:
                assert after["mtls_state"]=="needs_configuration" and not after["mtls_recovery_configured"], "legacy active node was not blocked"
            else: assert after["mtls_state"]==before.get("mtls_state","ready"), "history-only node access changed"
        # Exact preservation of business rows; derived caches, projections and
        # intentionally cancelled jobs are checked separately below.
        preserve = {"admin_tokens","users","subscription_credentials","authorization_groups","authorization_group_users","authorization_group_nodes","traffic_records","traffic_baselines","data_gaps","node_packages","node_usage_periods","node_usage_samples","node_online_samples","traffic_poll_runs"}
        def normalized(records, fields):
            return sorted(json.dumps({k:r[k] for k in fields},sort_keys=True,default=str) for r in records)
        for table in sorted(preserve & before_tables):
            before,after = rows(old,table),rows(new,table)
            assert normalized(before,sorted(before[0]) if before else [])==normalized(after,sorted(before[0]) if before else []), "business rows changed: "+table
        assignments = {(r["user_id"],r["node_id"]):r for r in rows(new,"node_assignments")}
        old_assignments = rows(old,"node_assignments")
        assert len(assignments)==len(old_assignments), "authorization pairs changed"
        for before in old_assignments:
            after = assignments[(before["user_id"],before["node_id"])]
            ignored = {"client_certificate_enc","client_private_key_enc","mtls_credential_id","mtls_credential_version"}
            for field in before.keys() & after.keys() - ignored:
                assert before[field]==after[field], "retained connection credential or assignment changed"
            assert after["mtls_credential_id"] is None and after["mtls_credential_version"] is None, "old user binding remains"
            if before["node_id"] in blocked:
                kick = new.execute("SELECT reasons,state FROM kick_requests WHERE node_id=%s AND user_id=%s",(before["node_id"],before["user_id"])).fetchone()
                assert kick and kick["reasons"].get("legacy_mtls_reset"), "offline disconnect obligation is missing"
        snapshots = {r["id"]:r for r in rows(new,"config_versions")}
        old_snapshots = rows(old,"config_versions")
        assert len(snapshots)==len(old_snapshots), "snapshot identities changed"
        for before in old_snapshots:
            after = snapshots[before["id"]]
            original,enabled = strip_legacy(json.loads(decrypt(before["config_enc"])))
            plain = decrypt(after["config_enc"])
            upgraded = json.loads(plain)
            assert normalize(original,before["node_id"],old_versions,old_credentials,old_resources)==normalize(upgraded,before["node_id"],versions,credentials,new_resources), "non-mTLS historical material changed"
            assert hashlib.sha256(plain).hexdigest()==after["content_sha256"], "snapshot digest is invalid"
            if enabled: assert upgraded.get("legacy_mtls_reset") is True and not after["deployed_success"], "old mTLS rollback is still enabled"
        jobs = {r["id"]:r for r in rows(new,"jobs")}
        for before in rows(old,"jobs"):
            after = jobs[before["id"]]
            if after["stage"]=="legacy_mtls_reset":
                if before["status"] in ("queued","running","waiting_confirmation"): assert after["status"]=="cancelled", "old mTLS job can still run"
                assert after["payload_json"]=={} and after["result_json"] is None and after["logs_json"]==[], "old job material was not sanitized"
            elif before["kind"]=="kick" and before["node_id"] in blocked and before["status"]=="queued":
                for field in before.keys()-{"payload_json","updated_at"}:
                    assert before[field]==after[field], "existing disconnect job changed unexpectedly"
                assert after["payload_json"]["kick_reasons"].get("legacy_mtls_reset"), "legacy reset was not merged into existing disconnect"
                for reason in before["payload_json"].get("kick_reasons",{}):
                    assert after["payload_json"]["kick_reasons"].get(reason), "existing disconnect reason was lost"
            else: assert before==after, "unrelated job changed"
        for identity,before in old_credentials.items():
            # Standalone TLS consolidation and the explicitly removed old user
            # client identities have different preservation rules.
            if before["kind"] in ("certificate","private_key"): continue
            if before["kind"]=="tls_identity" and before["owner_user_id"] is not None:
                if identity not in credentials: continue
            assert identity in credentials, "unrelated credential disappeared"
            for key,version in old_versions.items():
                if key[0]==identity: assert versions.get(key)==version, "retained immutable material changed"
        ordinary = lambda mapping: sorted((r for r in mapping.values() if r["resource_kind"] not in ("certificate","private_key","ech_key")),key=lambda r:r["id"])
        assert ordinary(old_resources)==ordinary(new_resources), "ordinary resources changed"
        assert not any(r["resource_kind"] in ("certificate","private_key","ech_key") for r in new_resources.values()), "legacy secret resources remain"
        assert new.execute("SELECT 1 FROM credential_migrations WHERE name='managed_mtls_reset_v1'").fetchone(), "reset marker missing"
        assert not new.execute("SELECT 1 FROM authorization_group_previews LIMIT 1").fetchone(), "old authorization previews remain"
        if "mtls_user_identities" not in before_tables:
            assert not new.execute("SELECT 1 FROM mtls_user_identities LIMIT 1").fetchone(), "old users were converted into new CA identities"
        print(f"Managed mTLS migration verified: {len(before_nodes)} nodes, {len(blocked)} requiring configuration, {len(assignments)} preserved authorizations, {len(snapshots)} snapshots; business state preserved and old client material reset.")


if __name__ == "__main__": main()
