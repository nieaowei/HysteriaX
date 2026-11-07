#!/usr/bin/env python3
"""Compare real sampling paths at explicit test-only queue concurrency limits.

Requires a frozen Rust test executable and native Hysteria v2.12.3 executable.
All cases use 100k users/10k nodes/1m assignments, a 32-connection application
pool, real loopback SSH and QUIC, and synthetic traffic/network observations.
Creates an owned, disposable PostgreSQL container unless --container is supplied.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
TEST = 'background::sampling_benchmark::sampling_concurrency'


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--probe-binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--container', help='Reuse an owned sampling-concurrency test container; never removed by this script')
    parser.add_argument('--nodes', default='6,64', help='Active nodes; stored nodes are always 10000')
    parser.add_argument('--parallelism', default='4,8,16,32')
    parser.add_argument('--rounds', type=int, default=2, help='Alternate forward/reverse limit order')
    parser.add_argument('--delay-ms', type=int, default=25)
    args = parser.parse_args()
    nodes = [int(s) for s in args.nodes.split(',')]
    limits = [int(s) for s in args.parallelism.split(',')]
    if not all(1 <= n <= 10000 for n in nodes) or not all(1 <= n <= 64 for n in limits):
        parser.error('nodes must be 1..10000; parallelism 1..64')
    if not 1 <= args.rounds <= 10 or not 0 <= args.delay_ms <= 100:
        parser.error('rounds must be 1..10; delay-ms 0..100')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    binary, probe = args.binary.resolve(), args.probe_binary.resolve()
    hashes = {'test_binary': digest(binary), 'probe_binary': digest(probe),
              'controller': digest(Path(__file__))}
    sources = {str(p.relative_to(ROOT)): digest(p) for p in sorted((ROOT/'crates').rglob('*'))
               if p.is_file() and p.suffix in ('.rs', '.sql', '.toml', '.lua')}
    (output/'source-inputs.json').write_text(json.dumps(sources, indent=2)+'\n')
    (output/'inputs.json').write_text(json.dumps({**hashes, 'nodes': nodes, 'limits': limits,
        'rounds': args.rounds, 'db_response_delay_ms': args.delay_ms,
        'host': platform.platform(), 'logical_cpus': os.cpu_count(),
        'docker_capacity': command('docker', 'info', '--format', '{{.NCPU}} CPUs; {{.MemTotal}} bytes'),
        'scope': 'finite cohort; synthetic counters; shared local Hysteria server; simplified fixture auth; not production capacity acceptance'}, indent=2)+'\n')
    owned = args.container is None
    container = args.container or 'hx-sampling-concurrency-'+uuid.uuid4().hex[:10]
    if owned:
        command('docker', 'run', '--detach', '--name', container,
                '--label', 'hysteriax.test=sampling-concurrency',
                '-e', 'POSTGRES_PASSWORD=fixture', '-e', 'POSTGRES_DB=hysteriax_sampling',
                '-p', '127.0.0.1::5432', '--tmpfs', '/var/lib/postgresql:rw,size=3g',
                'postgres:18.6-alpine', '-c', 'max_connections=180',
                '-c', 'shared_buffers=256MB', '-c', 'fsync=on', '-c', 'synchronous_commit=on')
    identifier = command('docker', 'inspect', '--format', '{{.Id}}', container)
    label = command('docker', 'inspect', '--format', '{{index .Config.Labels "hysteriax.test"}}', identifier)
    if not label.startswith('sampling-concurrency'):
        raise RuntimeError('Refusing to use an unowned container')
    cases = []
    try:
        for _ in range(100):
            if subprocess.run(['docker','exec',identifier,'pg_isready','-U','postgres'], capture_output=True).returncode == 0:
                break
            time.sleep(.1)
        else:
            raise RuntimeError('Disposable PostgreSQL did not become ready')
        bindings = json.loads(command('docker','inspect','--format','{{json .NetworkSettings.Ports}}',identifier))['5432/tcp']
        if len(bindings) != 1 or bindings[0]['HostIp'] != '127.0.0.1':
            raise RuntimeError('Database must be bound only to loopback')
        port = int(bindings[0]['HostPort'])
        environment = {k:v for k,v in os.environ.items() if not k.startswith(('HYSTERIAX_', 'HX_', 'PG'))
                       and k not in ('DATABASE_URL','TEST_DATABASE_URL','REDIS_URL','TEST_REDIS_URL')}
        environment.update(TEST_DATABASE_URL=f'postgres://postgres:fixture@127.0.0.1:{port}/hysteriax_sampling?sslmode=disable',
            HYSTERIAX_PROBE_BINARY=str(probe), HYSTERIAX_SAMPLING_INTERVAL_SECONDS='60',
            HYSTERIAX_EXECUTION_LEASE_SECONDS='180', HYSTERIAX_OPERATION_TIMEOUT_SECONDS='60',
            HX_SAMPLING_DB_DELAY_MS=str(args.delay_ms), HX_SAMPLING_OUTPUT=str(output),
            RUST_MIN_STACK='16777216', GOMAXPROCS='2')
        for active in nodes:
            for round_index in range(args.rounds):
                order = limits if round_index % 2 == 0 else list(reversed(limits))
                for limit in order:
                    name = f'n{active}-p{limit}-r{round_index+1}'
                    env = dict(environment, HX_SAMPLING_PARALLEL=str(limit), HX_SAMPLING_ACTIVE_NODES=str(active))
                    print(f'Starting {name}', flush=True)
                    started = time.monotonic()
                    with (output/(name+'.log')).open('w') as log:
                        process = subprocess.Popen([str(binary),'--ignored','--exact',TEST,'--nocapture','--test-threads=1'],
                            cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
                        try:
                            code = process.wait(timeout=360)
                        finally:
                            # This group contains only this experiment's child
                            # and its temporary Hysteria server/client processes.
                            try:
                                os.killpg(process.pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                            process.wait()
                    if code:
                        raise RuntimeError(f'{name} failed; inspect retained log')
                    reports = [json.loads(line.split('SAMPLING_CONCURRENCY ',1)[1])
                        for line in (output/(name+'.log')).read_text().splitlines() if 'SAMPLING_CONCURRENCY ' in line]
                    if len(reports) != 1:
                        raise RuntimeError('Expected exactly one cohort report')
                    case = dict(reports[0], name=name, round=round_index+1, total_seconds=time.monotonic()-started)
                    accounting = case['accounting']
                    case['integrity_ok'] = (accounting['bytes'] == accounting['record_bytes']
                        == accounting['records'] * 150 and accounting['duplicate_records'] == 0
                        and accounting['captured'] == accounting['complete_batches']
                        and accounting['records'] == accounting['complete_batches'] * 100
                        and accounting['uncertain_gaps'] == 0 and accounting['active_leases'] == 0
                        and all(v == 1 for v in case['clear_requests'].values()))
                    log_text = (output/(name+'.log')).read_text()
                    case['warnings'] = {key: log_text.count(term) for key,term in {
                        'renewal_lost':'execution renewal lost:', 'execution_stopped':'execution stopped',
                        'slow_acquire':'slow to acquire a connection', 'pool_failure':'benchmark pool acquisition failed',
                        'claim_failure':'benchmark claim failed'}.items()}
                    if digest(binary) != hashes['test_binary'] or digest(probe) != hashes['probe_binary']:
                        raise RuntimeError('Frozen binary changed')
                    cases.append(case)
                    (output/(name+'.json')).write_text(json.dumps(case,indent=2)+'\n')
                    (output/'results.json').write_text(json.dumps({'hashes':hashes,'cases':cases},indent=2)+'\n')
                    print(json.dumps({k:case[k] for k in ('name','elapsed_seconds','states','accounting','pool_acquire','warnings')},ensure_ascii=False),flush=True)
                    if active <= 6 and limit == 4 and (accounting['probe_ok'] != active or not case['integrity_ok']):
                        raise RuntimeError('Small baseline must prove successful real proxies and accounting integrity')
                    # Only delete the schema reported by this successful child.
                    schema=case['schema']
                    if not schema.startswith('hysteriax_test_') or not schema.replace('_','').isalnum():
                        raise RuntimeError('Invalid fixture schema')
                    subprocess.run(['docker','exec',identifier,'psql','-U','postgres','-d','hysteriax_sampling',
                        '-q','-c',f'DROP SCHEMA {schema} CASCADE'],check=True,capture_output=True)
    finally:
        if owned:
            current = command('docker','inspect','--format','{{index .Config.Labels "hysteriax.test"}}',identifier)
            if current == label:
                subprocess.run(['docker','rm','--force','--volumes',identifier],check=True,capture_output=True)
                (output/'cleanup.json').write_text(json.dumps({'container_id':identifier,'removed':True})+'\n')


if __name__ == '__main__':
    main()
