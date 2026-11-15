#!/usr/bin/env python3
"""Bounded SQLx pool crossover on an existing production Compose installation.

Run on the API host. Uses a previously verified diagnostic image, takes an online
backup, preserves private configs, drains every API transition, and restores the
original image/config in finally. No schema migration or synthetic business write.
"""
import argparse
import concurrent.futures
import datetime as dt
import hashlib
import json
import math
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import threading
import time
import urllib.request
from urllib.parse import unquote, urlsplit

CONFIGS = ['compose.yaml', 'api.env', '.env', 'redis.env', 'deploy/caddy/Caddyfile']
ORIGINAL = 'hysteriax-server:sampling-parallelism16-20261108-r1'
DIAGNOSTIC = 'hysteriax-server:pool-phase-20261108-r1'
POOL_KEY = 'HYSTERIAX_DB_POOL_MAX_CONNECTIONS'


def now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def command(*args, cwd=None, timeout=180):
    p = subprocess.run(args, cwd=cwd, capture_output=True, timeout=timeout)
    if p.returncode:
        # Never echo command/env: configuration and database authentication are private.
        raise RuntimeError('Command failed: ' + args[0] + '; ' + p.stderr.decode()[-500:])
    return p.stdout.decode()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')
    path.chmod(0o600)


def parse_env(raw):
    return {k: v.strip().strip('"').strip("'") for line in raw.splitlines()
            if line and not line.startswith('#') and '=' in line
            for k, v in [line.split('=', 1)]}


def atomic(path, raw):
    temp = path.with_name(path.name + '.pool-comparison-next')
    temp.write_bytes(raw)
    temp.chmod(0o600)
    temp.replace(path)


def activity_query():
    sql = "SELECT json_build_object('at',clock_timestamp(),'sessions',(SELECT coalesce(json_agg(x),'[]'::json) FROM (SELECT pid,backend_start,state,wait_event_type,wait_event,extract(epoch FROM(clock_timestamp()-xact_start)) xact_age_seconds FROM pg_stat_activity WHERE datname=current_database() AND usename=current_user AND pid<>pg_backend_pid()) x))"
    # json_agg(record) emits newlines; one SQL result must be one wire line.
    return "SELECT replace((" + sql[len('SELECT '):] + ")::text,chr(10),' ')"


