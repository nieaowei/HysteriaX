#!/usr/bin/env python3
"""Three full services: Redis outage/restart, lost hints, rollback and late fills.

Only own unique DB/container/PIDs are changed. Rank node metadata is inserted as
an isolated fixture; no SSH/deployment is claimed. Production cache triggers run.
"""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import psycopg
from psycopg import sql

from multi_instance_test_fixture import MultiInstanceServer, free_port
from live_cache_test_fixture import OwnedRedis


def inspect(name):
    return json.loads(subprocess.check_output(['docker', 'inspect', name]))[0]


def request(origin, token, method, path, payload=None, expected=200):
    headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'}
    if method != 'GET':
        headers['Idempotency-Key'] = str(uuid.uuid4())
    req = urllib.request.Request(origin + path, headers=headers, method=method,
                                 data=None if payload is None else json.dumps(payload).encode())
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            data = response.read()
            status, body = response.status, json.loads(data) if data else None
    except urllib.error.HTTPError as error:
        status, body = error.code, None
    assert status == expected, f'HTTP {status} instead of {expected}; inspect private logs'
    return body


@contextmanager
def paused(fixture):
    indices = []
    try:
        for index, process in enumerate(fixture.processes):
            process.send_signal(signal.SIGSTOP)
            fixture.paused.add(index)
            indices.append(index)
        yield
    finally:
        for index in indices:
            fixture.processes[index].send_signal(signal.SIGCONT)
            fixture.paused.remove(index)


