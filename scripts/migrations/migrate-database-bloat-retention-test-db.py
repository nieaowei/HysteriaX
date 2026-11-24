#!/usr/bin/env python3
"""Migrate a stopped, restored schema 14 test database to schema 15 and verify bloat controls."""

import argparse
import json
import os
from pathlib import Path
import subprocess

import psycopg


DROPPED_INDEXES = {
    "online_samples_node_cleanup_idx",
    "online_samples_node_time_idx",
    "online_samples_retention_idx",
    "proxy_probe_samples_node_cleanup_idx",
    "proxy_probe_samples_node_time_idx",
    "proxy_probe_samples_retention_idx",
    "traffic_records_node_time_idx",
    "node_network_samples_node_idx",
}

SHARED_LATEST_INDEXES = {
    "online_samples_node_latest_idx",
    "proxy_probe_samples_node_latest_idx",
}

CREATED_INDEXES = {
    "online_samples_history_idx",
    "proxy_probe_samples_history_idx",
    "traffic_sample_batches_discarded_history_idx",
    "traffic_baselines_history_idx",
    "monitoring_probe_tokens_history_idx",
    "execution_operations_verified_history_idx",
    "execution_recheck_requests_history_idx",
    "background_work_non_sampling_retention_idx",
    "resource_leases_released_idx",
    "cache_entity_versions_tombstone_idx",
    "management_receipts_history_idx",
    "mtls_operations_history_idx",
    "kick_requests_history_idx",
    "credential_application_receipts_history_idx",
    "credential_batches_history_idx",
    "dns_operations_history_idx",
    "dns_operations_resource_idx",
    "dns_bindings_record_ids_idx",
    "dns_records_deleted_history_idx",
    "user_credentials_revoked_history_idx",
    "config_versions_history_idx",
    "mtls_node_operations_history_idx",
    "mtls_rotations_history_idx",
    "mtls_trust_cleanup_history_idx",
    "mtls_deployment_snapshots_history_idx",
    "mtls_activations_history_idx",
    "mtls_activations_previous_idx",
}

HISTORY_TABLES = (
    "online_samples",
    "proxy_probe_samples",
    "traffic_sample_batches",
    "traffic_baselines",
    "monitoring_probe_tokens",
    "execution_operations",
    "execution_rechecks",
    "execution_recheck_requests",
    "kick_requests",
    "management_receipts",
    "mtls_operations",
    "cache_entity_versions",
    "job_node_kick_totals",
    "user_credentials",
    "credential_application_receipts",
    "credential_batches",
    "dns_operations",
    "dns_records",
    "mtls_node_operations",
    "mtls_rotations",
    "mtls_trust_cleanup",
    "mtls_deployment_snapshots",
    "mtls_activations",
    "config_versions",
)

EXPECTED_RELOPTIONS = {
    "traffic_records": "autovacuum_vacuum_scale_factor=0.01",
    "online_samples": "autovacuum_vacuum_scale_factor=0.01",
    "proxy_probe_samples": "autovacuum_vacuum_scale_factor=0.01",
    "cache_bucket_pending": "autovacuum_vacuum_scale_factor=0.0",
    "analytics_dirty_cycles": "autovacuum_vacuum_scale_factor=0.0",
    "nodes": "fillfactor=80",
    "users": "fillfactor=80",
}

LEGACY_WORK_IDS = (
    "maintenance:monitoring-retention:online_samples",
    "maintenance:monitoring-retention:proxy_probe_samples",
    "maintenance:monitoring-retention:monitoring_probe_tokens",
)

LEGACY_LEASE_KEYS = (
    "work:maintenance:monitoring-retention:online_samples",
    "work:maintenance:monitoring-retention:proxy_probe_samples",
    "work:maintenance:monitoring-retention:monitoring_probe_tokens",
)


def schema_version(connection, expected_database):
    database, version = connection.execute(
        "SELECT current_database(), schema_version FROM runtime_contract WHERE id"
    ).fetchone()
    if database != expected_database or version not in (14, 15):
        raise RuntimeError("expected an initialized schema 14 or 15 test database")
    return version


def history_counts(connection):
    return {
        table: connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
        for table in HISTORY_TABLES
    }


