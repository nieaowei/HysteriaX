#!/usr/bin/env python3
"""Migrate a stopped, restored schema 16 test database to schema 17 and verify HOT-blocking index removal."""

import argparse
import json
import os
from pathlib import Path
import subprocess

import psycopg


def schema_version(connection, expected_database):
    database, version = connection.execute(
        "SELECT current_database(), schema_version FROM runtime_contract WHERE id"
    ).fetchone()
    if database != expected_database or version not in (16, 17):
        raise RuntimeError("expected an initialized schema 16 or 17 test database")
    return version


def users_count(connection):
    return connection.execute("SELECT count(*) FROM users").fetchone()[0]


def verify_schema_17(connection):
    exists = connection.execute(
        """
        SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE schemaname = current_schema()
              AND indexname = 'users_enabled_exhausted_idx'
        )
        """
    ).fetchone()[0]
    if exists:
        raise RuntimeError("users_enabled_exhausted_idx still exists in schema 17")


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
        before_users = users_count(connection)

    env = os.environ.copy()
    env["DATABASE_URL"] = args.database_url
    env["HYSTERIAX_MASTER_KEY"] = args.master_key
    subprocess.run([str(binary), "--migrate-only"], env=env, check=True)

    with psycopg.connect(args.database_url) as connection:
        after_version = schema_version(connection, args.expected_database)
        if after_version != 17:
            raise RuntimeError("schema 17 runtime contract was not recorded")
        after_users = users_count(connection)
        if after_users != before_users:
            raise RuntimeError(f"unexpected users row count drift: before={before_users} after={after_users}")
        verify_schema_17(connection)

    print(
        json.dumps(
            {
                "before_schema": before_version,
                "after_schema": after_version,
                "users_count": after_users,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
