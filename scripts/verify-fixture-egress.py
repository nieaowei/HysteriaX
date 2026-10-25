#!/usr/bin/env python3
"""Verify clone-worker isolation before using a real snapshot for capacity tests."""
import argparse
import json
from pathlib import Path
import subprocess
import uuid
from fixture_network_guard import apply_guard, docker, OWNER_LABEL

SERVER = '''from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import os
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(b'ok')
ThreadingHTTPServer(('0.0.0.0',int(os.environ['PORT'])),Handler).serve_forever()
'''
PROBE = '''import json,sys,urllib.request
result=[]
for url in sys.argv[1:]:
 try:
  with urllib.request.urlopen(url,timeout=1) as response:result.append(response.status==200)
 except OSError:result.append(False)
print(json.dumps(result))
'''


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--guard-image',required=True)
    parser.add_argument('--report',type=Path,required=True)
    args=parser.parse_args()
    owner=uuid.uuid4().hex
    prefix='hx0034-egress-'+owner[:10]
    network=prefix+'-network'
    containers=[]
    network_created=False
    report={'scope':'owned-container-egress-allowlist','passed':False,'host_firewall_changed':False}
    label=OWNER_LABEL+'='+owner
    try:
        docker('network','create','--label',label,network)
        network_created=True
        def start(suffix,port=None):
            name=prefix+'-'+suffix
            command=['run','-d','--name',name,'--label',label,'--network',network,'--memory','96m','--memory-swap','96m','--cpus','.2']
            if port:
                command+=['-e','PORT='+str(port),'python:3.11-alpine','python','-u','-c',SERVER]
            else:
                command+=['--user','65534:65534','--cap-drop','ALL','--sysctl','net.ipv6.conf.all.disable_ipv6=1','python:3.11-alpine','python','-c','import time;time.sleep(300)']
            docker(*command);containers.append(name)
            return name
        pg=start('postgres-fixture',5432)
        redis=start('redis-fixture',6379)
        denied=start('denied-same-port',5432)
        client=start('client')
        addresses=[json.loads(docker('inspect',name))[0]['NetworkSettings']['Networks'][network]['IPAddress'] for name in (pg,redis,denied)]
        urls=['http://'+address+':'+str(port) for address,port in zip(addresses,(5432,6379,5432))]
        def probe():return json.loads(docker('exec',client,'python','-c',PROBE,*urls))
        # Backend startup may take a moment under a CPU limit.
        import time
        deadline=time.monotonic()+15
        while True:
            before=probe()
            if all(before):break
            if time.monotonic()>deadline:raise RuntimeError('fixture peers did not become ready')
            time.sleep(.1)
        report['before']=before
        report['guard']=apply_guard(client,pg,redis,owner=owner,image=args.guard_image)
        after=probe();report['after']=after
        if after != [True,True,False]:raise AssertionError('fixture allowlist did not enforce destination and port isolation')
        capabilities=docker('exec',client,'sh','-c',"id -u; sed -n 's/^CapEff:[[:space:]]*//p' /proc/self/status").splitlines()
        if capabilities != ['65534','0000000000000000']:raise AssertionError('fixture client has unexpected privilege')
        report.update(passed=True,unprivileged_client=True,
                      checks=['both-allowed-peers-reachable','same-port-unrelated-peer-denied','IPv6-disabled-before-guard','no-host-namespace','client-has-no-effective-capabilities'])
    finally:
        for name in reversed(containers):
            subprocess.run(['docker','rm','-f',name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        if network_created:
            subprocess.run(['docker','network','rm',network],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        args.report.parent.mkdir(parents=True,exist_ok=True)
        args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))


if __name__=='__main__':main()
