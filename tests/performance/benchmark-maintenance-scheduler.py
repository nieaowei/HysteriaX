#!/usr/bin/env python3
"""Compare frozen baseline/candidate workers on isolated, identically seeded PG fixtures.

The Rust background_contention_comparison test needs the same test-only pool and
summary instrumentation in both binaries. SSH connects only to loopback port 1;
this measures worker DB overhead, not successful collection or service capacity.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
TEST = 'credentials::retention::tests::benchmarks::background_contention_comparison'


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fields(line):
    values = {}
    for name, value in re.findall(r'\b([a-z_]+)=("[^"]*"|[^\s]+)', line):
        if value.startswith('"'):
            values[name] = value[1:-1]
        else:
            try:
                values[name] = int(value)
            except ValueError:
                values[name] = value
    return values


def window(text, first, last):
    assert text.count(first) == text.count(last) == 1
    result = {'polls': {}, 'transactions': {}}
    for line in text.split(first, 1)[1].split(last, 1)[0].splitlines():
        values = fields(line)
        if 'maintenance poll summary' in line:
            result['polls'][(values['kind'], values['operation'])] = values
        elif 'database transaction summary' in line:
            result['transactions'][(values['operation'], values['isolation'])] = values
    return result


def analyze(text):
    start = window(text, 'BACKGROUND_WINDOW_START', 'BACKGROUND_WINDOW_OPEN')
    end = window(text, 'BACKGROUND_WINDOW_END', 'BACKGROUND_WINDOW_CLOSED')
    deltas = {}
    for category in start:
        deltas[category] = []
        for key, final in end[category].items():
            before = start[category].get(key, {})
            delta = {'key': list(key)}
            for name, value in final.items():
                if isinstance(value, int):
                    delta[name] = value - before.get(name, 0)
            deltas[category].append(delta)
    reports = [json.loads(line.split('BACKGROUND_BENCHMARK ', 1)[1])
               for line in text.splitlines() if 'BACKGROUND_BENCHMARK ' in line]
    assert len(reports) == 1
    polls, transactions = deltas['polls'], deltas['transactions']
    result = {'fixture': reports[0], 'window_seconds': reports[0]['seconds'] - reports[0]['warmup_seconds'],
              'actual_empty_preflights': sum(x.get('preflight_empty', 0) for x in polls),
              'coalesced_empty': sum(x.get('coalesced_empty', 0) for x in polls),
              'maintenance_calls_needing_db': sum(x.get('calls', 0) - x.get('coalesced_empty', 0)
                  for x in polls if x['key'][1] in ('ensure', 'claim', 'probe')),
              'transaction_calls': sum(x.get('calls', 0) for x in transactions),
              'transaction_wall_seconds': sum(x.get('duration_us', 0) for x in transactions) / 1e6,
              'poll_failures': sum(x.get('failed', 0) for x in polls),
              'transaction_failures': sum(x.get('failed', 0) for x in transactions),
              'panics': text.count('panicked at'),
              'quota_listener_reconnects': text.count('quota leaderboard listener reconnecting'), 'details': deltas}
    assert result['panics'] == 0
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--container', help='Reuse a labeled fixture container; never removed by this script')
    parser.add_argument('--redis', action='store_true', help='Include an isolated Redis and the quota rank listener')
    parser.add_argument('--seconds', type=int, default=120)
    parser.add_argument('--warmup', type=int, default=30)
    parser.add_argument('--pool', type=int, default=100)
    parser.add_argument('--rounds', type=int, default=2)
    args = parser.parse_args()
    assert 90 <= args.seconds <= 600 and 0 < args.warmup < args.seconds
    assert 4 <= args.pool <= 100 and 1 <= args.rounds <= 5
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    binaries = {'baseline': args.baseline.resolve(), 'candidate': args.candidate.resolve()}
    hashes = {name: digest(path) for name, path in binaries.items()}
    assert binaries['baseline'] != binaries['candidate'], 'Freeze each binary to a separate path'
    assert hashes['baseline'] != hashes['candidate'], 'Baseline and candidate binaries are identical'
    controller_hash = digest(Path(__file__))
    (output / 'inputs.json').write_text(json.dumps({'binaries': hashes, 'controller': controller_hash,
        'seconds': args.seconds, 'warmup': args.warmup, 'pool': args.pool,
        'rounds': args.rounds, 'redis_enabled': args.redis, 'host': platform.platform(),
        'scope': '6 fixture nodes, 260k historical work, 240k leases, 25ms DB response delay; failed loopback SSH is intentional; not capacity acceptance'}, indent=2) + '\n')
    owned = args.container is None
    container = args.container or 'hx-maintenance-scheduler-' + uuid.uuid4().hex[:10]
    if owned:
        command('docker', 'run', '--detach', '--name', container,
                '--label', 'hysteriax.test=maintenance-coalescing', '-e', 'POSTGRES_PASSWORD=fixture',
                '-e', 'POSTGRES_DB=hysteriax_maintenance', '-p', '127.0.0.1::5432',
                '--tmpfs', '/var/lib/postgresql:rw,size=8g', 'postgres:18.6-alpine',
                '-c', 'max_connections=240', '-c', 'shared_buffers=256MB')
    identifier = command('docker', 'inspect', '--format', '{{.Id}}', container)
    label = command('docker', 'inspect', '--format', '{{index .Config.Labels "hysteriax.test"}}', identifier)
    assert label.startswith('maintenance-coalescing'), 'Refusing an unowned database container'
    redis_id = None
    cases = []
    try:
        if args.redis:
            redis_container = 'hx-maintenance-redis-' + uuid.uuid4().hex[:10]
            command('docker', 'run', '--detach', '--name', redis_container,
                    '--label', 'hysteriax.test=maintenance-coalescing-redis', '-p', '127.0.0.1::6379',
                    'redis:8.6.1-alpine', 'redis-server', '--save', '', '--appendonly', 'no')
            redis_id = command('docker', 'inspect', '--format', '{{.Id}}', redis_container)
            redis_bindings = json.loads(command('docker', 'inspect', '--format', '{{json .NetworkSettings.Ports}}', redis_id))['6379/tcp']
            assert len(redis_bindings) == 1 and redis_bindings[0]['HostIp'] == '127.0.0.1'
            redis_url = 'redis://127.0.0.1:' + redis_bindings[0]['HostPort'] + '/'

        for _ in range(100):
            if subprocess.run(['docker', 'exec', identifier, 'pg_isready', '-U', 'postgres'], capture_output=True).returncode == 0:
                break
            time.sleep(.1)
        else:
            raise RuntimeError('Fixture database did not become ready')
        bindings = json.loads(command('docker', 'inspect', '--format', '{{json .NetworkSettings.Ports}}', identifier))['5432/tcp']
        assert len(bindings) == 1 and bindings[0]['HostIp'] == '127.0.0.1'
        port = int(bindings[0]['HostPort'])
        env = {k: v for k, v in os.environ.items() if not k.startswith(('HYSTERIAX_', 'HX_', 'PG'))
               and k not in ('DATABASE_URL', 'TEST_DATABASE_URL', 'REDIS_URL', 'TEST_REDIS_URL')}
        env.update(TEST_DATABASE_URL=f'postgres://postgres:fixture@127.0.0.1:{port}/hysteriax_maintenance?sslmode=disable',
            HYSTERIAX_DB_POOL_MAX_CONNECTIONS=str(args.pool), HYSTERIAX_BACKGROUND_WORKER_PARALLELISM='16',
            HYSTERIAX_SAMPLING_INTERVAL_SECONDS='60', HYSTERIAX_EXECUTION_LEASE_SECONDS='180',
            HYSTERIAX_OPERATION_TIMEOUT_SECONDS='60', HYSTERIAX_DB_STATEMENT_TIMEOUT_SECONDS='60',
            HYSTERIAX_DB_IDLE_TRANSACTION_TIMEOUT_SECONDS='60', HX_MAINTENANCE_BENCH_POOL=str(args.pool),
            RUST_MIN_STACK='16777216',
            HX_MAINTENANCE_BENCH_SECONDS=str(args.seconds), HX_MAINTENANCE_BENCH_WARMUP=str(args.warmup))
        if args.redis:
            env['HYSTERIAX_REDIS_URL'] = redis_url
        for round_index in range(args.rounds):
            order = ['baseline', 'candidate'] if round_index % 2 == 0 else ['candidate', 'baseline']
            for name in order:
                assert digest(Path(__file__)) == controller_hash
                assert digest(binaries[name]) == hashes[name]
                case_name = f'{round_index + 1}-{name}'
                print('Starting ' + case_name, flush=True)
                with (output / (case_name + '.log')).open('w') as log:
                    process = subprocess.Popen([str(binaries[name]), '--ignored', '--exact', TEST,
                        '--nocapture', '--test-threads=1'], cwd=ROOT, env=env,
                        stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                    try:
                        code = process.wait(timeout=args.seconds + 240)
                    finally:
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        process.wait()
                assert digest(Path(__file__)) == controller_hash
                assert digest(binaries[name]) == hashes[name]
                assert code == 0, case_name + ' failed; retain log'
                result = dict(analyze((output / (case_name + '.log')).read_text()), name=case_name)
                assert result['fixture']['pool_max_connections'] == args.pool
                if args.redis:
                    assert result['quota_listener_reconnects'] == 0, 'Quota rank listener did not remain healthy'
                cases.append(result)
                (output / 'results.json').write_text(json.dumps({'hashes': hashes, 'cases': cases}, indent=2) + '\n')
                print(json.dumps({k: v for k, v in result.items() if k != 'details'}), flush=True)
                # Delete only this successfully reported fixture schema. Keep
                # unexpected DB failures available for inspection.
                schema = result['fixture']['schema']
                assert schema.startswith('hysteriax_test_') and re.fullmatch(r'[a-zA-Z0-9_]+', schema)
                if result['poll_failures'] == result['transaction_failures'] == 0:
                    command('docker', 'exec', identifier, 'psql', '-U', 'postgres', '-d',
                            'hysteriax_maintenance', '-v', 'ON_ERROR_STOP=1', '-qc',
                            f'DROP SCHEMA {schema} CASCADE')
    finally:
        if redis_id is not None:
            assert command('docker', 'inspect', '--format', '{{index .Config.Labels "hysteriax.test"}}', redis_id) == 'maintenance-coalescing-redis'
            command('docker', 'rm', '--force', '--volumes', redis_id)
        if owned:
            assert command('docker', 'inspect', '--format', '{{index .Config.Labels "hysteriax.test"}}', identifier) == label
            command('docker', 'rm', '--force', '--volumes', identifier)
            (output / 'cleanup.json').write_text(json.dumps({'container_id': identifier, 'removed': True}) + '\n')


if __name__ == '__main__':
    main()
