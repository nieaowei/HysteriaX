#!/usr/bin/env python3
"""Audit an offline PG custom snapshot through explicit maintenance migration.

Uses a new disposable database only. Never starts ordinary workers or contacts
snapshot nodes/providers. The private JSON key file contains HYSTERIAX_MASTER_KEY.
Only hashes, table/column names and counts are written to the public report.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from urllib.parse import quote
import uuid

import psycopg
from psycopg import sql
from snapshot_migration_checks import prepare, check


def fingerprint(connection, original=None):
    result = {}
    tables = original.keys() if original else [row[0] for row in connection.execute("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")]
    for table in tables:
        columns = [row[0] for row in connection.execute("SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name=%s ORDER BY ordinal_position",(table,))]
        selected = original[table]['columns'] if original else columns
        if not columns or set(selected)-set(columns):
            result[table] = {'status':'table-missing' if not columns else 'columns-missing','missing_columns':sorted(set(selected)-set(columns))}
            continue
        query = sql.SQL("SELECT encode(sha256(convert_to(to_jsonb(r)::text,'UTF8')),'hex') FROM (SELECT {} FROM {}) r ORDER BY 1").format(sql.SQL(',').join(map(sql.Identifier,selected)),sql.Identifier('public',table))
        count, digest = 0, hashlib.sha256()
        with connection.transaction():
            connection.execute("SET LOCAL statement_timeout='5min'; SET LOCAL work_mem='16MB'; SET LOCAL TIME ZONE 'UTC'")
            with connection.cursor(name='fingerprint_'+uuid.uuid4().hex) as cursor:
                cursor.execute(query)
                while batch := cursor.fetchmany(5000):
                    for row in batch:
                        digest.update(row[0].encode()+b'\n')
                    count += len(batch)
        result[table] = {'columns':selected,'rows':count,'sha256':digest.hexdigest()}
        print('fingerprinted',table,count,flush=True)
    return result


def file_sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda:source.read(1024*1024),b''):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--snapshot',type=Path,required=True)
    parser.add_argument('--expected-sha256',required=True)
    parser.add_argument('--key-file',type=Path,required=True)
    parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--pg-container',default='hx-uitest-pg')
    parser.add_argument('--server',type=Path,default=Path(__file__).resolve().parents[1]/'target/debug/hysteriax-server')
    args = parser.parse_args()
    if args.key_file.stat().st_mode & 0o077:
        parser.error('key file must be readable only by its owner')
    if file_sha(args.snapshot)!=args.expected_sha256:
        parser.error('snapshot checksum mismatch')
    key = json.loads(args.key_file.read_text())['HYSTERIAX_MASTER_KEY']
    pg = json.loads(subprocess.check_output(['docker','inspect',args.pg_container]))[0]
    settings = dict(value.split('=',1) for value in pg['Config']['Env'] if '=' in value)
    user = settings.get('POSTGRES_USER','postgres')
    port = pg['NetworkSettings']['Ports']['5432/tcp'][0]['HostPort']
    params = {'host':'127.0.0.1','port':port,'user':user,'password':settings['POSTGRES_PASSWORD'],'connect_timeout':5}
    database = 'hx0034_snapshot_'+uuid.uuid4().hex
    url = 'postgres://{}:{}@127.0.0.1:{}/{}'.format(quote(user,safe=''),quote(params['password'],safe=''),port,database)
    logs = Path(tempfile.mkdtemp(prefix='hx0034-migration-audit-'))
    report = {'scope':'offline-snapshot-maintenance-and-fidelity','passed':False,'ordinary_workers_started':False,
              'snapshot_sha256':args.expected_sha256,'private_logs':str(logs),'server_sha256':file_sha(args.server)}
    with psycopg.connect(**params,dbname=settings.get('POSTGRES_DB',user),autocommit=True) as admin:
        admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(database)))
        try:
            descriptor = os.open(logs/'restore.log',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
            with os.fdopen(descriptor,'wb') as output,args.snapshot.open('rb') as source:
                restored = subprocess.run(['docker','exec','-i',args.pg_container,'pg_restore','-U',user,'-d',database,'--exit-on-error','--no-owner','--no-acl'],stdin=source,stdout=output,stderr=subprocess.STDOUT,timeout=1200)
            if restored.returncode:
                raise RuntimeError('snapshot restore failed; inspect private logs')
            with psycopg.connect(**params,dbname=database,autocommit=True) as connection:
                connection.execute("SET TIME ZONE 'UTC'")
                report['source_schema'] = connection.execute('SELECT max(version) FROM _sqlx_migrations').fetchone()[0]
                before = fingerprint(connection)
                schema = prepare(connection)
            descriptor = os.open(logs/'migration.log',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
            started = time.monotonic()
            with os.fdopen(descriptor,'wb') as output:
                migrated = subprocess.run([str(args.server.resolve()),'--migrate-only'],env={**os.environ,'DATABASE_URL':url,'HYSTERIAX_MASTER_KEY':key,'RUST_LOG':'warn'},stdout=output,stderr=subprocess.STDOUT,timeout=1200)
            report['maintenance_seconds'] = time.monotonic()-started
            if migrated.returncode:
                raise RuntimeError('maintenance migration failed; inspect private logs')
            with psycopg.connect(**params,dbname=database,autocommit=True) as connection:
                connection.execute("SET TIME ZONE 'UTC'; SET statement_timeout='5min'; SET work_mem='16MB'")
                report['target_schema'] = connection.execute('SELECT max(version) FROM _sqlx_migrations').fetchone()[0]
                after = fingerprint(connection,before)
                changed = {table:{'before':before[table],'after':after[table]} for table in before if before[table]!=after[table]}
                report.update(unchanged_tables=[table for table in before if table not in changed],changes=changed,
                              fingerprints_before=before,fingerprints_after=after)
                report['reviewed_changes'] = check(connection,schema,changed)
            report['passed'] = True
        except Exception as error:
            report['error_type'] = type(error).__name__
            # Report only controlled check labels, never DB/credential error text.
            if isinstance(error,AssertionError):report['failed_check'] = str(error)
        finally:
            admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(database)))
            args.report.parent.mkdir(parents=True,exist_ok=True)
            args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({'passed':report['passed'],'error_type':report.get('error_type'),'failed_check':report.get('failed_check'),
                      'unchanged_table_count':len(report.get('unchanged_tables',[])),'changed_tables':list(report.get('changes',{})),
                      'reviewed_changes':report.get('reviewed_changes')},indent=2))
    if not report['passed']:
        raise SystemExit(1)


if __name__=='__main__':
    main()