def verify_post_migration(connection):
    indexes = {
        row[0]
        for row in connection.execute(
            "SELECT c.relname FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid "
            "JOIN pg_namespace n ON n.oid = c.relnamespace "
            "WHERE n.nspname = current_schema() AND i.indisvalid AND i.indisready"
        )
    }
    present_dropped = sorted(DROPPED_INDEXES & indexes)
    if present_dropped:
        raise RuntimeError(f"redundant hot-table indexes still present: {present_dropped}")
    missing_created = sorted((CREATED_INDEXES | SHARED_LATEST_INDEXES) - indexes)
    if missing_created:
        raise RuntimeError(f"required bloat-retention indexes missing or invalid: {missing_created}")
    probe_latest_def = connection.execute(
        "SELECT pg_get_indexdef(c.oid) FROM pg_class c "
        "JOIN pg_namespace n ON n.oid = c.relnamespace "
        "WHERE n.nspname = current_schema() AND c.relname = 'proxy_probe_samples_node_latest_idx'"
    ).fetchone()[0]
    if "DESC NULLS LAST, id DESC" not in probe_latest_def:
        raise RuntimeError(f"proxy_probe_samples_node_latest_idx missing id DESC: {probe_latest_def}")

    has_updated_at = connection.execute(
        "SELECT EXISTS("
        "  SELECT 1 FROM information_schema.columns "
        "  WHERE table_schema = current_schema() "
        "    AND table_name = 'cache_entity_versions' AND column_name = 'updated_at'"
        ")"
    ).fetchone()[0]
    if not has_updated_at:
        raise RuntimeError("cache_entity_versions.updated_at column is missing")

    triggers = {
        (row[0], row[1]): (row[2], row[3], row[4])
        for row in connection.execute(
            "SELECT c.relname, t.tgname, t.tgenabled::text, (t.tgtype & 1) <> 0, (t.tgtype & 8) <> 0 "
            "FROM pg_trigger t "
            "JOIN pg_class c ON c.oid = t.tgrelid "
            "JOIN pg_namespace n ON n.oid = c.relnamespace "
            "WHERE n.nspname = current_schema() AND NOT t.tgisinternal"
        )
    }
    for table, name in (
        ("proxy_probe_samples", "probe_deleted"),
        ("online_samples", "online_deleted"),
    ):
        info = triggers.get((table, name))
        if not info or info[0] not in ("O", "A") or info[1] or not info[2]:
            raise RuntimeError(f"statement-level AFTER DELETE trigger {table}.{name} invalid: {info}")

    for table, name in (
        ("proxy_probe_samples", "probe_totals"),
        ("online_samples", "cycle_dirty"),
    ):
        info = triggers.get((table, name))
        if not info or not info[1] or info[2]:
            raise RuntimeError(f"row-level trigger {table}.{name} must not fire on DELETE: {info}")

    reloptions = {
        row[0]: set(row[1] or ())
        for row in connection.execute(
            "SELECT c.relname, c.reloptions FROM pg_class c "
            "JOIN pg_namespace n ON n.oid = c.relnamespace "
            "WHERE n.nspname = current_schema() AND c.relname = ANY(%s)",
            (list(EXPECTED_RELOPTIONS),),
        )
    }
    for table, expected_opt in EXPECTED_RELOPTIONS.items():
        if expected_opt not in reloptions.get(table, set()):
            raise RuntimeError(f"expected reloption {expected_opt!r} missing on {table}: {reloptions.get(table)}")

    legacy_work = connection.execute(
        "SELECT count(*) FROM background_work WHERE id = ANY(%s)",
        (list(LEGACY_WORK_IDS),),
    ).fetchone()[0]
    legacy_leases = connection.execute(
        "SELECT count(*) FROM resource_leases WHERE resource_key = ANY(%s)",
        (list(LEGACY_LEASE_KEYS),),
    ).fetchone()[0]
    if legacy_work or legacy_leases:
        raise RuntimeError(
            f"legacy monitoring-retention rows still exist (work={legacy_work}, leases={legacy_leases})"
        )

    return {
        "dropped_indexes_verified_absent": len(DROPPED_INDEXES),
        "created_indexes_verified_valid": len(CREATED_INDEXES),
        "statement_delete_triggers": ["online_samples.online_deleted", "proxy_probe_samples.probe_deleted"],
        "cache_entity_versions_updated_at": True,
        "tuned_tables_verified": sorted(EXPECTED_RELOPTIONS),
        "legacy_monitoring_retention_rows_remaining": 0,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--expected-database", required=True)
    parser.add_argument("--confirm-test-environment", action="store_true", required=True)
    args = parser.parse_args()
    url = os.environ.get("TEST_DATABASE_URL")
    if not url or not os.environ.get("HYSTERIAX_MASTER_KEY"):
        parser.error("TEST_DATABASE_URL and HYSTERIAX_MASTER_KEY are required")

    with psycopg.connect(url) as connection:
        before = schema_version(connection, args.expected_database)
        counts_before = history_counts(connection)
        clients = connection.execute(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() "
            "AND pid <> pg_backend_pid() AND backend_type = 'client backend'"
        ).fetchone()[0]
        if clients:
            raise RuntimeError("stop the API and other test database clients before migration")

    if before == 14:
        subprocess.run(
            [args.binary.resolve(), "--migrate-only"],
            env=dict(os.environ, DATABASE_URL=url),
            check=True,
        )

    with psycopg.connect(url, options="-c default_transaction_read_only=on") as connection:
        after = schema_version(connection, args.expected_database)
        if after != 15:
            raise RuntimeError("schema 15 runtime contract was not recorded")
        counts_after = history_counts(connection)
        if counts_after != counts_before:
            raise RuntimeError("history row counts changed during migrate-only")
        checks = verify_post_migration(connection)

    print(
        json.dumps(
            {
                "schema_before": before,
                "schema_after": after,
                "row_counts_preserved": counts_after,
                **checks,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
