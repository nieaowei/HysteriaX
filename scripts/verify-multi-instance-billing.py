#!/usr/bin/env python3
"""Crash a full service after a 500-item billing checkpoint, then verify recovery.

Only uniquely owned DBs/processes are mutated. Captured batches are DB fixtures;
production workers, leases, accounting and HTTP quota-reset code process them.
No SSH capture, node counter correction, or production rollout is claimed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request
import uuid

import psycopg
from psycopg import sql
from multi_instance_test_fixture import MultiInstanceServer, free_port

COUNT = 512
USERS = [f'billing-user-{index:04d}' for index in range(COUNT)]
NODES = ['billing-node-a', 'billing-node-b']


def inspect(name):
    return json.loads(subprocess.check_output(['docker', 'inspect', name]))[0]


def wait_for(predicate, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.01)
    raise AssertionError('billing condition timed out; inspect isolated logs')


def seed(connection):
    with connection.transaction():
        connection.execute("SET LOCAL hysteriax.reference_maintenance='on'")
        connection.execute("INSERT INTO credentials(id,kind,name,created_at,updated_at) VALUES('billing-ssh','ssh_password','Billing fixture',now(),now())")
        connection.execute("INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('billing-ssh',1,'fixture',now())")
        connection.execute("INSERT INTO users(id,name,created_at,updated_at) SELECT id,id,now(),now() FROM unnest(%s::text[]) id", (USERS,))
        for index, node in enumerate(NODES):
            connection.execute("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES(%s,%s,'127.0.0.1',1,'root','billing-ssh',1,'billing.test',443,':443',%s,'fixture','fixture','fixture',now(),now())", (node,node,node))
            connection.execute("DELETE FROM credential_reference_dirty WHERE kind='node' AND source_id=%s", (node,))
            connection.execute("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT id,%s,id,'fixture',now() FROM unnest(%s::text[]) id", (node,USERS))
            connection.execute("INSERT INTO traffic_baselines(node_id,user_id,instance_id,tx_total,rx_total,sampled_at,sample_order) SELECT %s,id,'boot-1',%s,%s,now(),0 FROM unnest(%s::text[]) id", (node,(index+1)*100,(index+1)*50,USERS))


def reserve(connection, node, state='capturing', counters=None, boot='boot-1'):
    cycle = uuid.uuid4()
    sample = f'{cycle}:{node}'
    with connection.transaction():
        connection.execute("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES(%s,%s,now(),now()+interval '10 seconds',1)", (cycle,'billing-fixture:'+str(cycle)))
        connection.execute("INSERT INTO traffic_sample_batches(sample_id,node_id,cycle_id) VALUES(%s,%s,%s)", (sample,node,cycle))
        if state == 'ready':
            seal(connection,sample,node,cycle,counters,boot)
    return sample, cycle


def seal(connection, sample, node, cycle, counters, boot='boot-1'):
    tx, rx = counters
    with connection.transaction():
        connection.execute("INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at) SELECT %s,%s,id,%s,%s,now() FROM unnest(%s::text[]) id ON CONFLICT(sample_id,user_id) DO NOTHING", (sample,node,tx,rx,USERS))
        digest = hashlib.sha256(f'{boot}:{tx}:{rx}'.encode()).hexdigest()
        connection.execute("UPDATE traffic_sample_batches SET instance_id=%s,payload_digest=%s,item_count=%s,state='ready' WHERE sample_id=%s AND state='capturing'", (boot,digest,COUNT,sample))
        connection.execute("INSERT INTO background_work(id,kind,cycle_id,node_id,deadline_at,payload) VALUES(%s,'traffic',%s,%s,clock_timestamp()+interval '10 seconds','{\"fixture\":true}') ON CONFLICT(id) DO NOTHING", (sample,cycle,node))


def completed(connection, sample):
    row = connection.execute("SELECT b.processed_count,b.state,w.state,w.generation FROM traffic_sample_batches b JOIN background_work w ON w.id=b.sample_id WHERE b.sample_id=%s", (sample,)).fetchone()
    return row if row and row[:3] == (COUNT,'complete','complete') else None


def check_usage(connection, total, reset_total):
    rows = connection.execute('SELECT id,usage_bytes FROM users ORDER BY id').fetchall()
    expected = [(user,reset_total if user==USERS[0] else total) for user in USERS]
    assert rows == expected, 'billing diverged from serial reference'
    return hashlib.sha256(json.dumps(rows).encode()).hexdigest()


def verify(connection, url, fixture, token):
    seed(connection)
    # The 501st user's lock leaves the first page free to COMMIT and forces the
    # next page to wait. Kill while the DB proves that exact wait is in flight.
    with psycopg.connect(url) as gate:
        gate_pid = gate.execute('SELECT pg_backend_pid()').fetchone()[0]
        gate.execute('SELECT id FROM users WHERE id=%s FOR NO KEY UPDATE', (USERS[500],))
        sample_a, _ = reserve(connection,NODES[0],'ready',(120,70))
        wait_for(lambda: connection.execute('SELECT processed_count=500 FROM traffic_sample_batches WHERE sample_id=%s',(sample_a,)).fetchone()[0])
        wait_for(lambda: connection.execute('SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND %s=ANY(pg_blocking_pids(pid)))',(gate_pid,)).fetchone()[0])
        old_owner, old_generation = connection.execute('SELECT owner,generation FROM background_work WHERE id=%s',(sample_a,)).fetchone()
        assert old_owner in fixture.instance_ids
        old_expiry = connection.execute('SELECT expires_at FROM resource_leases WHERE resource_key=%s',('work:'+sample_a,)).fetchone()[0]
        index = fixture.instance_ids.index(old_owner)
        with fixture.lock:
            fixture.active_ports.remove(fixture.ports[index])
        started = time.monotonic()
        fixture.processes[index].kill()
        fixture.processes[index].wait(timeout=5)
        gate.rollback()
    assert connection.execute('SELECT processed_count FROM traffic_sample_batches WHERE sample_id=%s',(sample_a,)).fetchone()[0] == 500
    assert connection.execute('SELECT count(*) FROM traffic_records WHERE sample_id=%s',(sample_a,)).fetchone()[0] == 500
    print('killed owned billing executor after committed 500-item checkpoint',flush=True)
    front = 'http://127.0.0.1:' + fixture.environment['HYSTERIAX_LISTEN_ADDR'].rsplit(':',1)[1]
    request = urllib.request.Request(front+'/api/v2/users/'+USERS[0]+'/quota/reset',method='POST',
                                    headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','Idempotency-Key':str(uuid.uuid4())},
                                    data=json.dumps({'expected_revision':1}).encode())
    with urllib.request.urlopen(request,timeout=5) as response:
        assert response.status == 200
    # The same users receive accounting from the unrelated node before the
    # crashed work's lease expires. Its live worker is never reset by recovery.
    sample_b, _ = reserve(connection,NODES[1],'ready',(230,120))
    wait_for(lambda: completed(connection,sample_b))
    assert connection.execute('SELECT clock_timestamp()<%s',(old_expiry,)).fetchone()[0]
    peer = connection.execute('SELECT generation FROM background_work WHERE id=%s',(sample_b,)).fetchone()[0]
    wait_for(lambda: completed(connection,sample_a),seconds=100)
    recovery_seconds = time.monotonic()-started
    new_generation = connection.execute('SELECT generation FROM background_work WHERE id=%s',(sample_a,)).fetchone()[0]
    assert new_generation > old_generation
    assert connection.execute('SELECT clock_timestamp()>=%s',(old_expiry,)).fetchone()[0]
    assert connection.execute('SELECT generation,state FROM background_work WHERE id=%s',(sample_b,)).fetchone() == (peer,'complete')
    check_usage(connection,90,50)
    assert connection.execute('SELECT count(*) FROM traffic_records').fetchone()[0] == COUNT*2
    assert connection.execute('SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NULL').fetchone()[0] == 0
    print('healthy peer completed; expired captured work resumed without duplicate charges',flush=True)
    # Duplicate delivery of the same durable work item must acknowledge the
    # existing complete batch, preserving immutable counters and ledger rows.
    connection.execute("UPDATE background_work SET state='queued',finished_at=NULL WHERE id=ANY(%s)",([sample_a,sample_b],))
    wait_for(lambda: completed(connection,sample_a) and completed(connection,sample_b))
    check_usage(connection,90,50)
    assert connection.execute('SELECT count(*) FROM traffic_records').fetchone()[0] == COUNT*2
    older, cycle = reserve(connection,NODES[0])
    newer, _ = reserve(connection,NODES[0],'ready',(200,150))
    wait_for(lambda: connection.execute('SELECT generation>0 FROM background_work WHERE id=%s',(newer,)).fetchone()[0])
    assert connection.execute('SELECT processed_count FROM traffic_sample_batches WHERE sample_id=%s',(newer,)).fetchone()[0] == 0
    seal(connection,older,NODES[0],cycle,(150,100))
    wait_for(lambda: completed(connection,older) and completed(connection,newer))
    check_usage(connection,250,210)
    restart, _ = reserve(connection,NODES[0],'ready',(5,8),'boot-2')
    wait_for(lambda: completed(connection,restart))
    check_usage(connection,263,223)
    decreased, _ = reserve(connection,NODES[0],'ready',(1,2),'boot-2')
    wait_for(lambda: completed(connection,decreased))
    final_digest = check_usage(connection,263,223)
    final_order = connection.execute('SELECT sample_order FROM traffic_sample_batches WHERE sample_id=%s',(decreased,)).fetchone()[0]
    assert connection.execute("SELECT count(*) FROM traffic_baselines WHERE node_id=%s AND instance_id='boot-2' AND tx_total=1 AND rx_total=2 AND sample_order=%s",(NODES[0],final_order)).fetchone()[0] == COUNT
    assert connection.execute("SELECT count(*) FROM traffic_baselines WHERE node_id=%s AND instance_id='boot-1' AND tx_total=230 AND rx_total=120",(NODES[1],)).fetchone()[0] == COUNT
    gaps = dict(connection.execute('SELECT gap_reason,count(*) FROM traffic_records WHERE gap_reason IS NOT NULL GROUP BY gap_reason'))
    assert gaps == {'node_restarted_between_samples':COUNT,'counter_decreased_without_instance_change':COUNT}
    assert connection.execute('SELECT count(*) FROM traffic_records').fetchone()[0] == COUNT*6
    assert connection.execute('SELECT count(*) FROM execution_operations').fetchone()[0] == 0, 'captured billing attempted external mutation'
    assert connection.execute('SELECT count(*) FROM resource_leases WHERE blocked').fetchone()[0] == 0
    return {'passed':True,'users':COUNT,'captured_samples':6,'ledger_records':COUNT*6,
            'committed_checkpoint_before_kill':500,'original_generation':old_generation,'recovered_generation':new_generation,
            'kill_to_completion_seconds':recovery_seconds,'serial_reference_sha256':final_digest,'gap_reason_counts':gaps,
            'checks':['SIGKILL-during-second-billing-page','atomic-500-item-checkpoint','natural-lease-expiry-takeover','peer-node-completes-before-expiry','healthy-work-not-reset','cross-node-shared-user-atomic-addition','HTTP-quota-reset-does-not-clear-baselines','duplicate-durable-work-no-double-charge','out-of-order-capture-seals-in-reservation-order','node-restart-counts-new-counters','counter-decrease-records-gap-without-negative-charge'],
            'limitations':['already-captured batches and initial baselines are DB fixtures; no SSH capture is claimed','fixture payload digest marks stable captured input; native capture validation is covered separately','SIGKILL proves rollback/recovery, not resumed-old-process write rejection','node package manual correction and node-generation sample rejection require separate acceptance']}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server',type=Path,default=Path(__file__).resolve().parents[1]/'target/debug/hysteriax-server')
    parser.add_argument('--pg-container',default='hx-uitest-pg')
    parser.add_argument('--redis-container',default='hx-0034-reference-check')
    parser.add_argument('--report',type=Path,required=True)
    args=parser.parse_args()
    pg,redis=inspect(args.pg_container),inspect(args.redis_container)
    settings=dict(item.split('=',1) for item in pg['Config']['Env'] if '=' in item)
    user=settings.get('POSTGRES_USER','postgres')
    port=pg['NetworkSettings']['Ports']['5432/tcp'][0]['HostPort']
    params={'host':'127.0.0.1','port':port,'user':user,'password':settings['POSTGRES_PASSWORD'],'connect_timeout':5}
    name='hx0034_billing_'+uuid.uuid4().hex
    url='postgres://{}:{}@127.0.0.1:{}/{}'.format(urllib.parse.quote(user,safe=''),urllib.parse.quote(params['password'],safe=''),port,name)
    logs=Path(tempfile.mkdtemp(prefix='hx0034-billing-'))
    token=secrets.token_urlsafe(40)
    env={**os.environ,'DATABASE_URL':url,'HYSTERIAX_LISTEN_ADDR':f'127.0.0.1:{free_port()}',
         'HYSTERIAX_MASTER_KEY':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA','HYSTERIAX_ADMIN_TOKEN':token,
         'HYSTERIAX_REDIS_URL':'redis://127.0.0.1:'+redis['NetworkSettings']['Ports']['6379/tcp'][0]['HostPort']+'/', 'RUST_LOG':'warn'}
    fixture=None
    report={'scope':'three-full-process-captured-billing-fault','passed':False,'private_logs':str(logs)}
    with psycopg.connect(**params,dbname=settings.get('POSTGRES_DB',user),autocommit=True) as admin:
        admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(name)))
        try:
            with psycopg.connect(url,autocommit=True) as observer:
                def instances():
                    return [row[0] for row in observer.execute('SELECT instance_id FROM service_instances')]
                with (logs/'migration.log').open('wb') as output:
                    fixture=MultiInstanceServer(args.server.resolve(),env,logs,3,output,observe_instances=instances)
                observer.execute("SET statement_timeout='3s'")
                report.update(verify(observer,url,fixture,token))
                report['backends']=fixture.report()
                report['server_sha256']=hashlib.sha256(args.server.read_bytes()).hexdigest()
        finally:
            if fixture:
                fixture.terminate()
                try:
                    fixture.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait(timeout=5)
            admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(name)))
            args.report.parent.mkdir(parents=True,exist_ok=True)
            args.report.write_text(json.dumps(report,indent=2)+'\n')
    print('three-process billing fault verification passed',flush=True)


if __name__=='__main__':
    main()
