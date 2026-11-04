#!/usr/bin/env python3
"""Exercise the one-time upgrade, rollback, replay and real server readiness."""
import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid
from urllib.parse import urlsplit

from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parents[1]


def start(binary, environment, directory, label):
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    log = (directory / (label + '.log')).open('wb')
    process = subprocess.Popen([str(binary)], env=dict(environment, HYSTERIAX_LISTEN_ADDR=f'127.0.0.1:{port}'),
                               cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(label + ' server exited; inspect private logs')
            try:
                with urllib.request.urlopen(origin + '/readyz', timeout=2) as response:
                    if response.status == 200:
                        return process, log, origin
            except OSError:
                pass
            time.sleep(.1)
        raise RuntimeError(label + ' readiness deadline')
    except BaseException:
        stop(process, log)
        raise


def stop(process, log):
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
    log.close()


def request(origin, token, path, payload=None):
    headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json', 'Idempotency-Key': str(uuid.uuid4())}
    req = urllib.request.Request(origin + path, headers=headers,
                                 data=None if payload is None else json.dumps(payload).encode())
    with urllib.request.urlopen(req, timeout=10) as response:
        return response.status, json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before-server', type=Path, required=True)
    parser.add_argument('--after-server', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    private = Path(tempfile.mkdtemp(prefix='hysteriax-lock-migration-'))
    token = secrets.token_urlsafe(32)
    with PostgresTestSchema() as database:
        environment = dict(os.environ, DATABASE_URL=database.url,
                           HYSTERIAX_MASTER_KEY=base64.b64encode(secrets.token_bytes(32)).decode().rstrip('='),
                           HYSTERIAX_ADMIN_TOKEN=token, RUST_LOG='warn')
        environment.pop('HYSTERIAX_REDIS_URL', None)
        with (private / 'initialize.log').open('wb') as log:
            subprocess.run([str(args.before_server.resolve()), '--migrate-only'], cwd=ROOT, env=environment,
                           stdout=log, stderr=subprocess.STDOUT, check=True, timeout=120)
        process, log, origin = start(args.before_server.resolve(), environment, private, 'before')
        try:
            for index in range(3):
                status, _ = request(origin, token, '/api/v2/users', {'name': 'migration-user-' + str(index)})
                assert status == 201
        finally:
            stop(process, log)
        with database.connect() as connection:
            assert connection.execute('SELECT max(version) FROM _sqlx_migrations').fetchone()[0] == 9
            connection.execute("UPDATE users SET usage_bytes=17")
            connection.execute("CREATE FUNCTION hysteriax_package_schedule_changed() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$")
        def migrate(label, success):
            report = private / (label + '.json')
            with (private / (label + '.log')).open('wb') as log:
                result = subprocess.run([sys.executable, str(ROOT / 'scripts/migrate-lock-contention-test-db.py'),
                                         '--expected-database', urlsplit(database.url).path.lstrip('/'),
                                         '--confirm-test-environment', '--confirm-api-stopped', '--report', str(report)],
                                        env=dict(environment, TEST_DATABASE_URL=database.url), cwd=ROOT,
                                        stdout=log, stderr=subprocess.STDOUT, timeout=120)
            assert (result.returncode == 0) == success, label + ': inspect ' + str(private)
            return json.loads(report.read_text()) if success else None
        migrate('injected-ddl-failure', False)
        with database.connect() as connection:
            assert connection.execute('SELECT max(version) FROM _sqlx_migrations').fetchone()[0] == 9
            assert not connection.execute("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='node_packages' AND column_name='next_evaluation_at')").fetchone()[0]
            connection.execute('DROP FUNCTION hysteriax_package_schedule_changed()')
        upgraded = migrate('upgrade', True)
        repeated = migrate('repeat', True)
        assert upgraded['business_values_preserved'] and repeated['repeat']
        assert upgraded['tables'] == repeated['tables']
        process, log, origin = start(args.after_server.resolve(), environment, private, 'after')
        try:
            status, _ = request(origin, token, '/api/v2/users?page_size=20')
            assert status == 200
        finally:
            stop(process, log)
        result = {'passed': True, 'schema_before': 9, 'schema_after': 10,
                  'ddl_failure_rolled_back': True, 'repeat_safe': True,
                  'business_values_preserved': True, 'real_server_ready': True,
                  'production_changed': False, 'private_evidence': str(private)}
    args.report.write_text(json.dumps(result, indent=2))
    args.report.chmod(0o600)
    print(json.dumps(result))


if __name__ == '__main__':
    main()
