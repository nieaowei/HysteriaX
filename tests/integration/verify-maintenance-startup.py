#!/usr/bin/env python3
"""Run the complete release image with default thread stacks on owned fixtures."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import time
import uuid


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=330)
    args = parser.parse_args()
    assert 65 <= args.seconds <= 600
    args.output.mkdir(parents=True, exist_ok=False)
    label = 'maintenance-startup-' + uuid.uuid4().hex[:10]
    network = label
    containers = []
    api = None
    result = {'image': args.image, 'runtime_stack_override': False,
              'scope': 'fresh isolated database; full worker startup, 300-second fallback and graceful stop; no node capacity claim'}
    command('docker', 'network', 'create', '--internal', '--label', 'hysteriax.test=' + label, network)

    def run(name, *options):
        identifier = command('docker', 'run', '--detach', '--name', label + '-' + name,
            '--label', 'hysteriax.test=' + label, '--network', network, *options)
        containers.append(identifier)
        return identifier

    try:
        pg = run('pg', '--network-alias', 'pg', '--tmpfs', '/var/lib/postgresql:rw,size=2g',
            '-e', 'POSTGRES_PASSWORD=fixture', '-e', 'POSTGRES_DB=startup', 'postgres:18.6-alpine',
            '-c', 'max_connections=160')
        redis = run('redis', '--network-alias', 'redis', 'redis:8-alpine',
            'redis-server', '--save', '', '--appendonly', 'no')
        for _ in range(120):
            if subprocess.run(['docker', 'exec', pg, 'pg_isready', '-U', 'postgres'], capture_output=True).returncode == 0:
                break
            time.sleep(.25)
        else:
            raise RuntimeError('Fixture PostgreSQL did not become ready')
        environment = {
            'DATABASE_URL': 'postgres://postgres:fixture@pg:5432/startup?sslmode=disable',
            'HYSTERIAX_REDIS_URL': 'redis://redis:6379/0',
            'HYSTERIAX_MASTER_KEY': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
            'HYSTERIAX_ADMIN_TOKEN': 'fixture-administrator-token-for-startup-verification-0000',
            'HYSTERIAX_DB_POOL_MAX_CONNECTIONS': '100',
            'HYSTERIAX_BACKGROUND_WORKER_PARALLELISM': '16',
            'HYSTERIAX_SAMPLING_INTERVAL_SECONDS': '60',
            'HYSTERIAX_EXECUTION_LEASE_SECONDS': '180',
            'HYSTERIAX_DB_LOCK_TIMEOUT_SECONDS': '60',
            'HYSTERIAX_DB_STATEMENT_TIMEOUT_SECONDS': '60',
            'HYSTERIAX_DB_IDLE_TRANSACTION_TIMEOUT_SECONDS': '60',
            'HYSTERIAX_OPERATION_TIMEOUT_SECONDS': '60',
            'HYSTERIAX_ASSET_CACHE_DIR': '/tmp/assets',
            'TOKIO_WORKER_THREADS': '2',
            'RUST_LOG': 'info',
        }
        env_args = [arg for k, v in environment.items() for arg in ('-e', k + '=' + v)]
        init = run('migrate', '--platform', 'linux/amd64', '--read-only', '--tmpfs', '/tmp',
                   *env_args, args.image, '--migrate-only')
        deadline = time.monotonic() + 90
        while json.loads(command('docker', 'inspect', init))[0]['State']['Running']:
            assert time.monotonic() < deadline, 'Fixture migration timed out'
            time.sleep(.5)
        init_state = json.loads(command('docker', 'inspect', init))[0]['State']
        with (args.output / 'migrate.log').open('w') as log:
            subprocess.run(['docker', 'logs', init], stdout=log, stderr=subprocess.STDOUT, check=True)
        assert init_state['ExitCode'] == 0, 'Fixture migration failed'
        api = run('api', '--platform', 'linux/amd64', '--read-only', '--tmpfs', '/tmp',
            '--stop-timeout', '150', *env_args, args.image)
        data = json.loads(command('docker', 'inspect', api))[0]
        assert all(not e.startswith('RUST_MIN_STACK=') for e in data['Config']['Env'])
        started = time.monotonic()
        probes = []
        while time.monotonic() - started < args.seconds:
            state = json.loads(command('docker', 'inspect', api))[0]['State']
            assert state['Running'], 'Release process exited: ' + str(state['ExitCode'])
            at = time.monotonic()
            try:
                response = subprocess.run(['docker', 'exec', api, 'curl', '--fail', '--silent',
                    '--max-time', '5', 'http://127.0.0.1:8080/readyz'], capture_output=True, text=True)
                if response.returncode:
                    raise OSError('Readiness probe failed')
                assert json.loads(response.stdout)['status'] == 'ready'
                probes.append({'seconds': round(at - started, 3), 'ready': True})
            except (OSError, ValueError):
                probes.append({'seconds': round(at - started, 3), 'ready': False})
                assert at - started < 15, 'Readiness lost after startup'
            time.sleep(2)
        assert probes and sum(x['ready'] for x in probes) > 20
        command('docker', 'stop', '--time', '150', api)
        data = json.loads(command('docker', 'inspect', api))[0]
        assert data['State']['ExitCode'] == 0 and data['RestartCount'] == 0, 'Release did not exit cleanly'
        raw = re.sub(r'\x1b\[[0-9;]*m', '', command('docker', 'logs', api))
        assert 'stack overflow' not in raw and 'panicked at' not in raw
        assert raw.count('sampling queue drained') == 3 and 'sampling workers drained' in raw
        assert 'maintenance poll summary' in raw and 'coalesced_empty=' in raw, 'Missing maintenance counters'
        result.update(passed=True, probes=probes, exit_code=0, restart_count=0,
                      graceful_sampling_queues=3, image_id=data['Image'])
    except Exception as error:
        result.update(passed=False, error=str(error) or type(error).__name__)
        raise
    finally:
        for identifier in reversed(containers):
            data = json.loads(command('docker', 'inspect', identifier))[0]
            assert data['Config']['Labels']['hysteriax.test'] == label
            name = data['Name'].removeprefix('/')
            (args.output / (name + '.inspect.json')).write_text(json.dumps(data, indent=2) + '\n')
            with (args.output / (name + '.log')).open('w') as log:
                subprocess.run(['docker', 'logs', identifier], stdout=log, stderr=subprocess.STDOUT)
            command('docker', 'rm', '--force', '--volumes', identifier)
        assert command('docker', 'network', 'inspect', '--format', '{{index .Labels "hysteriax.test"}}', network) == label
        command('docker', 'network', 'rm', network)
        result['owned_fixtures_removed'] = True
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))


if __name__ == '__main__':
    main()
