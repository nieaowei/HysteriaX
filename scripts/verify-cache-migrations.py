#!/usr/bin/env python3
"""Verify populated schema 0015 -> latest backfills in a disposable schema."""
from pathlib import Path
from uuid import UUID
from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parent.parent


def main():
    migrations = sorted((ROOT / 'crates/hysteriax-server/migrations').glob('*.sql'))
    with PostgresTestSchema() as database, database.connect() as connection:
        for migration in migrations:
            if int(migration.name.split('_')[0]) <= 15:
                connection.execute(migration.read_text())
        connection.execute("INSERT INTO users(id,name,usage_bytes,created_at,updated_at) VALUES('u','User',123,now(),now())")
        connection.execute("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES('n','Node','localhost',22,'root','password','unused','example.test',443,':443','hash','unused','unused','unused',now(),now())")
        connection.execute("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('v','n',1,'unused','hash',now())")
        connection.execute("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('a','n','u','instance',0,0,10,20,'2026-10-01T00:00:01Z'),('b','n','u','instance',0,0,30,40,'2026-10-01T00:00:02Z')")
        connection.execute("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at) VALUES('n','period',70,80,'2026-10-01T00:00:03Z')")
        cycle = UUID('00000000-0000-0000-0000-000000000001')
        connection.execute("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes,traffic_status) VALUES(%s,'n','2026-10-01T00:00:04Z','ok','{\"u\":2}','2026-10-01T00:00:04Z',1,'ok')", (cycle,))
        connection.execute("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status,external_status,latency_ms) VALUES('n',1,'2026-10-01T00:00:05Z','ok','ok',25),('n',1,'2026-10-01T00:00:06Z','failed','failed',NULL)")
        connection.execute("INSERT INTO jobs(id,node_id,node_name,kind,status,stage,payload_json,available_at,created_at,updated_at,finished_at,error_message) VALUES('root','n','Node','kick','failed','failed','{\"node_limit\":true}',now(),now(),now(),now(),'initial')")
        connection.execute("INSERT INTO jobs(id,node_id,node_name,kind,status,stage,retry_of_job_id,available_at,created_at,updated_at) VALUES('retry','n','Node','kick','queued','queued','root',now(),now(),now())")
        connection.execute("INSERT INTO jobs(id,node_id,node_name,kind,status,stage,payload_json,available_at,created_at,updated_at,finished_at) VALUES('orphan','deleted-node','Deleted','kick','failed','failed','{\"node_limit\":true}',now(),now(),now(),now())")
        connection.commit()
        for migration in migrations:
            if int(migration.name.split('_')[0]) > 15:
                connection.execute(migration.read_text())
                connection.commit()
        assert connection.execute("SELECT source,tx::bigint,rx::bigint,samples FROM traffic_minute_totals ORDER BY source").fetchall() == [('network',70,80,1),('users',40,60,2)]
        assert connection.execute("SELECT tx::bigint,rx::bigint,samples FROM user_node_usage").fetchone() == (40,60,2)
        assert connection.execute("SELECT attempts,successes FROM probe_minute_totals").fetchone() == (2,1)
        assert connection.execute("SELECT kind,source_id FROM credential_reference_dirty ORDER BY kind").fetchall() == [('history','v'),('node','n')]
        assert connection.execute("SELECT cycle_id FROM analytics_dirty_cycles").fetchone() == (cycle,)
        query = (ROOT / 'crates/hysteriax-server/src/analytics/refresh_cycles.sql').read_text().replace('$1','%s')
        connection.execute(query, ([cycle],))
        assert connection.execute("SELECT users,connections::bigint,covered,expected,traffic_covered FROM online_cycle_metrics WHERE scope_node_id=''").fetchone() == (1,2,1,1,1)
        assert connection.execute("SELECT source,tx::bigint,rx::bigint FROM traffic_hour_totals ORDER BY source").fetchall() == [('network',70,80),('users',40,60)]
        assert connection.execute("SELECT status,total FROM job_status_totals ORDER BY status").fetchall() == [('failed',2),('queued',1)]
        assert connection.execute("SELECT pending,pending_limit,failed_limit FROM job_node_kick_totals WHERE node_id='n'").fetchone() == (1,0,1)
        connection.execute((ROOT/'crates/hysteriax-server/src/analytics/refresh_rollups.sql').read_text())
        connection.execute((ROOT/'crates/hysteriax-server/src/analytics/refresh_retries.sql').read_text().replace('$1','%s'), (['root'],))
        assert connection.execute("SELECT status,last_error FROM job_retry_chain_state").fetchone() == ('queued','initial')
        assert connection.execute("SELECT users_sum::bigint,user_cycles FROM online_minute_totals WHERE scope_node_id=''").fetchone() == (1,1)
        connection.execute("UPDATE traffic_records SET delta_tx=50 WHERE id='a'")
        assert connection.execute("SELECT tx::bigint FROM user_node_usage").fetchone() == (80,)
        assert connection.execute("SELECT usage_bytes FROM users WHERE id='u'").fetchone() == (123,)
        assert connection.execute("SELECT count(*) FROM traffic_dirty_hours").fetchone()[0]>0
        connection.execute((ROOT/'crates/hysteriax-server/src/analytics/refresh_rollups.sql').read_text())
        assert connection.execute("SELECT tx::bigint FROM traffic_hour_totals WHERE source='users'").fetchone() == (80,)
        connection.commit()
    print('Populated 0015 -> latest upgrade passed: backfills, dirty queues, online rebuild, subsequent updates and unchanged billing.')


if __name__ == '__main__':
    main()
