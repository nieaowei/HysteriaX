"""Offline old/new/old release verification on an owned, egress-isolated clone.

Restoring this disposable clone is permitted because every API namespace has
denied external destinations from before startup. This is not a production
restore override; live external timelines retain backup-restore.py's refusal.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess


class ReleaseRecovery:
    def __init__(self, *, pg, redis, owner, image, private, docker, sql, gated, wait):
        self.pg,self.redis,self.owner,self.image=pg,redis,owner,image
        self.private,self.docker,self.sql,self.gated,self.wait=private,docker,sql,gated,wait
        self.backup=private/'release-before-upgrade.dump'
        info=json.loads(docker('image','inspect',image).stdout)[0]
        self.report={'scope':'offline-full-snapshot-old-new-old-release-recovery',
                     'passed':False,'old_image_id':info['Id'],'old_image':image,
                     'external_effects_impossible_by_startup_egress_guards':True,
                     'production_restore_override':False}

    def private_file(self,name):
        return os.fdopen(os.open(self.private/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600),'wb')

    def fingerprints(self):
        tables=json.loads(self.sql("SELECT json_agg(tablename ORDER BY tablename) FROM pg_tables WHERE schemaname='public'"))
        result={}
        for table in tables:
            identifier='"'+table.replace('"','""')+'"'
            statement=f"COPY (SELECT encode(sha256(convert_to(to_jsonb(t)::text,'UTF8')),'hex') FROM public.{identifier} t ORDER BY 1) TO STDOUT"
            digest=hashlib.sha256();count=0
            with self.private_file('hash-'+str(len(list(self.private.glob('hash-*'))))+'.log') as error:
                process=subprocess.Popen(['docker','exec',self.pg,'psql','-X','-U','hysteriax',
                    '-d','hysteriax','-Atq','-v','ON_ERROR_STOP=1','-c',statement],
                    stdout=subprocess.PIPE,stderr=error)
                for row in process.stdout:
                    digest.update(row);count+=1
                process.stdout.close()
                if process.wait(timeout=120):
                    raise RuntimeError('private release fingerprint failed')
            result[table]={'rows':count,'sha256':digest.hexdigest()}
        sequences=json.loads(self.sql("SELECT coalesce(json_agg(sequencename ORDER BY sequencename),'[]') FROM pg_sequences WHERE schemaname='public'"))
        sequence_digest=hashlib.sha256()
        for name in sequences:
            quoted='"'+name.replace('"','""')+'"'
            sequence_digest.update((name+':'+self.sql(f'SELECT last_value::text||\':\'||is_called::text FROM public.{quoted}')+'\n').encode())
        return {'tables':result,'sequence_state_sha256':sequence_digest.hexdigest()}

    def ready(self,name):
        self.wait(lambda:self.docker('exec',name,'curl','--max-time','3','--fail','-s',
            'http://127.0.0.1:8080/readyz',check=False).returncode==0,90,'old API readiness')

    def api(self,name,path):
        # The token is already in the container environment, never a CLI argument.
        raw=self.docker('exec',name,'sh','-ec',
            'exec curl --max-time 15 --fail --silent -H "Authorization: Bearer $HYSTERIAX_ADMIN_TOKEN" "http://127.0.0.1:8080$1"',
            'fixture',path).stdout
        return json.loads(raw)

    def stop(self,name):
        self.docker('stop','-t','30',name)
        info=json.loads(self.docker('inspect',name).stdout)[0]
        if info['State']['Running']:
            raise RuntimeError('old/new API must stop before release transition')
        self.docker('rm',name)

    def before_upgrade(self):
        name,_=self.gated('old-before-upgrade',image=self.image)
        self.ready(name)
        version=self.api(name,'/api/v1/version')
        version=version.get('api_version',version.get('apiVersion'))
        if version!='1.0.0':
            raise RuntimeError('rollback baseline must expose the old v1 protocol')
        self.api(name,'/api/v1/users?page_size=20')
        self.api(name,'/api/v1/credentials?page_size=20')
        self.stop(name)
        self.report['source_schema']=int(self.sql('SELECT max(version) FROM _sqlx_migrations'))
        self.before=self.fingerprints()
        with self.private_file(self.backup.name) as output,self.private_file('release-dump.log') as error:
            result=subprocess.run(['docker','exec',self.pg,'pg_dump','-U','hysteriax','-d',
                'hysteriax','-Fc','--no-owner','--no-acl'],stdout=output,stderr=error,timeout=1200)
        if result.returncode:
            raise RuntimeError('full pre-upgrade release backup failed')
        self.report.update(old_protocol_before=version,old_api_stopped_before_migration=True,
            complete_database_backup=True,backup_bytes=self.backup.stat().st_size)

    def rollback(self,apis):
        self.report['upgraded_schema']=int(self.sql('SELECT max(version) FROM _sqlx_migrations'))
        for name in apis:
            version=self.api(name,'/api/v2/version')
            if version.get('api_version',version.get('apiVersion'))!='2.0.0':
                raise RuntimeError('new replica does not expose protocol 2')
        self.report['new_protocol_replicas_verified']=len(apis)
        for name in apis:
            self.stop(name)
        info=json.loads(self.docker('inspect',self.pg).stdout)[0]
        if (info['Config'].get('Labels') or {}).get('hysteriax.fixture-owner')!=self.owner:
            raise RuntimeError('refusing release restore outside owned fixture database')
        # Drop/create is deliberately limited to this owned PG container, after
        # every new API stopped. Restore the original complete schema, not DOWN SQL.
        for command in ['DROP DATABASE hysteriax WITH (FORCE)',
                        'CREATE DATABASE hysteriax OWNER hysteriax']:
            self.docker('exec',self.pg,'psql','-X','-U','postgres','-d','postgres',
                        '-v','ON_ERROR_STOP=1','-c',command)
        with self.backup.open('rb') as source,self.private_file('release-restore.log') as output:
            result=subprocess.run(['docker','exec','-i',self.pg,'pg_restore','-U','hysteriax',
                '-d','hysteriax','--exit-on-error','--no-owner','--no-acl'],stdin=source,
                stdout=output,stderr=subprocess.STDOUT,timeout=1200)
        if result.returncode:
            raise RuntimeError('full old-release restore failed')
        after=self.fingerprints()
        if after!=self.before:
            raise RuntimeError('complete restored table/sequence fingerprints differ')
        self.report.update(all_new_apis_stopped_before_restore=True,
            restored_schema=int(self.sql('SELECT max(version) FROM _sqlx_migrations')),
            protected_tables=len(after['tables']),full_table_fingerprints_equal=True,
            all_sequence_states_equal=True)
        self.docker('exec',self.redis,'sh','-ec',
                    'REDISCLI_AUTH="$HYSTERIAX_REDIS_PASSWORD" redis-cli FLUSHDB')
        name,_=self.gated('old-after-rollback',image=self.image)
        self.ready(name)
        version=self.api(name,'/api/v1/version')
        version=version.get('api_version',version.get('apiVersion'))
        if version!='1.0.0' or self.report['restored_schema']!=self.report['source_schema']:
            raise RuntimeError('old protocol/schema did not recover')
        self.api(name,'/api/v1/users?page_size=20')
        self.api(name,'/api/v1/credentials?page_size=20')
        self.api(name,'/api/v1/jobs?page_size=20')
        self.report.update(passed=True,old_protocol_after=version,old_readiness_recovered=True,
            old_management_reads_recovered=True,new_and_old_never_running_together=True,
            interactive_old_client_tested=False)
        return self.report
