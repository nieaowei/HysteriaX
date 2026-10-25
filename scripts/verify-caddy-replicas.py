#!/usr/bin/env python3
"""Exercise the production Caddy config against isolated Docker DNS replicas."""
import argparse
import collections
import http.client
import hashlib
import json
from pathlib import Path
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
BACKEND = r'''
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,threading,time
mutations=0
lock=threading.Lock()
class Handler(BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def log_message(self,*args):pass
 def do_GET(self):
  if self.path=='/events':
   self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length','33');self.end_headers()
   self.wfile.write(b'data: first\n\n');self.wfile.flush();time.sleep(.2);self.wfile.write(b'data: second event\n\n');self.wfile.flush();return
  body=json.dumps({'instance':os.environ['INSTANCE'],'mutations':mutations}).encode()
  self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def do_POST(self):
  global mutations
  self.rfile.read(int(self.headers.get('Content-Length','0')))
  with lock:mutations+=1
  self.close_connection=True
  self.connection.shutdown(2)
ThreadingHTTPServer(('0.0.0.0',8080),Handler).serve_forever()
'''
READY = '''import time,urllib.request
deadline=time.monotonic()+10
while True:
 try:
  urllib.request.urlopen('http://127.0.0.1:8080/readyz',timeout=.5).read();break
 except OSError:
  if time.monotonic()>deadline:raise
  time.sleep(.1)
'''


def run(*args, check=True):
    return subprocess.run(['docker', *args], check=check, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    prefix = 'hx0034-caddy-' + uuid.uuid4().hex[:10]
    network, ingress, caddy = prefix + '-network', prefix + '-ingress', prefix + '-proxy'
    containers = []
    report = {'scope':'production-caddy-config-docker-dns', 'passed':False,
              'real_api_backends':False, 'config_sha256':hashlib.sha256((ROOT/'deploy/caddy/Caddyfile').read_bytes()).hexdigest()}
    connection = None
    try:
        run('network', 'create', '--internal', network)
        run('network', 'create', ingress)
        def backend(index):
            name = prefix + '-backend-' + str(index)
            run('run','-d','--name',name,'--network',network,'--network-alias','api',
                '--memory','48m','--memory-swap','48m','--cpus','.15',
                '-e','INSTANCE=backend-'+str(index),'python:3.11-alpine','python','-u','-c',BACKEND)
            containers.append(name)
            run('exec',name,'python','-c',READY)
            return name
        for index in range(1,4):backend(index)
        run('run','-d','--name',caddy,'--network',ingress,'--memory','64m','--memory-swap','64m','--cpus','.3',
            '-p','127.0.0.1::80','-e','HYSTERIAX_DOMAIN=http://:80',
            '-v',str(ROOT/'deploy/caddy/Caddyfile')+':/etc/caddy/Caddyfile:ro','caddy:2.10')
        containers.append(caddy)
        run('network','connect',network,caddy)
        report['caddy_version']=run('exec',caddy,'caddy','version').stdout.strip()
        deadline=time.monotonic()+20
        while True:
            info=json.loads(run('inspect',caddy).stdout)[0]
            ports=info['NetworkSettings']['Ports'].get('80/tcp')
            if ports:break
            if not info['State']['Running'] or time.monotonic()>deadline:
                report['proxy_state']={key:info['State'][key] for key in ('Status','ExitCode','OOMKilled')}
                raise RuntimeError('proxy did not publish its HTTP port')
            time.sleep(.1)
        binding=ports[0]
        connection=http.client.HTTPConnection('127.0.0.1',int(binding['HostPort']),timeout=5)
        def get(path='/readyz'):
            connection.request('GET',path)
            response=connection.getresponse()
            body=response.read()
            if response.status!=200:raise RuntimeError('proxy GET failed with HTTP '+str(response.status))
            return json.loads(body)
        deadline=time.monotonic()+20
        while True:
            try:get();break
            except (OSError,http.client.HTTPException,RuntimeError):
                if time.monotonic()>deadline:raise
                time.sleep(.2)
        started=time.monotonic()
        hits=collections.Counter(get()['instance'] for _ in range(60))
        report['initial_hits']=dict(hits)
        report['distribution_seconds']=time.monotonic()-started
        # A refreshed DNS answer may reorder the same peers mid-cycle. Require
        # every replica and bounded skew, rather than an invariant peer order.
        if set(hits)!={'backend-1','backend-2','backend-3'} or max(hits.values())-min(hits.values())>4:
            raise AssertionError('persistent client distribution: '+str(dict(hits)))
        run('stop','--time','1',containers[0])
        recovered=collections.Counter(get()['instance'] for _ in range(30))
        if recovered.get('backend-1',0):raise AssertionError('stopped replica served a response')
        report['after_stop_hits']=dict(recovered)
        backend(4)
        deadline=time.monotonic()+15
        while True:
            if get()['instance']=='backend-4':break
            if time.monotonic()>deadline:raise AssertionError('new Docker DNS replica was not discovered')
            time.sleep(.1)
        connection.request('POST','/lost',body=b'one mutation',headers={'Idempotency-Key':'fixture-one-mutation'})
        response=connection.getresponse();response.read()
        if response.status!=502:raise AssertionError('lost response was not reported as upstream failure')
        total=0
        for name in containers:
            if name in (caddy,containers[0]):continue
            result=run('exec',name,'python','-c',"import urllib.request;print(urllib.request.urlopen('http://127.0.0.1:8080/readyz').read().decode())")
            total+=json.loads(result.stdout)['mutations']
        if total!=1:raise AssertionError('proxy replayed a sent POST after response loss')
        report['lost_post_mutations']=total
        started=time.monotonic();connection.request('GET','/events');response=connection.getresponse()
        if response.status!=200 or response.readline()!=b'data: first\n':raise AssertionError('SSE first record was altered')
        first=time.monotonic()-started
        response.read()
        if first>1:raise AssertionError('SSE first record was buffered')
        report.update(passed=True,sse_first_record_seconds=first,
                      checks=['production-config-valid','Docker-DNS-all-replicas','persistent-client-round-robin',
                              'stopped-replica-failover','new-replica-discovery','sent-POST-not-replayed','SSE-first-record-forwarded'],
                      limitations=['lightweight HTTP backends; full HysteriaX accounting/authentication is covered by separate acceptance'])
    finally:
        if connection:connection.close()
        if caddy in containers:
            log=args.report.with_suffix('.caddy.log')
            log.parent.mkdir(parents=True,exist_ok=True)
            log.write_text(run('logs',caddy,check=False).stderr)
            log.chmod(0o600)
        for name in reversed(containers):run('rm','-f',name,check=False)
        run('network','rm',network,check=False)
        run('network','rm',ingress,check=False)
        args.report.parent.mkdir(parents=True,exist_ok=True)
        args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))


if __name__=='__main__':main()
