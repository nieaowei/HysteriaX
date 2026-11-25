#!/usr/bin/env python3
"""Migrate a stopped, restored schema 15 test database to schema 16 and verify scale indexes."""

import argparse
import json
import os
from pathlib import Path
import subprocess

import psycopg


CREATED_INDEXES = {
    "node_assignments_node_user_idx",
    "credentials_owner_user_idx",
    "traffic_baselines_user_node_idx",
    "nodes_mtls_activation_idx",
    "mtls_deployment_snapshots_activation_idx",
    "mtls_node_operations_base_activation_idx",
    "mtls_node_operations_rotation_idx",
    "mtls_node_operations_node_idx",
    "mtls_cleanup_nodes_node_idx",
    "mtls_rotation_nodes_node_idx",
    "users_enabled_expires_idx",
    "users_enabled_exhausted_idx",
}

DROPPED_INDEXES = {
    "monitoring_probe_tokens_expiry_idx",
    "traffic_sample_batches_capture_deadline",
}

TABLES = (
    "users",
    "nodes",
    "node_assignments",
    "credentials",
    "traffic_baselines",
    "node_network_samples",
)


def schema_version(connection, expected_database):
    database, version = connection.execute(
        "SELECT current_database(), schema_version FROM runtime_contract WHERE id"
    ).fetchone()
    if database != expected_database or version not in (15, 16):
        raise RuntimeError("expected an initialized schema 15 or 16 test database")
    return version


def table_counts(connection):
    return {
        table: connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
        for table in TABLES
    }


def verify_schema_16(connection):
    indexes = {
        row[0]
        for row in connection.execute(
            "SELECT indexname FROM pg_indexes WHERE schemaname = current_schema()"
        ).fetchall()
    }
    missing = sorted(CREATED_INDEXES - indexes)
    if missing:
        raise RuntimeError(f"missing schema 16 indexes: {missing}")
    lingering = sorted(DROPPED_INDEXES & indexes)
    if lingering:
        raise RuntimeError(f"dropped duplicate indexes still exist: {lingering}")

    triggers = {
        row[0]: row[1]
        for row in connection.execute(
            """
            SELECT t.tgname, pg_get_triggerdef(t.oid)
            FROM pg_trigger t
            JOIN pg_class c ON c.oid = t.tgrelid
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = current_schema()
              AND c.relname = 'node_network_samples'
              AND NOT t.tgisinternal
            """
        ).fetchall()
    }
    if "network_deleted" not in triggers or "FOR EACH STATEMENT" not in triggers["network_deleted"]:
        raise RuntimeError("node_network_samples is missing statement-level network_deleted trigger")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database-url", required=True)
    parser.add_argument("--expected-database", required=True)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--master-key", required=True)
    parser.add_argument("--confirm-stopped-test-db", action="store_true")
    args = parser.parse_args()
    if not args.confirm_stopped_test_db:
        raise SystemExit("pass --confirm-stopped-test-db after stopping all API instances")

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        raise SystemExit(f"binary not found: {binary}")

    with psycopg.connect(args.database_url) as connection:
        before_version = schema_version(connection, args.expected_database)
        before_counts = table_counts(connection)

    env = os.environ.copy()
    env["DATABASE_URL"] = args.database_url
    env["HYSTERIAX_MASTER_KEY"] = args.master_key
    subprocess.run([str(binary), "--migrate-only"], env=env, check=True)

    with psycopg.connect(args.database_url) as connection:
        after_version = schema_version(connection, args.expected_database)
        if after_version != 16:
            raise RuntimeError("schema 16 runtime contract was not recorded")
        after_counts = table_counts(connection)
        if after_counts != before_counts:
            raise RuntimeError(f"unexpected row count drift: before={before_counts} after={after_counts}")
        verify_schema_16(connection)

    print(
        json.dumps(
            {
                "before_schema": before_version,
                "after_schema": after_version,
                "counts": after_counts,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
