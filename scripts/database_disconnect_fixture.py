"""Database-only billing takeover after one owned Docker API loses its DB."""
import json
import hashlib
import os
from pathlib import Path
import subprocess
import time

from fixture_network_guard import PreparedPostgresOutage


def verify(*, owner, apis, pg, redis, guard_image, docker, sql, wait, private, register_container):
    prefix = 'db-fault-'+owner
    users = prefix+'-u'
    nodes = [prefix+'-a',prefix+'-b']
    credential = prefix+'-ssh'

    def value(query):
        return json.loads(sql(query))

    # Fixture-only, unreachable SSH material: accounting resumes captured data;
    # all API namespaces already deny every destination except owned PG/Redis.
    sql(f"""BEGIN; SET LOCAL hysteriax.reference_maintenance='on';
        INSERT INTO credentials(id,kind,name,created_at,updated_at)
        VALUES('{credential}','ssh_password','DB disconnect fixture',now(),now());
        INSERT INTO credential_versions(credential_id,version,payload_enc,created_at)
        VALUES('{credential}',1,'fixture',now());
        INSERT INTO users(id,name,created_at,updated_at)
        SELECT '{users}'||lpad(i::text,4,'0'),'DB fault user',now(),now() FROM generate_series(0,511)i;
        COMMIT;""")
    for index,node in enumerate(nodes):
        sql(f"""BEGIN; SET LOCAL hysteriax.reference_maintenance='on';
            INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,
            ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,
            node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at)
            VALUES('{node}','DB fault node','127.0.0.1',1,'root','{credential}',1,
            'fixture.invalid',443,':443','{node}','fixture','fixture','fixture',now(),now());
            DELETE FROM credential_reference_dirty WHERE kind='node' AND source_id='{node}';
            INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at)
            SELECT id,'{node}',id,'fixture',now() FROM users WHERE id LIKE '{users}%';
            INSERT INTO traffic_baselines(node_id,user_id,instance_id,tx_total,rx_total,sampled_at,sample_order)
            SELECT '{node}',id,'boot-1',{(index+1)*100},{(index+1)*50},now(),0
            FROM users WHERE id LIKE '{users}%'; COMMIT;""")

    def reserve(index):
        sample = prefix+'-sample-'+str(index)
        node = nodes[index]
        tx,rx = [(120,70),(230,120)][index]
        sql(f"""BEGIN;
            INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes)
            VALUES(gen_random_uuid(),'{sample}',now(),now()+interval '10 seconds',1);
            INSERT INTO traffic_sample_batches(sample_id,node_id,cycle_id)
            SELECT '{sample}','{node}',id FROM sampling_cycles WHERE kind='{sample}';
            INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at)
            SELECT '{sample}','{node}',id,{tx},{rx},now() FROM users WHERE id LIKE '{users}%';
            UPDATE traffic_sample_batches SET instance_id='boot-1',payload_digest='{sample}',
            item_count=512,state='ready' WHERE sample_id='{sample}';
            INSERT INTO background_work(id,kind,cycle_id,node_id,deadline_at,payload)
            SELECT '{sample}','traffic',cycle_id,'{node}',clock_timestamp()+interval '10 seconds',
            '{{"fixture":true}}' FROM traffic_sample_batches WHERE sample_id='{sample}'; COMMIT;""")
        return sample

    def state(sample):
        return value(f"""SELECT row_to_json(t) FROM (SELECT b.processed_count,b.state batch_state,
            w.state work_state,w.owner,w.generation FROM traffic_sample_batches b
            JOIN background_work w ON w.id=b.sample_id WHERE b.sample_id='{sample}')t""")

    def complete(sample):
        row = state(sample)
        return row if row and (row['processed_count'],row['batch_state'],row['work_state'])==(512,'complete','complete') else None

    controllers=[]
    try:
        for index,name in enumerate(apis):
            controllers.append(PreparedPostgresOutage(name,pg,redis,owner=owner,
                image=guard_image,control=private/('db-fault-'+str(index))))
            register_container(controllers[-1].name)
    except BaseException:
        for controller in controllers:
            controller.close()
        raise


    # Hold the 501st user while the executor durably commits its first page.
    application = prefix+'-gate'
    output = private/'database-fault-gate.log'
    log = os.fdopen(os.open(output,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600),'wb')
    gate = subprocess.Popen(['docker','exec','-e','PGAPPNAME='+application,pg,
        'psql','-X','-U','hysteriax','-d','hysteriax','-Atq','-v','ON_ERROR_STOP=1','-c',
        f"BEGIN; SELECT id FROM users WHERE id='{users}0500' FOR NO KEY UPDATE; SELECT pg_sleep(240)"],
        stdout=log,stderr=subprocess.STDOUT)
    blocked = None
    gate_pid = None
    try:
        wait(lambda: int(sql(f"SELECT count(*) FROM pg_stat_activity WHERE application_name='{application}' AND wait_event='PgSleep'"))==1,15,'billing row gate')
        gate_pid = int(sql(f"SELECT pid FROM pg_stat_activity WHERE application_name='{application}'"))
        sample_a = reserve(0)
        wait(lambda: state(sample_a)['processed_count']==500,30,'durable billing checkpoint')
        deadline=time.monotonic()+30
        while True:
            holder = value(f"""SELECT coalesce(json_agg(application_name),'[]') FROM pg_stat_activity
                WHERE datname=current_database() AND {gate_pid}=ANY(pg_blocking_pids(pid))
                AND clock_timestamp()-query_start<interval '250 milliseconds'""")
            if holder:
                break
            if time.monotonic()>deadline:
                raise RuntimeError('fresh blocked billing statement deadline exceeded')
            time.sleep(.01)
        assert len(holder)==1 and holder[0].startswith(owner+'-api')
        index = int(holder[0].rsplit('api',1)[1])
        assert 0 <= index < len(apis)
        old = state(sample_a)
        expiry = sql(f"SELECT expires_at::text FROM resource_leases WHERE resource_key='work:{sample_a}'")
        started = time.monotonic()
        activated=time.monotonic()
        controllers[index].block()
        blocked = index
        terminated = int(sql(f"SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name='{holder[0]}')t"))
        assert terminated>0
        assert state(sample_a)['processed_count']==500
        assert int(sql(f"SELECT count(*) FROM traffic_records WHERE sample_id='{sample_a}'"))==500
        assert state(sample_a)['owner']==old['owner'] and state(sample_a)['generation']==old['generation']
        activation_ms=(time.monotonic()-activated)*1000
        sql(f'SELECT pg_terminate_backend({gate_pid})')
        gate.wait(timeout=10)
        sample_b = reserve(1)
        wait(lambda: complete(sample_b),30,'healthy peer billing')
        assert sql(f"SELECT clock_timestamp()<'{expiry}'::timestamptz")=='t'
        peer = state(sample_b)
        assert peer['owner'] != old['owner']
        for i,name in enumerate(apis):
            if i != index:
                assert docker('exec',name,'curl','--max-time','3','--fail','-s','http://127.0.0.1:8080/readyz',check=False).returncode==0
        wait(lambda: complete(sample_a),120,'database-disconnected owner takeover')
        new = state(sample_a)
        assert new['owner'] is None and new['generation']>old['generation']
        # Completion clears owner. Prove the old executor still cannot write,
        # rather than treating a NULL completed-owner field as a new identity.
        assert int(sql(f"SELECT count(*) FROM pg_stat_activity WHERE application_name='{holder[0]}'"))==0
        assert docker('exec',apis[index],'curl','--max-time','3','--fail','-s','http://127.0.0.1:8080/readyz',check=False).returncode!=0
        assert sql(f"SELECT clock_timestamp()>='{expiry}'::timestamptz")=='t'
        assert state(sample_b)==peer
        assert int(sql(f"SELECT count(*) FROM users WHERE id LIKE '{users}%' AND usage_bytes=90"))==512
        assert int(sql(f"SELECT count(*) FROM traffic_records WHERE sample_id IN ('{sample_a}','{sample_b}')"))==1024
        elapsed = time.monotonic()-started
        before = state(sample_a)
        heartbeat = sql(f"SELECT heartbeat_at::text FROM service_instances WHERE instance_id='{old['owner']}'")
        controllers[index].restore()
        blocked = None
        wait(lambda: sql(f"SELECT heartbeat_at>'{heartbeat}'::timestamptz FROM service_instances WHERE instance_id='{old['owner']}'")=='t',30,'same process database reconnect')
        assert docker('exec',apis[index],'curl','--max-time','3','--fail','-s','http://127.0.0.1:8080/readyz',check=False).returncode==0
        time.sleep(2)
        assert state(sample_a)==before and state(sample_b)==peer
        assert int(sql(f"SELECT count(*) FROM traffic_records WHERE sample_id IN ('{sample_a}','{sample_b}')"))==1024
        for name in apis:
            info=json.loads(docker('inspect',name).stdout)[0]
            assert info['State']['Running'] and not info['State']['OOMKilled'] and info['RestartCount']==0
        return {'passed':True,'terminated_owned_database_connections':terminated,
                'fault_activation_and_checkpoint_check_ms':activation_ms,
                'checkpoint_before_disconnect':500,'users':512,'exact_ledger_rows':1024,
                'usage_bytes_per_user':90,'recovery_seconds':elapsed,
                'natural_lease_expiry_observed':True,'higher_generation_while_old_owner_disconnected':True,
                'healthy_peer_completed_before_expiry':True,'healthy_work_not_reset':True,
                'same_old_process_reconnected_without_restart':True,'no_duplicate_after_reconnect':True,
                'fixture_source_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    finally:
        if blocked is not None:
            controllers[blocked].restore()
        if gate.poll() is None:
            if gate_pid is not None:
                sql(f'SELECT pg_terminate_backend({gate_pid})')
            gate.wait(timeout=10)
        log.close()
        for controller in controllers:
            controller.close()
