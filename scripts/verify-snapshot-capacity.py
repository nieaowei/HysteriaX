#!/usr/bin/env python3
"""Measure a private production clone under explicit container memory limits.

All ordinary API workers remain behind a startup gate until their network
namespace permits only the fixture's PostgreSQL and Redis peers. A passing run
is a bounded workload result, not proof of the target host's OS memory budget.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.client import HTTPException
import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen
import uuid

from fixture_network_guard import apply_guard, OWNER_LABEL

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--snapshot', type=Path, required=True)
    parser.add_argument('--key-file', type=Path, required=True)
    parser.add_argument('--expected-sha256', required=True)
    parser.add_argument('--image', required=True)
    parser.add_argument('--postgres-image', default='postgres:18-bookworm')
    parser.add_argument('--redis-image', default='redis:8-alpine')
    parser.add_argument('--caddy-image', default='caddy:2.10')
    parser.add_argument('--guard-image', default='hysteriax-egress-guard:0034-local-arm64')
    parser.add_argument('--postgres-jit', choices=['on','off'], default='on')
    parser.add_argument('--postgres-parallel-workers', type=int, choices=range(0,9), default=2)
    parser.add_argument('--postgres-memory-mib', type=int, default=384)
    parser.add_argument('--api-memory-mib', type=int, default=128)
    parser.add_argument('--replicas', type=int, choices=[1,2,3], default=2)
    parser.add_argument('--database-disconnect-check', action='store_true')
    parser.add_argument('--rollback-image', help='verify a full old/new/old release recovery on this offline clone')
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if args.database_disconnect_check and args.replicas != 3:
        parser.error('database disconnect acceptance requires three API replicas')
    if not 128 <= args.postgres_memory_mib <= 8192:
        parser.error('PostgreSQL memory must be between 128 and 8192 MiB')
    if not 64 <= args.api_memory_mib <= 4096:
        parser.error('API memory must be between 64 and 4096 MiB')
    if args.key_file.stat().st_mode & 0o077:
        parser.error('key file must be owner-only')
    digest = hashlib.sha256()
    with args.snapshot.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    if digest.hexdigest() != args.expected_sha256:
        parser.error('snapshot checksum mismatch')
    private = Path(tempfile.mkdtemp(prefix='hx0034-capacity-'))
    owner = uuid.uuid4().hex
    prefix = 'hx0034-cap-' + owner
    containers, networks, volumes = [], [], []
    report = {'scope':'isolated-real-snapshot-constrained-capacity', 'passed':False,
              'snapshot_sha256':digest.hexdigest(), 'private_logs':str(private),
              'target_host_capacity_proven':False, 'production_changed':False,
              'limits_mib':{'postgres':args.postgres_memory_mib, 'redis':64, 'api_each':args.api_memory_mib, 'caddy':48},
              'runtime_limit_sum_mib':args.postgres_memory_mib+args.replicas*args.api_memory_mib+112, 'api_replicas':args.replicas, 'guards':[]}
    began = time.monotonic()
    report['verifier_source_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    report['postgres_settings'] = {'shared_buffers':'96MB','work_mem':'4MB',
                                  'maintenance_work_mem':'16MB','max_connections':40,
                                  'jit':args.postgres_jit,
                                  'max_parallel_workers_per_gather':args.postgres_parallel_workers}

    def docker(*arguments, input=None, env=None, timeout=120, check=True):
        result = subprocess.run(['docker', *arguments], input=input, env=env,
                                capture_output=True, timeout=timeout)
        if check and result.returncode:
            # Docker diagnostics and application logs can contain private data.
            path = private / ('failure-' + uuid.uuid4().hex + '.log')
            path.touch(mode=0o600)
            path.write_bytes(result.stdout + result.stderr)
            raise RuntimeError('Docker operation failed; private diagnostics retained')
        return result

    def inspect(name):
        return json.loads(docker('inspect', name).stdout)[0]

    def start(role, image, memory, cpu, extra, command=(), *, network=None):
        name = prefix + '-' + role
        containers.append(name)
        docker('run', '-d', '--name', name, '--label', OWNER_LABEL+'='+owner,
               '--network', network or networks[0], '--memory', str(memory)+'m',
               '--memory-swap', str(memory)+'m', '--cpus', str(cpu),
               *extra, image, *command, env=environment)
        return name

    def wait(predicate, seconds, description):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(1)
        raise RuntimeError(description + ' deadline exceeded')

    def sql(statement):
        return docker('exec', '-i', pg, 'psql', '-X', '-U', 'hysteriax',
                      '-d', 'hysteriax', '-At', '-v', 'ON_ERROR_STOP=1',
                      input=statement.encode()).stdout.decode().strip()

    def metrics(name):
        info = inspect(name)
        state = {'oom_killed':info['State']['OOMKilled'], 'restarts':info['RestartCount'],
                 'running':info['State']['Running'], 'exit_code':info['State']['ExitCode']}
        result = docker('exec', name, 'sh', '-ec',
                        'for f in memory.current memory.peak memory.max memory.events cpu.stat; '
                        'do echo "$f"; cat /sys/fs/cgroup/"$f"; done',check=False)
        if result.returncode:
            return {**state,'collection_complete':False}
        output = result.stdout.decode().splitlines()
        values, section = {}, None
        for line in output:
            if line in ('memory.current', 'memory.peak', 'memory.max', 'memory.events', 'cpu.stat'):
                section = line
                values[section] = {}
            elif section in ('memory.current', 'memory.peak', 'memory.max'):
                values[section] = int(line)
            else:
                key, value = line.split()
                values[section][key] = int(value)
        values.update(state,collection_complete=True)
        return values

    environment = dict(os.environ)
    environment.update(POSTGRES_PASSWORD=secrets.token_hex(24), POSTGRES_DB='hysteriax',
                       HYSTERIAX_DB_USER='hysteriax', HYSTERIAX_DB_PASSWORD=secrets.token_hex(24),
                       HYSTERIAX_REDIS_PASSWORD=secrets.token_hex(24),
                       HYSTERIAX_MASTER_KEY=json.loads(args.key_file.read_text())['HYSTERIAX_MASTER_KEY'],
                       HYSTERIAX_ADMIN_TOKEN=secrets.token_urlsafe(40), TOKIO_WORKER_THREADS='2',
                       HYSTERIAX_LISTEN_ADDR='0.0.0.0:8080', HYSTERIAX_PUBLIC_URL='http://fixture.invalid',
                       HYSTERIAX_ASSET_CACHE_DIR='/assets', RUST_LOG='warn')
    environment['DATABASE_URL'] = 'postgresql://hysteriax:'+environment['HYSTERIAX_DB_PASSWORD']+'@postgres:5432/hysteriax'
    environment['HYSTERIAX_REDIS_URL'] = 'redis://:'+environment['HYSTERIAX_REDIS_PASSWORD']+'@redis:6379/0'
    api_env = ['DATABASE_URL','HYSTERIAX_MASTER_KEY','HYSTERIAX_ADMIN_TOKEN','HYSTERIAX_REDIS_URL',
               'TOKIO_WORKER_THREADS','HYSTERIAX_LISTEN_ADDR','HYSTERIAX_PUBLIC_URL','HYSTERIAX_ASSET_CACHE_DIR','RUST_LOG']
    try:
        report['images'] = {}
        for role, image in [('api',args.image),('postgres',args.postgres_image),
                            ('redis',args.redis_image),('caddy',args.caddy_image)]:
            value = json.loads(docker('image','inspect',image).stdout)[0]
            report['images'][role] = {'id':value['Id'],'architecture':value['Architecture']}
        report['docker_architecture'] = docker('info','--format','{{.Architecture}}').stdout.decode().strip()
        report['all_management_images_amd64'] = all(value['architecture']=='amd64' for value in report['images'].values())
        report['emulation_required'] = report['docker_architecture'] not in ('x86_64','amd64') and report['images']['api']['architecture']=='amd64'
        for suffix, internal in [('data',True),('ingress',False)]:
            network = prefix+'-'+suffix
            docker('network','create','--label',OWNER_LABEL+'='+owner,
                   *(['--internal'] if internal else []), network)
            networks.append(network)
        volume = prefix+'-pg'
        docker('volume','create','--label',OWNER_LABEL+'='+owner,volume)
        volume_info = json.loads(docker('volume','inspect',volume).stdout)[0]
        if (volume_info.get('Labels') or {}).get(OWNER_LABEL) != owner:
            raise RuntimeError('refusing a volume not owned by this fixture')
        volumes.append(volume)
        pg = start('pg', args.postgres_image,args.postgres_memory_mib,1,
                   ['--network-alias','postgres','-e','POSTGRES_PASSWORD','-e','POSTGRES_DB',
                    '-e','HYSTERIAX_DB_USER','-e','HYSTERIAX_DB_PASSWORD','-v',volume+':/var/lib/postgresql',
                    '-v',str(ROOT/'scripts/provision-postgres.sh')+':/docker-entrypoint-initdb.d/10-provision-hysteriax.sh:ro'],
                   ['postgres','-c','shared_buffers=96MB','-c','work_mem=4MB','-c','maintenance_work_mem=16MB',
                    '-c','max_parallel_maintenance_workers=0','-c','max_connections=40',
                    '-c','jit='+args.postgres_jit,'-c','max_parallel_workers_per_gather='+str(args.postgres_parallel_workers)])
        redis = start('redis',args.redis_image,64,.1,
                      ['--network-alias','redis','-e','HYSTERIAX_REDIS_PASSWORD'],
                      ['sh','-ec','exec redis-server --requirepass "$HYSTERIAX_REDIS_PASSWORD" '
                       '--maxmemory 32mb --maxmemory-policy allkeys-lru --appendonly no --save ""'])
        # The entrypoint's temporary init server accepts Unix-socket traffic
        # before the application role exists. Wait for the final TCP listener.
        wait(lambda: docker('exec',pg,'pg_isready','-h','127.0.0.1','-U','hysteriax','-d','hysteriax',check=False).returncode==0,90,'PostgreSQL')
        print('restoring private snapshot',flush=True)
        with args.snapshot.open('rb') as source, (private/'restore.log').open('xb') as output:
            os.chmod(private/'restore.log',0o600)
            result = subprocess.run(['docker','exec','-i',pg,'pg_restore','-U','hysteriax','-d','hysteriax',
                                     '--exit-on-error','--no-owner','--no-acl'],stdin=source,stdout=output,
                                    stderr=subprocess.STDOUT,timeout=1200)
        if result.returncode:
            raise RuntimeError('private snapshot restore failed')

        def gated(role, migrate=False, image=None):
            gate = private/role
            gate.mkdir(mode=0o755)
            assets = private/(role+'-assets')
            assets.mkdir(mode=0o777)
            assets.chmod(0o777)
            command = 'while [ ! -f /gate/go ]; do sleep 1; done; '
            if migrate:
                command += 'hysteriax-server --migrate-only; code=$?; echo "$code" >/assets/exit; sleep 3600'
            else:
                command += 'exec hysteriax-server'
            extra = ['--network-alias','api' if not migrate else 'maintenance',
                     '--cap-drop','ALL','--sysctl','net.ipv6.conf.all.disable_ipv6=1',
                     '--entrypoint','/bin/sh','-v',str(gate)+':/gate:ro','-v',str(assets)+':/assets']
            for key in api_env:
                extra += ['-e',key]
            original_url = environment['DATABASE_URL']
            if args.database_disconnect_check:
                environment['DATABASE_URL'] = original_url+'?application_name='+owner+'-'+role
            try:
                name = start(role,image or args.image,192 if migrate else args.api_memory_mib,.6 if migrate else .3,extra,['-c',command])
            finally:
                environment['DATABASE_URL'] = original_url
            uid = int(docker('exec',name,'id','-u').stdout)
            capabilities = docker('exec',name,'sh','-ec',
                                  'sed -n "s/^CapEff:[[:space:]]*//p" /proc/self/status').stdout.strip()
            if uid == 0 or int(capabilities,16) != 0:
                raise RuntimeError('fixture API must run as non-root without effective capabilities')
            report['guards'].append(apply_guard(name,pg,redis,owner=owner,image=args.guard_image))
            report['guards'][-1].update(nonroot=True,effective_capabilities=0)
            result = docker('exec',name,'curl','--noproxy','*','--connect-timeout','2',
                            '--max-time','3','-s','http://1.1.1.1/',check=False)
            if result.returncode == 0:
                raise RuntimeError('fixture egress unexpectedly reachable; gate remains closed')
            (gate/'go').touch(mode=0o644)
            return name, assets

        recovery = None
        if args.rollback_image:
            from snapshot_release_recovery import ReleaseRecovery
            token_hash = base64.urlsafe_b64encode(hashlib.sha256(environment['HYSTERIAX_ADMIN_TOKEN'].encode()).digest()).decode().rstrip('=')
            sql("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES ('capacity-fixture','"+token_hash+"','isolated release fixture',now())")
            recovery = ReleaseRecovery(pg=pg,redis=redis,owner=owner,image=args.rollback_image,
                private=private,docker=docker,sql=sql,gated=gated,wait=wait)
            print('checking old release and making full maintenance backup',flush=True)
            recovery.before_upgrade()
        print('migrating behind verified outbound guard',flush=True)
        migration, assets = gated('migration',True)
        wait(lambda: (assets/'exit').exists(),600,'migration')
        report['migration_memory'] = metrics(migration)
        if (assets/'exit').read_text().strip() != '0':
            raise RuntimeError('migration failed under memory limit')
        docker('rm','-f',migration)
        containers.remove(migration)
        token_hash = base64.urlsafe_b64encode(hashlib.sha256(environment['HYSTERIAX_ADMIN_TOKEN'].encode()).digest()).decode().rstrip('=')
        if not recovery:
            sql("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES ('capacity-fixture','"+token_hash+"','isolated capacity fixture',now())")
        before_users = int(sql('SELECT count(*) FROM users'))
        apis = [gated('api'+str(index))[0] for index in range(args.replicas)]
        for name in apis:
            wait(lambda: docker('exec',name,'curl','--max-time','3','--fail','-s','http://127.0.0.1:8080/readyz',check=False).returncode==0,90,'API readiness')
        config = private/'Caddyfile'
        config.write_text((ROOT/'deploy/caddy/Caddyfile').read_text().replace('reverse_proxy {',
                         'reverse_proxy {\n        header_down X-Fixture-Backend {http.reverse_proxy.upstream.address}'))
        caddy = start('caddy',args.caddy_image,48,.2,
                      ['-p','127.0.0.1::80','-e','HYSTERIAX_DOMAIN=http://:80','-v',str(config)+':/etc/caddy/Caddyfile:ro'], network=networks[1])
        docker('network','connect',networks[0],caddy)
        wait(lambda: bool(inspect(caddy)['NetworkSettings']['Ports'].get('80/tcp')),30,'Caddy port')
        port = inspect(caddy)['NetworkSettings']['Ports']['80/tcp'][0]['HostPort']
        base = 'http://127.0.0.1:'+port
        def request(path, body=None, identity=None):
            headers = {'Authorization':'Bearer '+environment['HYSTERIAX_ADMIN_TOKEN']}
            if body is not None:
                headers.update({'Content-Type':'application/json','Idempotency-Key':identity})
            start_time = time.monotonic()
            try:
                response = urlopen(Request(base+path,headers=headers,data=body),timeout=30)
            except HTTPError as error:
                response = error
            with response:
                payload = response.read()
                return {'status':response.status,'ms':(time.monotonic()-start_time)*1000,
                        'bytes':len(payload),'backend':response.headers.get('X-Fixture-Backend')}, payload
        def caddy_ready():
            try:
                return request('/readyz')[0]['status']==200
            except (OSError, HTTPException):
                return False
        wait(caddy_ready,30,'Caddy readiness')
        routes = ['/readyz','/api/v2/users?page_size=20','/api/v2/nodes?page_size=20',
                  '/api/v2/jobs?page_size=20','/api/v2/audit?page_size=20','/api/v2/overview',
                  '/api/v2/overview/nodes','/api/v2/overview/history?range=30d&timezone=Asia%2FShanghai']
        def workload(stage):
            print('workload '+stage,flush=True)
            with ThreadPoolExecutor(max_workers=4) as executor:
                rows = list(executor.map(lambda index:request(routes[index%len(routes)])[0],range(96)))
            delays = sorted(row['ms'] for row in rows)
            backends = {row['backend'] for row in rows if row['backend']}
            result = {'requests':len(rows),'statuses':{},'p50_ms':delays[47],
                      'p95_ms':delays[91],'p99_ms':delays[95], 'backends_seen':len(backends),
                      'response_bytes':sum(row['bytes'] for row in rows)}
            for row in rows:
                key = str(row['status'])
                result['statuses'][key] = result['statuses'].get(key,0)+1
            report.setdefault('workloads',{})[stage] = result
            report.setdefault('memory',{})[stage] = {role:metrics(name) for role,name in
                [('postgres',pg),*[(f'api{index}',name) for index,name in enumerate(apis)],('caddy',caddy)]}
            if result['statuses'] != {'200':96} or len(backends)!=args.replicas:
                raise RuntimeError('bounded workload failed or did not reach both replicas')
        workload('warm')
        identity, body = str(uuid.uuid4()), b'{"name":"Isolated capacity fixture"}'
        replies = [request('/api/v2/users',body,identity) for _ in range(12)]
        report['idempotency'] = {'requests':12,'backends_seen':len({r[0]['backend'] for r in replies}),
                                 'exact_replay':len({r[1] for r in replies})==1,
                                 'user_count_delta':int(sql('SELECT count(*) FROM users'))-before_users}
        if any(r[0]['status'] not in (200,201) for r in replies) or report['idempotency'] != {
                'requests':12,'backends_seen':args.replicas,'exact_replay':True,'user_count_delta':1}:
            raise RuntimeError('idempotency cross-replica check failed')
        docker('stop','-t','3',redis)
        workload('redis_outage')
        docker('start',redis)
        wait(lambda: docker('exec',redis,'sh','-ec','REDISCLI_AUTH="$HYSTERIAX_REDIS_PASSWORD" redis-cli ping',check=False).stdout.strip()==b'PONG',30,'Redis restart')
        time.sleep(3)
        workload('redis_empty_restart')
        def redis_rebuilt():
            if request('/api/v2/users?page_size=20')[0]['status'] != 200:
                return False
            count = int(docker('exec',redis,'sh','-ec',
                               'REDISCLI_AUTH="$HYSTERIAX_REDIS_PASSWORD" redis-cli dbsize').stdout)
            report['redis_keys_after_empty_restart'] = count
            return count > 0
        wait(redis_rebuilt,30,'Redis cache rebuild')
        if args.database_disconnect_check:
            from database_disconnect_fixture import verify
            print('isolating one live billing executor from PostgreSQL',flush=True)
            report['database_disconnect'] = verify(owner=owner,apis=apis,pg=pg,redis=redis,
                guard_image=args.guard_image,docker=docker,sql=sql,wait=wait,private=private,
                register_container=containers.append)
        report['redis_memory'] = metrics(redis)
        report['database_connections'] = int(sql('SELECT count(*) FROM pg_stat_activity'))
        report['granted_advisory_lock_count_after_migration'] = int(sql("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND granted"))
        samples = [value for stage in report['memory'].values() for value in stage.values()]+[report['redis_memory'],report['migration_memory']]
        if any(not value['collection_complete'] or value['oom_killed'] or value['memory.events'].get('oom',0) or value['memory.events'].get('oom_kill',0) or value['restarts'] or not value['running'] for value in samples):
            raise RuntimeError('memory pressure killed or restarted a service')
        if report['granted_advisory_lock_count_after_migration']:
            raise RuntimeError('unexpected runtime advisory lock observed')
        if recovery:
            print('stopping all new replicas and restoring complete old release backup',flush=True)
            report['release_recovery'] = recovery.rollback(apis)
        report['passed'] = True
    except Exception as error:
        report['failure_category'] = type(error).__name__
        report['failure'] = str(error) if isinstance(error,RuntimeError) else 'private diagnostic withheld'
        raise
    finally:
        for name in reversed(containers):
            value = docker('inspect',name,check=False)
            if value.returncode:
                continue
            info = json.loads(value.stdout)[0]
            if (info['Config'].get('Labels') or {}).get(OWNER_LABEL) != owner:
                report.setdefault('cleanup_refused_unowned',[]).append(name)
                continue
            try:
                report.setdefault('final_container_memory',{})[name.removeprefix(prefix+'-')] = metrics(name)
            except Exception:
                report.setdefault('unavailable_final_metrics',[]).append(name.removeprefix(prefix+'-'))
            value = docker('logs',name,check=False)
            path = private/(name+'.log')
            path.touch(mode=0o600)
            path.write_bytes(value.stdout+value.stderr)
            if docker('rm','-f',name,check=False).returncode:
                report.setdefault('cleanup_failures',[]).append('container')
        for volume in volumes:
            value = docker('volume','inspect',volume,check=False)
            if value.returncode==0 and (json.loads(value.stdout)[0].get('Labels') or {}).get(OWNER_LABEL)==owner:
                if docker('volume','rm',volume,check=False).returncode:
                    report.setdefault('cleanup_failures',[]).append('volume')
        for network in reversed(networks):
            value = docker('network','inspect',network,check=False)
            if value.returncode==0 and (json.loads(value.stdout)[0].get('Labels') or {}).get(OWNER_LABEL)==owner:
                if docker('network','rm',network,check=False).returncode:
                    report.setdefault('cleanup_failures',[]).append('network')
        report['cleanup_complete'] = not report.get('cleanup_failures') and not report.get('cleanup_refused_unowned')
        report['passed'] = report['passed'] and report['cleanup_complete']
        report['elapsed_seconds'] = time.monotonic()-began
        args.report.parent.mkdir(parents=True,exist_ok=True)
        args.report.write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps({'passed':report['passed'],'report':str(args.report)}),flush=True)


if __name__ == '__main__':
    main()