class Comparison:
    def __init__(self, args):
        self.args, self.root, self.out = args, args.root.resolve(), args.output.resolve()
        self.original = {f: (self.root / f).read_bytes() for f in CONFIGS}
        config = parse_env(self.original['api.env'].decode())
        assert config[POOL_KEY] == '32'
        assert config['HYSTERIAX_BACKGROUND_WORKER_PARALLELISM'] == '16'
        assert config['HYSTERIAX_SAMPLING_INTERVAL_SECONDS'] == '60'
        url = urlsplit(config['DATABASE_URL'])
        self.env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
                        PGUSER=unquote(url.username), PGPASSWORD=unquote(url.password),
                        PGDATABASE=url.path.lstrip('/'), PGSSLMODE='disable',
                        PGCONNECT_TIMEOUT='15', PGTZ='UTC',
                        PGOPTIONS='-c default_transaction_read_only=on -c statement_timeout=15000',
                        PGAPPNAME='hysteriax-pool-crossover-observer')
        self.changed = False

    def query(self, sql):
        p = subprocess.run(['psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1', '-c', sql],
                           env=self.env, capture_output=True, text=True, timeout=40)
        assert p.returncode == 0, 'Read-only database query failed: ' + p.stderr[-500:]
        return p.stdout.strip()

    def rows(self, sql):
        return json.loads(self.query("SELECT coalesce(json_agg(r),'[]'::json) FROM (" + sql + ') r'))

    def inspect(self):
        return json.loads(command('docker', 'inspect', 'hysteriax-api-1',
                                  'hysteriax-caddy-1', 'hysteriax-redis-1'))

    def compose(self, *args):
        return command('docker', 'compose', '-p', 'hysteriax', '-f', str(self.root / 'compose.yaml'),
                       *args, cwd=self.root, timeout=220)

    def gaps(self):
        return self.rows("SELECT id,node_id,opened_at FROM data_gaps WHERE reason='traffic_clear_uncertain' ORDER BY id")

    def current_owner(self):
        sql = "SELECT instance_id FROM service_instances WHERE heartbeat_at>clock_timestamp()-interval '60 seconds'"
        owners = self.rows(sql)
        deadline = time.monotonic() + 75
        while len(owners) > 1 and time.monotonic() < deadline:
            # A recently drained instance retains its last heartbeat briefly.
            time.sleep(2)
            owners = self.rows(sql)
        assert len(owners) == 1
        return owners

    def contract(self):
        return {'runtime': self.rows('SELECT * FROM runtime_contract ORDER BY id'),
                'migrations': self.rows("SELECT version,success,encode(checksum,'hex') checksum FROM _sqlx_migrations ORDER BY version"),
                'counts': {t: int(self.query('SELECT count(*) FROM ' + t)) for t in
                           ['users', 'nodes', 'node_assignments', 'user_credentials',
                            'subscription_templates', 'subscription_template_versions']}}

    def prepare(self):
        data = self.inspect()
        assert data[0]['Config']['Image'] == ORIGINAL and data[0]['State']['Health']['Status'] == 'healthy'
        assert data[0]['Config']['StopTimeout'] == 150 and data[0]['RestartCount'] == 0
        image = json.loads(command('docker', 'image', 'inspect', DIAGNOSTIC))[0]
        assert image['Config']['Labels']['io.hysteriax.diagnostic.sqlx-pool-phase'] == '1'
        assert image['Id'] == 'sha256:10151cc8c9ca30e83628f3d8823d274c9ac4ff78b7bfddb27097c967eb2464b3'
        assert self.original['compose.yaml'].decode().count(ORIGINAL) == 1
        assert POOL_KEY not in self.original['compose.yaml'].decode()
        self.out.mkdir(mode=0o700, parents=True, exist_ok=False)
        for f, raw in self.original.items():
            dest = self.out / 'config-before' / f
            dest.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            atomic(dest, raw)
        self.before, self.initial_contract, self.initial_gaps = data, self.contract(), self.gaps()
        capacity = self.rows("SELECT name,setting FROM pg_settings WHERE name IN ('max_connections','superuser_reserved_connections','reserved_connections') ORDER BY name")
        sessions = int(self.query('SELECT count(*) FROM pg_stat_activity'))
        max_connections = int(next(x['setting'] for x in capacity if x['name'] == 'max_connections'))
        reserved = sum(int(x['setting']) for x in capacity if x['name'] != 'max_connections')
        assert sessions + 40 < max_connections - reserved, 'Insufficient database connection headroom'
        save(self.out / 'preflight.json', {'at': now(), 'services': [{k: d[k] for k in ['Id', 'Image', 'RestartCount', 'State']} for d in data],
             'contract': self.initial_contract, 'gaps': self.initial_gaps, 'capacity': capacity, 'sessions': sessions,
             'diagnostic_image_id': image['Id'], 'sequence': [32, 48, 64, 64, 48, 32],
             'warmup_seconds': self.args.warmup_seconds, 'measurement_minutes': self.args.minutes,
             'controller_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
             'probe': 'local readyz serial 1Hz; one persistent read-only PG observer at 0.2Hz',
             'limits': 'Sequential production observations; natural background load; slow-acquire logs are threshold-biased.'})
        dump = self.out / 'database-before.dump'
        p = subprocess.run(['pg_dump', '--format=custom', '--no-owner', '--no-acl', '--file', str(dump)],
                           env=self.env, capture_output=True, timeout=300)
        assert p.returncode == 0, 'Online database snapshot failed'
        dump.chmod(0o600)
        assert 'TABLE DATA' in command('pg_restore', '--list', str(dump))
        save(self.out / 'backup.json', {'bytes': dump.stat().st_size, 'sha256': hashlib.sha256(dump.read_bytes()).hexdigest(),
                                     'archive_list_verified': True, 'full_restore_rehearsal': False, 'at': now()})
        print(json.dumps({'event': 'prepared', 'at': now(), 'backup_bytes': dump.stat().st_size,
                          'db_max_connections': max_connections, 'db_sessions': sessions}), flush=True)

    def resume_prepared(self):
        """Resume only an untouched, already backed-up preflight, never a partial run."""
        assert not list(self.out.glob('[0-9][0-9]-pool-*')), 'Cannot resume a started crossover'
        p = json.loads((self.out / 'preflight.json').read_text())
        assert all(self.original[f] == (self.out / 'config-before' / f).read_bytes() for f in CONFIGS)
        data = self.inspect()
        assert data[0]['Id'] == p['services'][0]['Id'] and data[0]['Config']['Image'] == ORIGINAL
        assert data[0]['State']['Health']['Status'] == 'healthy'
        assert self.contract() == p['contract']
        backup = json.loads((self.out / 'backup.json').read_text())
        assert hashlib.sha256((self.out / 'database-before.dump').read_bytes()).hexdigest() == backup['sha256']
        self.before, self.initial_contract, self.initial_gaps = data, p['contract'], p['gaps']
        assert {x['id'] for x in self.gaps()} == {x['id'] for x in self.initial_gaps}
        save(self.out / 'resumed-preflight.json', {'at': now(), 'original_api_unchanged': True,
             'backup_sha256': backup['sha256'], 'controller_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})

    def drain(self, directory):
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        d = self.inspect()[0]
        if not d['State']['Running']:
            assert d['State']['ExitCode'] == 0 and not d['State']['OOMKilled']
            save(directory / 'already-stopped.json', {'at': now(), 'api_id': d['Id'], 'exit_code': 0})
            return
        owners = self.current_owner()
        owner = owners[0]['instance_id'].replace("'", "''")
        cohort = self.rows("SELECT sample_id FROM traffic_sample_batches WHERE state='capturing' AND clear_requested_at IS NOT NULL AND payload_digest IS NULL AND clear_owner='" + owner + "'")
        before_gaps = self.gaps()
        start = now()
        save(directory / 'before.json', {'at': start, 'api_id': d['Id'], 'owners': owners, 'capture_cohort': cohort, 'gaps': before_gaps})
        self.compose('stop', '--timeout', '150', 'api')
        stopped = self.inspect()[0]
        assert stopped['Id'] == d['Id'] and not stopped['State']['Running']
        assert stopped['State']['ExitCode'] == 0 and not stopped['State']['OOMKilled']
        raw = command('docker', 'logs', '--since', start, d['Id'])
        # Rust tracing writes stdout; keep stderr too in complete stage logs below.
        (directory / 'drain.log').write_text(raw)
        assert raw.count('sampling queue drained') == 3 and 'sampling workers drained' in raw
        assert 'sampling drain deadline reached' not in raw and 'sampling drain failed' not in raw
        leases = self.rows("SELECT l.resource_key,l.work_id FROM resource_leases l JOIN background_work w ON w.id=l.work_id WHERE l.owner='" + owner + "' AND w.kind IN ('traffic','network','proxy')")
        gaps = self.gaps()
        assert {x['id'] for x in gaps} == {x['id'] for x in before_gaps}, 'New uncertainty gap during transition'
        assert not leases, 'Old sampling owner still holds leases'
        ids = ','.join("'" + x['sample_id'].replace("'", "''") + "'" for x in cohort) or 'NULL'
        captured = self.rows('SELECT sample_id,state,payload_digest IS NOT NULL captured FROM traffic_sample_batches WHERE sample_id IN (' + ids + ')')
        assert len(captured) == len(cohort) and all(x['captured'] and x['state'] in ('ready', 'complete') for x in captured)
        # Save all retained logs before Compose removes the container.
        p = subprocess.run(['docker', 'logs', d['Id']], capture_output=True, timeout=60)
        (directory / 'complete-api.log').write_bytes(p.stdout + p.stderr)
        assert p.returncode == 0
        save(directory / 'after.json', {'at': now(), 'exit_code': 0, 'three_queues_drained': True,
             'old_owner_leases': leases, 'capture_cohort': captured, 'new_uncertain_gaps': []})

    def switch(self, pool, directory, restore=False):
        self.changed = True
        self.drain(directory / 'transition')
        for f in CONFIGS:
            expected = self.original[f]
            if f == 'compose.yaml' and not restore:
                expected = expected.replace(ORIGINAL.encode(), DIAGNOSTIC.encode())
            elif f == 'api.env' and not restore:
                raw, count = re.subn(rb'(?m)^' + POOL_KEY.encode() + rb'=32[ \t]*$',
                                     POOL_KEY.encode() + b'=' + str(pool).encode(), expected)
                assert count == 1
                expected = raw
            if f in ('compose.yaml', 'api.env'):
                atomic(self.root / f, expected)
            else:
                assert (self.root / f).read_bytes() == expected
            dest = directory / 'config' / f
            dest.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            atomic(dest, expected)
        self.compose('up', '-d', '--no-deps', 'api')
        deadline = time.monotonic() + 120
        while True:
            try:
                with urllib.request.urlopen('http://127.0.0.1:8080/readyz', timeout=8) as r:
                    assert r.status == 200 and json.load(r)['status'] == 'ready'
                break
            except Exception:
                assert time.monotonic() < deadline, 'API did not become ready'
                time.sleep(1)
        d = self.inspect()[0]
        env = parse_env('\n'.join(d['Config']['Env']))
        assert env[POOL_KEY] == str(pool) and env['HYSTERIAX_BACKGROUND_WORKER_PARALLELISM'] == '16'
        assert d['Config']['Image'] == (ORIGINAL if restore else DIAGNOSTIC)
        ready = time.time()
        save(directory / 'ready.json', {'at': now(), 'epoch': ready, 'container_id': d['Id'], 'pool': pool,
                                       'image': d['Config']['Image'], 'pid': d['State']['Pid']})
        print(json.dumps({'event': 'ready', 'stage': directory.name, 'pool': pool, 'at': now()}), flush=True)
        return d, ready

    def quality(self, start, end):
        predicate = f'c.period_start>=to_timestamp({start}) AND c.period_start<to_timestamp({end})'
        return {'from_epoch': start, 'to_epoch': end,
            'cycles': self.rows('SELECT kind,count(*) cycles,sum(expected_nodes) expected FROM sampling_cycles c WHERE ' + predicate + ' GROUP BY kind ORDER BY kind'),
            'work': self.rows("SELECT w.kind,count(*) total,count(*) FILTER(WHERE w.state='complete') complete,count(*) FILTER(WHERE w.state='failed') failed,count(*) FILTER(WHERE w.state='expired') expired,count(*) FILTER(WHERE w.state IN ('queued','running')) pending,percentile_cont(0.95) WITHIN GROUP(ORDER BY extract(epoch FROM(w.started_at-c.period_start))) p95_start_delay_seconds,percentile_cont(0.95) WITHIN GROUP(ORDER BY extract(epoch FROM(w.finished_at-w.started_at))) FILTER(WHERE w.state='complete') p95_run_seconds FROM background_work w JOIN sampling_cycles c ON c.id=w.cycle_id WHERE " + predicate + ' GROUP BY w.kind ORDER BY w.kind'),
            'proxy': self.rows(f'SELECT status,external_status,count(*) count FROM proxy_probe_samples WHERE sampled_at>=to_timestamp({start}) AND sampled_at<to_timestamp({end}) GROUP BY status,external_status'),
            'batches': self.rows("SELECT b.state,b.payload_digest IS NOT NULL captured,count(*) count FROM traffic_sample_batches b JOIN sampling_cycles c ON c.id=b.cycle_id WHERE " + predicate + ' GROUP BY b.state,captured'),
            'gaps': self.rows(f'SELECT reason,count(*) count FROM data_gaps WHERE opened_at>=to_timestamp({start}) AND opened_at<to_timestamp({end}) GROUP BY reason')}

    def observe(self, directory, data, ready):
        start = math.ceil((ready + self.args.warmup_seconds) / 60) * 60
        end = start + self.args.minutes * 60
        stop_at = end + 65
        pid = data['State']['Pid']
        cg = next(x.split('::', 1)[1] for x in Path(f'/proc/{pid}/cgroup').read_text().splitlines() if x.startswith('0::'))
        cgroup = Path('/sys/fs/cgroup') / cg.lstrip('/')
        metadata = {'stable_start_epoch': start, 'stable_end_epoch': end, 'observe_until_epoch': stop_at,
                    'actual_warmup_seconds': start - ready, 'sample_minutes': self.args.minutes}
        save(directory / 'window.json', metadata)
        stop = threading.Event()

        def http():
            with (directory / 'http.jsonl').open('w', buffering=1) as f:
                while time.time() < stop_at and not stop.is_set():
                    epoch, t = time.time(), time.monotonic()
                    record = {'epoch': epoch, 'time': now(), 'stable': start <= epoch < end}
                    try:
                        with urllib.request.urlopen('http://127.0.0.1:8080/readyz', timeout=8) as r:
                            record.update(status=r.status, body=json.load(r))
                    except Exception as e:
                        record.update(status=0, error=type(e).__name__)
                    record['seconds'] = time.monotonic() - t
                    f.write(json.dumps(record) + '\n')
                    stop.wait(max(0, 1 - (time.monotonic() - t)))

        def database():
            sql = activity_query()
            with (directory / 'observer-stderr.log').open('w') as err, (directory / 'resources.jsonl').open('w', buffering=1) as f:
                p = subprocess.Popen(['psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], env=self.env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, bufsize=1)
                try:
                    while time.time() < stop_at and not stop.is_set():
                        epoch, t = time.time(), time.monotonic()
                        record = {'epoch': epoch, 'time': now(), 'stable': start <= epoch < end}
                        for name in ('cpu.stat', 'memory.current', 'memory.events', 'cpu.pressure'):
                            record[name] = (cgroup / name).read_text()
                        record['host_cpu_ticks'] = list(map(int, Path('/proc/stat').read_text().splitlines()[0].split()[1:]))
                        p.stdin.write(sql + ';\n')
                        p.stdin.flush()
                        assert select.select([p.stdout], [], [], 20)[0], 'PG observer timed out'
                        raw = p.stdout.readline()
                        assert raw, 'PG observer ended'
                        record['database'] = json.loads(raw)
                        record['query_seconds'] = time.monotonic() - t
                        f.write(json.dumps(record) + '\n')
                        stop.wait(max(0, 5 - (time.monotonic() - t)))
                finally:
                    p.terminate()
                    p.wait(timeout=10)

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            futures = [executor.submit(http), executor.submit(database)]
            try:
                while time.time() < stop_at:
                    for future in futures:
                        if future.done():
                            future.result()
                    stop.wait(min(10, max(0, stop_at - time.time())))
                    (self.out / 'heartbeat.json').write_text(json.dumps({'at': now(), 'stage': directory.name,
                         'phase': 'warmup' if time.time() < start else 'stable' if time.time() < end else 'settle',
                         'stable_end_epoch': end}))
                    if (self.out / 'STOP').exists():
                        raise RuntimeError('Requested comparison stop')
            finally:
                stop.set()
                for future in futures:
                    future.result()
        quality = self.quality(start, end)
        save(directory / 'quality.json', quality)
        assert len(quality['work']) == 3 and all(x['total'] == self.args.minutes * 6 for x in quality['work']), 'Production cohort changed'
        assert not any(x['reason'] == 'traffic_clear_uncertain' for x in quality['gaps'])
        assert {x['id'] for x in self.gaps()} == {x['id'] for x in self.initial_gaps}, 'New uncertainty gap during experiment'
        check = self.inspect()
        assert check[0]['Id'] == data['Id'] and check[0]['RestartCount'] == 0
        assert check[0]['State']['Health']['Status'] == 'healthy'
        for old, current in zip(self.before[1:], check[1:]):
            assert old['Id'] == current['Id'] and old['State']['StartedAt'] == current['State']['StartedAt']
        p = subprocess.run(['docker', 'logs', data['Id']], capture_output=True, timeout=60)
        assert p.returncode == 0
        (directory / 'api.log').write_bytes(p.stdout + p.stderr)
        print(json.dumps({'event': 'measured', 'stage': directory.name, 'at': now(), 'work': quality['work']}), flush=True)

    def restore(self):
        directory = self.out / 'restored'
        directory.mkdir(mode=0o700, exist_ok=True)
        data, ready = self.switch(32, directory, restore=True)
        for f, raw in self.original.items():
            assert (self.root / f).read_bytes() == raw
        assert data['Image'] == self.before[0]['Image']
        assert self.contract() == self.initial_contract
        assert {x['id'] for x in self.gaps()} == {x['id'] for x in self.initial_gaps}
        save(self.out / 'restoration.json', {'at': now(), 'image': ORIGINAL, 'pool': 32,
             'exact_configs_restored': True, 'contract_and_core_counts_preserved': True,
             'historical_gaps_preserved': True, 'new_uncertain_gaps': 0, 'api_id': data['Id']})
        self.changed = False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--warmup-seconds', type=int, default=180)
    parser.add_argument('--minutes', type=int, default=3)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--resume-prepared', action='store_true', help='Resume an unchanged preflight before its first API switch')
    args = parser.parse_args()
    assert args.warmup_seconds >= 180 and 3 <= args.minutes <= 10
    os.umask(0o077)
    comparison = Comparison(args)
    if args.resume_prepared:
        comparison.resume_prepared()
    else:
        comparison.prepare()
    if args.prepare_only:
        return
    # The controller downloads and verifies the snapshot before any API stop.
    deadline = time.monotonic() + 300
    marker = comparison.out / 'backup-local-verified'
    expected = json.loads((comparison.out / 'backup.json').read_text())['sha256']
    while not marker.exists() or marker.read_text().strip() != expected:
        assert time.monotonic() < deadline, 'Waiting for locally verified backup timed out; API unchanged'
        time.sleep(1)
    error = None
    def interrupted(signum, frame):
        raise RuntimeError('Comparison interrupted by signal ' + str(signum))
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        for index, pool in enumerate([32, 48, 64, 64, 48, 32], 1):
            directory = comparison.out / f'{index:02d}-pool-{pool}'
            directory.mkdir(mode=0o700)
            data, ready = comparison.switch(pool, directory)
            comparison.observe(directory, data, ready)
    except BaseException as e:
        error = e
        save(comparison.out / 'failure.json', {'at': now(), 'error': type(e).__name__, 'message': str(e)})
        raise
    finally:
        if comparison.changed:
            comparison.restore()
        save(comparison.out / 'finished.json', {'at': now(), 'comparison_completed': error is None,
             'original_configuration_restored': not comparison.changed})


if __name__ == '__main__':
    main()
