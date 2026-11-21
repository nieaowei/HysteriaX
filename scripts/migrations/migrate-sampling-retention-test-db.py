#!/usr/bin/env python3
"""Rehearse schema 10 -> 11 on a stopped, restored test database."""

import argparse
import json
import os
from pathlib import Path
import subprocess

import psycopg
from psycopg import sql


ROOT = Path(__file__).resolve().parents[2]
QUERIES = ROOT / "crates/hysteriax-server/src/background"
INDEXES = {
    "background_work_retention_idx",
    "sampling_cycles_retention_idx",
    "execution_operations_resource_idx",
    "traffic_sample_batches_unsettled_cycle_idx",
}


def schema_version(connection, expected_database):
    database, version = connection.execute(
        "SELECT current_database(),schema_version FROM runtime_contract WHERE id"
    ).fetchone()
    if database != expected_database or version not in (10, 11):
        raise RuntimeError("expected an initialized schema 10 or 11 test database")
    return version


def inspect_page(connection, kind):
    page = (QUERIES / f"retention_{kind}_page.sql").read_text()
    connection.execute(sql.SQL("PREPARE retention_page AS " + page))
    statement = "EXECUTE retention_page(now()-interval '14 days', NULL, NULL, 512)"
    ids = [row[1] for row in connection.execute(statement)]
    plan = connection.execute("EXPLAIN (FORMAT JSON) " + statement).fetchone()[0][0]["Plan"]
    connection.execute("DEALLOCATE retention_page")
    if not ids:
        return {"scanned": 0, "eligible": 0, "plan": plan}
    rules = (QUERIES / f"retention_{'work' if kind == 'work' else 'cycles'}.sql").read_text()
    connection.execute(sql.SQL("PREPARE retention_candidates AS " + rules))
    arguments = sql.SQL("{}, now()-interval '14 days'").format(sql.Literal(ids))
    if kind == "work":
        arguments += sql.SQL(", false, ARRAY[]::text[], ARRAY[]::text[]")
    rows = connection.execute(
        sql.SQL("EXECUTE retention_candidates({})").format(arguments)
    ).fetchall()
    connection.execute("DEALLOCATE retention_candidates")
    return {"scanned": len(ids), "eligible": sum(row[1] is None for row in rows), "plan": plan}


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
        clients = connection.execute(
            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() "
            "AND pid<>pg_backend_pid() AND backend_type='client backend'"
        ).fetchone()[0]
        if clients:
            raise RuntimeError("stop the API and other test database clients before migration")
    if before == 10:
        subprocess.run(
            [args.binary.resolve(), "--migrate-only"],
            env=dict(os.environ, DATABASE_URL=url),
            check=True,
        )
    with psycopg.connect(url, options="-c default_transaction_read_only=on") as connection:
        after = schema_version(connection, args.expected_database)
        if after != 11:
            raise RuntimeError("schema 11 runtime contract was not recorded")
        indexes = {row[0] for row in connection.execute(
            "SELECT c.relname FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid "
            "JOIN pg_namespace n ON n.oid=c.relnamespace "
            "WHERE n.nspname=current_schema() AND i.indisvalid AND i.indisready"
        )}
        if not INDEXES <= indexes:
            raise RuntimeError("required retention indexes are missing or invalid")
        result = {
            "schema_before": before,
            "schema_after": after,
            "indexes": sorted(INDEXES),
            "work": inspect_page(connection, "work"),
            "cycles": inspect_page(connection, "cycle"),
        }
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
