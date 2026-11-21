#!/usr/bin/env python3
"""Rehearse a history retention upgrade on a stopped, isolated test database."""

import argparse
import json
import os
from pathlib import Path
import subprocess

import psycopg
from psycopg import sql


QUEUES = (
    "analytics_dirty_cycles",
    "traffic_dirty_hours",
    "online_dirty_minutes",
    "job_retry_state_dirty",
    "credential_reference_dirty",
)
HISTORY_TABLES = (
    "traffic_sample_items",
    "traffic_records",
    "node_network_samples",
    "traffic_sample_batches",
    "job_events",
    "audit_records",
    "management_receipts",
)


def contract(connection, expected_database, target):
    database, version = connection.execute(
        "SELECT current_database(), schema_version FROM runtime_contract WHERE id"
    ).fetchone()
    if database != expected_database or version not in (target - 1, target):
        raise RuntimeError(
            f"expected the named and initialized schema {target - 1} or {target} test database"
        )
    return version


def history_counts(connection):
    return {
        table: connection.execute(
            sql.SQL("SELECT count(*) FROM {}").format(sql.Identifier(table))
        ).fetchone()[0]
        for table in HISTORY_TABLES
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--expected-database", required=True)
    parser.add_argument("--confirm-test-environment", required=True, action="store_true")
    parser.add_argument(
        "--target-schema", type=int, choices=(12, 13), default=12,
        help="expected schema of the supplied release binary (default: 12)",
    )
    args = parser.parse_args()
    url = os.environ.get("TEST_DATABASE_URL")
    if not url or not os.environ.get("HYSTERIAX_MASTER_KEY"):
        parser.error("TEST_DATABASE_URL and HYSTERIAX_MASTER_KEY are required")

    with psycopg.connect(url, options="-c default_transaction_read_only=on") as connection:
        version_before = contract(connection, args.expected_database, args.target_schema)
        clients = connection.execute(
            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() "
            "AND pid<>pg_backend_pid() AND backend_type='client backend'"
        ).fetchone()[0]
        if clients:
            raise RuntimeError("stop API replicas and other test database clients before migration")
        before = history_counts(connection)

    subprocess.run(
        [args.binary.resolve(), "--migrate-only"],
        env=dict(os.environ, DATABASE_URL=url),
        check=True,
    )

    with psycopg.connect(url, options="-c default_transaction_read_only=on") as connection:
        if contract(connection, args.expected_database, args.target_schema) != args.target_schema:
            raise RuntimeError(f"schema {args.target_schema} runtime contract was not recorded")
        if args.target_schema == 13:
            ledger_trigger = connection.execute(
                "SELECT EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid='traffic_records'::regclass "
                "AND tgname='ledger_deleted' AND tgoldtable='removed_records' AND tgenabled='O')"
            ).fetchone()[0]
            if not ledger_trigger:
                raise RuntimeError("ledger deletion trigger is missing or disabled")
            claim_index = connection.execute(
                "SELECT EXISTS(SELECT 1 FROM pg_index WHERE indisvalid "
                "AND indexrelid=to_regclass('background_work_sampling_claim_idx'))"
            ).fetchone()[0]
            if not claim_index:
                raise RuntimeError("sampling claim index is missing or invalid")
        if connection.execute("SELECT to_regclass('dirty_key_versions')").fetchone()[0] is not None:
            raise RuntimeError("old dirty version directory still exists")
        for queue in QUEUES:
            sequence = queue + "_revision_seq"
            if connection.execute("SELECT to_regclass(%s)", (sequence,)).fetchone()[0] is None:
                raise RuntimeError(f"missing sequence for {queue}")
        after = history_counts(connection)
        if after != before:
            raise RuntimeError("history row counts changed during migrate-only")
    print(json.dumps({
        "schema_before": version_before,
        "schema_after": args.target_schema,
        "rows": after,
    }, indent=2))


if __name__ == "__main__":
    main()
