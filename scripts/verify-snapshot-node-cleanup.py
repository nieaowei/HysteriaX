#!/usr/bin/env python3
"""Check record-only cleanup on a disposable real snapshot, without remote workers.

Requires a current local server build and Rust test toolchain. Snapshot/key data
and subprocess diagnostics stay private; the report contains hashes and counts.
"""
import argparse,hashlib,json,os,subprocess,tempfile,time,uuid
from pathlib import Path
from urllib.parse import quote
import psycopg
from psycopg import sql
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--snapshot',type=Path,required=True)
parser.add_argument('--expected-sha256',required=True)
parser.add_argument('--key-file',type=Path,required=True)
parser.add_argument('--report',type=Path,required=True)
parser.add_argument('--pg-container',default='hx-uitest-pg')
parser.add_argument('--server',type=Path,default=Path(__file__).resolve().parents[1]/'target/debug/hysteriax-server')
args=parser.parse_args()
def file_sha(path):
 h=hashlib.sha256()
 with path.open('rb') as f:
  for chunk in iter(lambda:f.read(1024*1024),b''):h.update(chunk)
 return h.hexdigest()
if args.key_file.stat().st_mode & 0o077:parser.error('key file must be readable only by its owner')
if file_sha(args.snapshot)!=args.expected_sha256:parser.error('snapshot checksum mismatch')
logs=Path(tempfile.mkdtemp(prefix='hx0034-snapshot-cleanup-'))
repo=Path(__file__).resolve().parents[1]
def private_log(name):
 return os.fdopen(os.open(logs/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600),'wb')
pg=json.loads(subprocess.check_output(['docker','inspect',args.pg_container]))[0]
e=dict(v.split('=',1) for v in pg['Config']['Env'] if '=' in v)
user=e.get('POSTGRES_USER','postgres');base=e.get('POSTGRES_DB',user);port=pg['NetworkSettings']['Ports']['5432/tcp'][0]['HostPort']
params={'host':'127.0.0.1','port':port,'user':user,'password':e['POSTGRES_PASSWORD'],'connect_timeout':5}
name='hx_readyz_repro_'+uuid.uuid4().hex
url='postgres://'+quote(user,safe='')+':'+quote(e['POSTGRES_PASSWORD'],safe='')+'@127.0.0.1:'+port+'/'+name
key=json.loads(args.key_file.read_text())['HYSTERIAX_MASTER_KEY']
env={**os.environ,'DATABASE_URL':url,'TEST_DATABASE_URL':url,'HYSTERIAX_MASTER_KEY':key,'NODE_CLEANUP_SNAPSHOT_URL':url,'NODE_CLEANUP_SNAPSHOT_MASTER_KEY':key,'RUST_LOG':'warn'}
env.pop('HYSTERIAX_REDIS_URL',None)
report={'scope':'real-snapshot-record-only-node-cleanup','passed':False,'ordinary_workers_started':False,'snapshot_sha256':args.expected_sha256,'private_logs':str(logs),'test_source_sha256':file_sha(repo/'crates/hysteriax-server/src/node_cleanup_tests.rs'),'cleanup_source_sha256':file_sha(repo/'crates/hysteriax-server/src/node_cleanup.rs')}
def digest(connection,table,where,parameters):
 query=sql.SQL("SELECT encode(sha256(convert_to(to_jsonb(t)::text,'UTF8')),'hex') FROM {} t WHERE ").format(sql.Identifier(table))+sql.SQL(where)+sql.SQL(' ORDER BY 1')
 h=hashlib.sha256();count=0
 with connection.transaction():
  connection.execute("SET LOCAL statement_timeout='5min';SET LOCAL work_mem='16MB';SET LOCAL TIME ZONE 'UTC'")
  with connection.cursor(name='hash_'+uuid.uuid4().hex) as cursor:
   cursor.execute(query,parameters)
   while batch:=cursor.fetchmany(5000):
    for row in batch:h.update(row[0].encode()+b'\n')
    count+=len(batch)
 return {'rows':count,'sha256':h.hexdigest()}