def verify(connection, url, fixture, token, redis):
    origins = [f'http://127.0.0.1:{port}' for port in fixture.ports]
    front = 'http://127.0.0.1:' + fixture.environment['HYSTERIAX_LISTEN_ADDR'].rsplit(':', 1)[1]
    reads = 0
    def every_user(identifier, revision, name):
        nonlocal reads
        for origin in origins:
            user = request(origin, token, 'GET', '/api/v2/users/' + identifier)
            assert (user['revision'], user['name']) == (revision, name)
            reads += 1
    user = request(front, token, 'POST', '/api/v2/users', {'name': 'cache-before'}, 201)
    identifier = user['id']
    every_user(identifier, 1, 'cache-before')
    old_responses = {key: redis.command('GET', key) for key in redis.keys('*:responses:*')}
    assert old_responses
    request(front, token, 'PATCH', '/api/v2/users/' + identifier,
            {'expected_revision': 1, 'name': 'cache-committed'})
    every_user(identifier, 2, 'cache-committed')
    vector = connection.execute("SELECT domain,bucket,generation FROM cache_bucket_versions ORDER BY domain,bucket").fetchall()
    with psycopg.connect(url) as rollback:
        rollback.execute("UPDATE users SET name='uncommitted',revision=revision+1 WHERE id=%s", (identifier,))
        every_user(identifier, 2, 'cache-committed')
        rollback.rollback()
    assert vector == connection.execute("SELECT domain,bucket,generation FROM cache_bucket_versions ORDER BY domain,bucket").fetchall()
    every_user(identifier, 2, 'cache-committed')

    # Seed new, non-deployed metadata only. Reference maintenance bypass applies
    # only to these artificial encrypted fields, never cache version triggers.
    collision = connection.execute("SELECT array_agg(id ORDER BY id) FROM (SELECT 'cache-node-'||n AS id,hysteriax_cache_bucket('cache-node-'||n) bucket FROM generate_series(1,256) n) s GROUP BY bucket HAVING count(*)>1 ORDER BY bucket LIMIT 1").fetchone()[0][:2]
    nodes = collision + ['cache-rank-' + str(i) for i in range(10)]
    with connection.transaction():
        connection.execute("SET LOCAL hysteriax.reference_maintenance='on'")
        connection.execute("INSERT INTO credentials(id,kind,name,latest_version,created_at,updated_at) VALUES('cache-ssh','ssh_password','Cache fixture',1,now(),now())")
        connection.execute("INSERT INTO credential_versions(credential_id,version,payload_enc,artifact_names,created_at) VALUES('cache-ssh',1,'fixture','{}',now())")
        for index, node in enumerate(nodes):
            connection.execute("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES(%s,%s,'127.0.0.1',1,'root','cache-ssh',1,'cache.test',443,':443',%s,'fixture','fixture','fixture',now(),now())", (node, f'Cache node {index:02d}', node))
            connection.execute("INSERT INTO node_packages(node_id,period_id,config,usage_bytes) VALUES(%s,%s,'{\"quota_bytes\":1000}',%s)", (node, node, index * 50))
            connection.execute("DELETE FROM credential_reference_dirty WHERE kind='node' AND source_id=%s", (node,))
    def check_rank():
        nonlocal reads
        reference_sql = (Path(__file__).resolve().parents[1] / 'crates/hysteriax-server/src/overview/summary_quota.sql').read_text().replace('$1', '%s')
        expected = [row[0] for row in connection.execute(reference_sql, (connection.execute('SELECT now()').fetchone()[0],))]
        for origin in origins:
            actual = request(origin, token, 'GET', '/api/v2/overview')['quota_rank']
            assert [row['id'] for row in actual] == expected, 'HTTP ranking diverged from DB reference'
            reads += 1
        return expected
    check_rank()
    meta_keys = redis.keys('*:quota_rank:*:meta')
    assert len(meta_keys) == 1
    base = meta_keys[0][:-5]
    keys = [base + b':' + suffix for suffix in (b'meta', b'rank', b'members', b'deadlines', b'versions')]
    # Freeze all three listeners so hints are provably lost; ordinary version
    # triggers stay active. Disconnect only this disposable DB's LISTEN sessions.
    with paused(fixture):
        killed = connection.execute("SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND upper(query) LIKE 'LISTEN%') t").fetchone()[0]
        assert killed >= 3, 'did not identify all three listener sessions'
        with connection.transaction():
            for index, node in enumerate(collision):
                connection.execute('UPDATE node_packages SET usage_bytes=%s WHERE node_id=%s', (2000+index*1000, node))
    assert check_rank()[0] == collision[1]
    connection.execute("SELECT pg_notify('hysteriax_quota_rank',%s)", (json.dumps({'schema': 'public', 'id': collision[0], 'version': 0}),))
    assert check_rank()[0] == collision[1]

    # Save an actual receiver request before deletion and replay it afterwards.
    with paused(fixture):
        meta = redis.command('GET', keys[0]).split(b'|')
        members = redis.command('HGETALL', keys[2])
        versions = redis.command('HGETALL', keys[4])
        old_members = dict(zip(members[::2], members[1::2]))
        old_versions = dict(zip(versions[::2], versions[1::2]))
        late = ['rebuild', meta[0], str(int(time.time()*1000000)), meta[-1]]
        for node, version in old_versions.items():
            late += [node, version, old_members.get(node, b''), b'']
    deleted = collision[1]
    connection.execute('DELETE FROM nodes WHERE id=%s', (deleted,))
    assert deleted not in check_rank()
    lua = (Path(__file__).resolve().parents[1] / 'crates/hysteriax-server/src/quota_rank.lua').read_bytes()
    with paused(fixture):
        tombstone = redis.command('HGET', keys[4], deleted)
        assert tombstone > old_versions[deleted.encode()]
        assert redis.command('EVAL', lua, 5, *keys, *late) == 0
        redis.command('DEL', keys[0])
        assert redis.command('EVAL', lua, 5, *keys, *late) == 0
        assert redis.command('GET', keys[0]) is None
        assert redis.command('HGET', keys[4], deleted) == tombstone
    assert deleted not in check_rank()
    for origin in origins:
        request(origin, token, 'GET', '/api/v2/nodes/' + deleted, expected=404)
    request(front, token, 'DELETE', '/api/v2/users/' + identifier + '?expected_revision=2', expected=204)
    for key, value in old_responses.items():
        redis.command('SET', key, value, 'PX', 60000)
    for origin in origins:
        request(origin, token, 'GET', '/api/v2/users/' + identifier, expected=404)
    redis.stop()
    assert not redis.ready()
    replacement = request(front, token, 'POST', '/api/v2/users', {'name': 'created-without-redis'}, 201)
    every_user(replacement['id'], 1, 'created-without-redis')
    for origin in origins:
        assert request(origin, token, 'GET', '/readyz')['management_protocol'] == 2
    check_rank()
    errors = [request(origin, token, 'GET', '/api/v2/cache')['caches']['redis']['errors'] for origin in origins]
    assert all(value > 0 for value in errors)
    redis.start()
    # Persistence disabled: this is an actual empty Redis restart, not a flush of
    # the shared Redis. Allow bounded service circuit/reconnection recovery.
    deadline = time.monotonic() + 10
    while True:
        check_rank()
        if redis.keys('*:quota_rank:*:meta'):
            break
        assert time.monotonic() < deadline, 'Redis mirror did not rebuild after restart'
        time.sleep(.1)
    every_user(replacement['id'], 1, 'created-without-redis')
    assert all(process.poll() is None for process in fixture.processes)
    return {'passed': True, 'checked_http_projections': reads, 'colliding_node_ids': collision,
            'lost_listener_connections': killed, 'redis_error_counts_during_outage': errors,
            'checks': ['cross-process-committed-revision', 'uncommitted-and-rollback-isolation', 'same-bucket-distinct-entity-update', 'lost-LISTEN-notifications', 'stale-notification-hint', 'deleted-node-tombstone', 'late-rebuild-rejected-with-and-without-meta', 'late-user-cache-fill-does-not-resurrect', 'Redis-outage-DB-fallback-and-ready', 'empty-Redis-restart-rebuild'],
            'limitations': ['rank fixtures contain no deployed node and do not exercise SSH', 'delayed receiver uses the actual Lua script with previously captured version/member arguments; it is not a paused server HTTP snapshot', 'reference maintenance bypass only for metadata fixture insertion; production cache triggers remain active']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=Path(__file__).resolve().parents[1] / 'target/debug/hysteriax-server')
    parser.add_argument('--pg-container', default='hx-uitest-pg')
    parser.add_argument('--redis-image-container', default='hx-0034-reference-check')
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    pg, reference_redis = inspect(args.pg_container), inspect(args.redis_image_container)
    settings = dict(item.split('=', 1) for item in pg['Config']['Env'] if '=' in item)
    user = settings.get('POSTGRES_USER', 'postgres')
    port = pg['NetworkSettings']['Ports']['5432/tcp'][0]['HostPort']
    params = {'host': '127.0.0.1', 'port': port, 'user': user, 'password': settings['POSTGRES_PASSWORD'], 'connect_timeout': 5}
    name = 'hx0034_cache_' + uuid.uuid4().hex
    url = 'postgres://{}:{}@127.0.0.1:{}/{}'.format(urllib.parse.quote(user, safe=''), urllib.parse.quote(params['password'], safe=''), port, name)
    logs = Path(tempfile.mkdtemp(prefix='hx0034-cache-'))
    token = secrets.token_urlsafe(40)
    fixture, redis = None, None
    report = {'scope': 'three-full-process-cache-faults', 'passed': False, 'private_logs': str(logs)}
    with psycopg.connect(**params, dbname=settings.get('POSTGRES_DB', user), autocommit=True) as admin:
        admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(name)))
        try:
            redis = OwnedRedis(reference_redis['Image'], free_port())
            env = {**os.environ, 'DATABASE_URL': url, 'HYSTERIAX_LISTEN_ADDR': f'127.0.0.1:{free_port()}',
                   'HYSTERIAX_MASTER_KEY': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 'HYSTERIAX_ADMIN_TOKEN': token,
                   'HYSTERIAX_REDIS_URL': f'redis://127.0.0.1:{redis.port}/', 'RUST_LOG': 'warn'}
            with (logs / 'migration.log').open('wb') as output:
                fixture = MultiInstanceServer(args.server.resolve(), env, logs, 3, output)
            with psycopg.connect(url, autocommit=True) as connection:
                connection.execute("SET statement_timeout='3s'")
                report.update(verify(connection, url, fixture, token, redis))
                report['backends'] = fixture.report()
            report['server_sha256'] = hashlib.sha256(args.server.read_bytes()).hexdigest()
        finally:
            if fixture:
                fixture.terminate()
                try:
                    fixture.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait(timeout=5)
            if redis:
                redis.close()
            admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(name)))
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2) + '\n')
    print('three-process cache fault verification passed', flush=True)


if __name__ == '__main__':
    main()
