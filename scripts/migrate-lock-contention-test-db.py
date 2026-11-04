#!/usr/bin/env python3
"""One-time, data-preserving schema 9 -> 10 migration for a stopped test DB.

TEST_DATABASE_URL is read from the environment and is never printed. Production
maintenance continues to use the server's --migrate-only entry point.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time

import psycopg
from psycopg import sql

ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = ROOT / 'crates/hysteriax-server/migrations'
BUSINESS = [
    'users', 'nodes', 'node_assignments', 'authorization_groups',
    'authorization_group_users', 'authorization_group_nodes',
    'user_credentials', 'credentials', 'credential_versions',
    'subscription_templates', 'subscription_template_versions',
    'traffic_sample_batches', 'traffic_sample_items', 'traffic_baselines',
    'traffic_records', 'kick_requests', 'jobs', 'data_gaps', 'node_limit_epochs',
]


def table_digest(connection, table, projection='to_jsonb(t)'):
    keys = [row[0] for row in connection.execute(
        'SELECT a.attname FROM pg_index i '
        'CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum,position) '
        'JOIN pg_attribute a ON a.attrelid=i.indrelid AND a.attnum=k.attnum '
        'WHERE i.indrelid=to_regclass(%s) AND i.indisprimary ORDER BY k.position', (table,))]
    if not keys:
        raise RuntimeError('A snapshot table has no stable primary key: ' + table)
    digest = hashlib.sha256()
    count = 0
    # Server cursors keep a million assignments/ledger records out of client
    # memory. Primary-key order avoids sorting entire encrypted JSON values.
    with connection.cursor(name='hx_lock_snapshot_' + table) as cursor:
        cursor.itersize = 1000
        cursor.execute(sql.SQL('SELECT ({})::text FROM {} t ORDER BY {}').format(
            sql.SQL(projection), sql.Identifier(table),
            sql.SQL(',').join(sql.Identifier('t', key) for key in keys)))
        for (row,) in cursor:
            digest.update(row.encode())
            digest.update(b'\n')
            count += 1
    return {'count': count, 'sha256': digest.hexdigest()}


def snapshot(connection):
    result = {table: table_digest(connection, table) for table in BUSINESS}
    # Scheduling metadata is new; all package config and counters must survive.
    result['packages'] = table_digest(connection, 'node_packages', "to_jsonb(t)-'next_evaluation_at'")
    return result


def migrate(connection, expected_database):
    actual, readonly = connection.execute('SELECT current_database(), current_setting(\'transaction_read_only\')').fetchone()
    if actual != expected_database or readonly != 'off':
        raise RuntimeError('Expected writable test database does not match target')
    installed = connection.execute('SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version').fetchall()
    expected = sorted(MIGRATIONS.glob('[0-9][0-9][0-9][0-9]_*.sql'))
    if len(installed) not in (9, 10):
        raise RuntimeError('Only a schema 9 test database or its completed schema 10 migration is supported')
    for (version, success, checksum), path in zip(installed, expected):
        if version != int(path.name[:4]) or not success or bytes(checksum) != hashlib.sha384(path.read_bytes()).digest():
            raise RuntimeError('Migration history/checksum verification failed')
    contract = connection.execute('SELECT schema_version,management_protocol,master_key_fingerprint FROM runtime_contract WHERE id').fetchone()
    if not contract or contract[:2] != (len(installed), 2):
        raise RuntimeError('Initialized runtime contract does not match migration history')
    active = connection.execute("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND backend_type='client backend' AND state<>'idle'").fetchone()[0]
    if active:
        raise RuntimeError('Other active database clients exist; stop every API before migration')
    before = snapshot(connection)
    if len(installed) == 9:
        migration = MIGRATIONS / '0010_lock_contention.sql'
        began = time.perf_counter_ns()
        connection.execute(migration.read_text())
        connection.execute('INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(10,%s,true,%s,%s)',
                           ('lock contention', hashlib.sha384(migration.read_bytes()).digest(), time.perf_counter_ns()-began))
        connection.execute('UPDATE runtime_contract SET schema_version=10,initialized_at=clock_timestamp() WHERE id')
    after = snapshot(connection)
    if before != after:
        raise RuntimeError('Business values changed; rolling back the entire migration')
    final_contract = connection.execute('SELECT schema_version,management_protocol,master_key_fingerprint FROM runtime_contract WHERE id').fetchone()
    if final_contract != (10, 2, contract[2]):
        raise RuntimeError('Runtime contract verification failed')
    due_index = connection.execute("SELECT to_regclass(current_schema()||'.node_packages_evaluation_due_idx') IS NOT NULL").fetchone()[0]
    if not due_index:
        raise RuntimeError('Due index missing')
    return {'schema_before': len(installed), 'schema_after': 10, 'management_protocol': 2,
            'business_values_preserved': True, 'tables': after, 'repeat': len(installed) == 10}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-database', required=True)
    parser.add_argument('--confirm-test-environment', action='store_true', required=True)
    parser.add_argument('--confirm-api-stopped', action='store_true', required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    url = os.environ.get('TEST_DATABASE_URL')
    if not url:
        parser.error('TEST_DATABASE_URL is required')
    # Only non-secret report values leave the transaction. An error rolls back
    # both SQLx metadata and DDL/data changes, so retry is safe.
    with psycopg.connect(url, application_name='hysteriax_test_lock_migration') as connection:
        result = migrate(connection, args.expected_database)
    descriptor = os.open(args.report, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(descriptor, 0o600)
    with os.fdopen(descriptor, 'w') as output:
        json.dump(result, output, indent=2)
    print('Schema 10 verified; business values preserved. Report:', args.report)


if __name__ == '__main__':
    main()