with psycopg.connect(**params,dbname=base,autocommit=True) as admin:
 admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(name)))
 try:
  with args.snapshot.open('rb') as source,private_log('restore.log') as output:
   p=subprocess.run(['docker','exec','-i',args.pg_container,'pg_restore','-U',user,'-d',name,'--exit-on-error','--no-owner','--no-acl'],stdin=source,stdout=output,stderr=subprocess.STDOUT,timeout=1200)
  if p.returncode:raise RuntimeError('restore failed')
  with private_log('migration.log') as output:
   p=subprocess.run([str(args.server.resolve()),'--migrate-only'],env=env,stdout=output,stderr=subprocess.STDOUT,timeout=1200)
  if p.returncode:raise RuntimeError('migration failed')
  with psycopg.connect(**params,dbname=name,autocommit=True) as connection:
   connection.execute("SET TIME ZONE 'UTC'")
   target,target_rows=connection.execute('SELECT node_id,count(*) FROM traffic_records GROUP BY node_id ORDER BY count(*) DESC LIMIT 1').fetchone()
   max_event=connection.execute('SELECT max(id) FROM job_events').fetchone()[0]
   tables=['traffic_records','node_network_samples','online_samples','proxy_probe_samples','traffic_minute_totals','traffic_hour_totals','user_node_usage','probe_minute_totals']
   before={t:digest(connection,t,'node_id<>%s',(target,)) for t in tables}
   before['users']=digest(connection,'users','true',())
   users_before={row[0]:row[1:] for row in connection.execute('SELECT id,to_jsonb(u),revision,updated_at FROM users u')}
   affected_users={row[0] for row in connection.execute('SELECT user_id FROM node_assignments WHERE node_id=%s UNION SELECT gu.user_id FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id WHERE gn.node_id=%s',(target,target))}
   before['original_events']=digest(connection,'job_events','id<=%s',(max_event,))
   jobs_before=connection.execute('SELECT count(*) FROM jobs').fetchone()[0]
   original_jobs=[r[0] for r in connection.execute('SELECT id FROM jobs')]
   operations_before=digest(connection,'execution_operations','true',())
   expected_dns={r[0] for r in connection.execute("SELECT r.id FROM dns_bindings b JOIN dns_records r ON b.record_ids ? r.id WHERE b.node_id=%s AND r.origin='hysteriax' AND r.deleted_at IS NULL AND EXISTS(SELECT 1 FROM dns_operations o JOIN jobs j ON j.id=o.job_id WHERE o.resource_id=r.id AND o.action='record-create' AND j.node_id=%s) AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.id<>%s AND (n.public_host=r.name OR n.published_connection->>'public_host'=r.name OR EXISTS(SELECT 1 FROM dns_bindings other WHERE other.node_id=n.id AND other.record_ids ? r.id))) AND NOT EXISTS(SELECT 1 FROM dns_operations o WHERE o.idempotency_key='node-delete:'||%s||':'||r.id)",(target,target,target,target))}
  report.update(target_node_sha256=hashlib.sha256(target.encode()).hexdigest(),target_traffic_records=target_rows,protected_rows=before)
  env['NODE_CLEANUP_SNAPSHOT_NODE']=target
  started=time.monotonic()
  with private_log('cargo.log') as output:
   p=subprocess.run(['cargo','test','-p','hysteriax-server','jobs::node_cleanup_tests::restored_snapshot_removal_completes_while_reads_and_writes_continue','--','--ignored','--exact','--nocapture','--test-threads=1'],env=env,cwd=repo,stdout=output,stderr=subprocess.STDOUT,text=True,timeout=900)
  cargo_output=(logs/'cargo.log').read_text()
  report.update(cargo_exit=p.returncode,command_seconds=time.monotonic()-started)
  report['test_metrics']=[line for line in cargo_output.splitlines() if line.startswith('snapshot ')]
  if p.returncode:raise RuntimeError('targeted snapshot cleanup failed')
  if '1 passed; 0 failed' not in cargo_output:raise RuntimeError('targeted snapshot test did not run')
  with psycopg.connect(**params,dbname=name,autocommit=True) as connection:
   after={t:digest(connection,t,'node_id<>%s',(target,)) for t in tables}
   after['users']=digest(connection,'users','true',())
   after['original_events']=digest(connection,'job_events','id<=%s',(max_event,))
   report['protected_rows_after']=after
   assert all(before[t]==after[t] for t in before if t!='users'),'protected raw/history data changed'
   users_after={row[0]:row[1:] for row in connection.execute('SELECT id,to_jsonb(u),revision,updated_at FROM users u')}
   assert users_before.keys()==users_after.keys(),'user membership changed'
   finished=connection.execute("SELECT finished_at FROM jobs WHERE kind='record-remove' ORDER BY created_at DESC LIMIT 1").fetchone()[0]
   for user,(original,revision,updated) in users_before.items():
    current,new_revision,new_updated=users_after[user]
    # Node removal deliberately advances authorization revisions, but cannot
    # alter billing, quota, credentials, or any other original user field.
    expected=dict(original)
    if user in affected_users:
     assert new_revision==revision+1 and new_updated==finished,'authorization revision/timestamp mismatch'
     expected['revision']=new_revision
     expected['updated_at']=current['updated_at']
    assert current==expected,'unexpected user field change'
   report['user_field_checks']={'all_original_fields_preserved_except_expected_authorization_revision_and_timestamp':True,'affected_users':len(affected_users),'users':len(users_before)}
   added=connection.execute('SELECT id,kind,status,node_id,attempts FROM jobs WHERE NOT(id=ANY(%s))',(original_jobs,)).fetchall()
   report['new_jobs']=[{'kind':r[1],'status':r[2],'has_node':r[3] is not None,'attempts':r[4]} for r in added]
   report['expected_dns_cleanup_jobs']=len(expected_dns)
   report['operations_before']=operations_before
   report['operations_after']=digest(connection,'execution_operations','true',())
   assert report['operations_before']==report['operations_after'],'external execution operations changed'
   removals=[r for r in added if r[1]=='record-remove']
   assert len(removals)==1 and removals[0][2:]==('succeeded',target,1),'record cleanup job mismatch'
   followups=[r for r in added if r[1]!='record-remove']
   assert len(followups)==len(expected_dns),'unexpected follow-up jobs'
   observed=set()
   for job,kind,status,node,attempts in followups:
    assert (kind,status,node,attempts)==('dns-record-delete','queued',None,0),'follow-up must remain unexecuted'
    resource,key,applied=connection.execute('SELECT resource_id,idempotency_key,applied_at FROM dns_operations WHERE job_id=%s AND action=\'record-delete\'',(job,)).fetchone()
    assert resource in expected_dns and key=='node-delete:'+target+':'+resource and applied is None,'DNS cleanup ownership mismatch'
    observed.add(resource)
   assert observed==expected_dns,'DNS cleanup coverage mismatch'
   assert connection.execute('SELECT count(*) FROM jobs').fetchone()[0]==jobs_before+len(added),'original jobs removed'
  report['passed']=True
 except Exception as error:
  report['error_type']=type(error).__name__
  if isinstance(error,AssertionError):report['failed_check']=str(error)
 finally:
  admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(name)))
  args.report.parent.mkdir(parents=True,exist_ok=True)
  args.report.write_text(json.dumps(report,indent=2)+'\n')
  print(json.dumps(report,indent=2))
if not report['passed']:raise SystemExit(1)
