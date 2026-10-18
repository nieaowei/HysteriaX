#!/usr/bin/env python3
"""Exercise the production installation scripts on a disposable systemd node.

Uses a local fixture CA, never production ACME/DNS accounts. Run Pebble tests
separately for live HTTP/TLS/DNS ACME protocol coverage.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent

def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', default='hysteriax-acme-test-node:0003')
    parser.add_argument('--pebble-node-bin', default=os.environ.get('PEBBLE_NODE_BIN'), help='native Linux Pebble binary enables first issuance and real deadline recovery')
    parser.add_argument('--keep', action='store_true', help='keep the disposable node for diagnosis')
    args = parser.parse_args()
    arch = 'arm64' if platform.machine() in ('arm64', 'aarch64') else 'amd64'
    name = 'hx-acme-accept-' + uuid.uuid4().hex[:8]
    with tempfile.TemporaryDirectory(prefix='hx-acme-systemd-') as temporary:
        temp = Path(temporary)
        def docker(*command, **kwargs): return run(['docker', *command], **kwargs)
        def shell(script, check=True):
            result = subprocess.run(['docker','exec',name,'sh','-c',script],text=True,capture_output=True)
            if check and result.returncode: raise RuntimeError(result.stdout + result.stderr)
            return result
        def put(path, content):
            data = content if isinstance(content, bytes) else content.encode()
            subprocess.run(['docker','exec','-i',name,'sh','-c','cat > "$1"','sh',path],input=data,check=True,capture_output=True)
        try:
            docker('run','-d','--name',name,'--platform','linux/'+arch,'--privileged','--cgroupns=host','--tmpfs','/run','--tmpfs','/tmp','-v','/sys/fs/cgroup:/sys/fs/cgroup:rw',args.image)
            for _ in range(100):
                if shell('systemctl is-system-running',False).stdout.strip() in ('running','degraded'): break
                time.sleep(.1)
            asset = 'hysteria-linux-' + arch
            expected = {'arm64':'c8dc653c3ba0a28d29a26b8fa52d2086f27c0927afddce95c09965e7174e78b0','amd64':'8c7a68a906998b747a0db87586e364f995fbfddb95693ae6e2fdb68a6e920d3e'}[arch]
            with urllib.request.urlopen('https://github.com/apernet/hysteria/releases/download/app/v2.12.3/'+asset,timeout=90) as response: hy2=response.read()
            assert hashlib.sha256(hy2).hexdigest()==expected
            shell('mkdir -p /var/tmp/acme-fixtures')
            # Test keys are generated only inside the disposable container.
            shell('''cd /var/tmp/acme-fixtures
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout ca.key -out ca.crt -days 365 -subj '/CN=Isolated node test CA' -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign' >/dev/null 2>&1
for label in server server2 client; do
 openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$label.key" -out "$label.csr" -subj "/CN=$label" >/dev/null 2>&1
 if [ "$label" = client ]; then printf 'extendedKeyUsage=clientAuth\\n' > extensions; else printf 'subjectAltName=DNS:a.example,DNS:b.example\\nextendedKeyUsage=serverAuth\\n' > extensions; fi
 openssl x509 -req -in "$label.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 90 -extfile extensions -out "$label.crt" >/dev/null 2>&1
 cat ca.crt >> "$label.crt"
done''')
            shell('cp /var/tmp/acme-fixtures/ca.crt /usr/local/share/ca-certificates/hysteriax-isolated-test.crt; update-ca-certificates >/dev/null')
            rust=(ROOT/'crates/hysteriax-server/src/deployment.rs').read_text()
            unit=json.loads('"'+re.search(r'"(\[Unit\]\\nDescription=HysteriaX managed.*?\\n)"',rust).group(1)+'"').replace('{extra_capability}','')
            policy='a'*64
            component={'policy_id':policy,'state_dir':'/var/lib/hysteriax-acme/'+policy,'publish_dir':'/var/lib/hysteriax/certificates/'+policy,'acme':{'domains':['a.example','b.example'],'dir':'/var/lib/hysteriax/acme','type':'http','http':{'altPort':18080}}}
            public=component['publish_dir']
            config={'listen':'127.0.0.1:14443','auth':{'type':'password','password':'fixture'},'trafficStats':{'listen':'127.0.0.1:19780','secret':'fixture'},'tls':{'cert':'/etc/hysteriax/resources/server.crt','key':'/etc/hysteriax/resources/server.key'}}
            def stage(acme=False,bad=False):
                shell('rm -rf /tmp/hx-acme-stage; mkdir -p /tmp/hx-acme-stage/resources')
                put('/tmp/hx-acme-stage/hysteria',hy2)
                put('/tmp/hx-acme-stage/hysteriax.service',unit)
                put('/tmp/hx-acme-stage/server.yaml',json.dumps(config))
                for file in ('server.crt','server.key','ca.crt'):
                    shell(f'cp /var/tmp/acme-fixtures/{file} /tmp/hx-acme-stage/resources/{file}')
                for file,target in [('node-rollback.sh','rollback.sh'),('node-acme-prepare.sh','acme-prepare.sh')]:put('/tmp/hx-acme-stage/'+target,(ROOT/'deploy'/file).read_bytes())
                if acme:
                    put('/tmp/hx-acme-stage/hysteriax-acme',(ROOT/'dist/node-acme'/('hysteriax-acme-linux-'+arch)).read_bytes())
                    put('/tmp/hx-acme-stage/acme-LICENSES.tar',(ROOT/'dist/node-acme/LICENSES.tar').read_bytes())
                    put('/tmp/hx-acme-stage/acme.json',json.dumps(component))
                    put('/tmp/hx-acme-stage/hysteriax-acme.service',(ROOT/'deploy/hysteriax-acme.service').read_bytes())
                    put('/tmp/hx-acme-stage/acme-layout.sh',f"ACME_STORAGE='/var/lib/hysteriax/acme'\nACME_STATE='{component['state_dir']}'\nACME_PUBLISH='{public}'\n")
                if bad: put('/tmp/hx-acme-stage/hysteriax.service', re.sub(r'ExecStart=.*','ExecStart=/nonexistent',unit).replace('Type=simple','Type=exec'))
                put('/tmp/hx-acme-install.sh',"STAGE=/tmp/hx-acme-stage\n"+(ROOT/'deploy/node-install.sh').read_text())
            def apply():return shell('sh /tmp/hx-acme-install.sh')
            def health():
                for _ in range(50):
                    response=shell("curl -sf -H 'Authorization: fixture' http://127.0.0.1:19780/online",False)
                    if response.returncode==0:return
                    time.sleep(.1)
                raise RuntimeError('Hy2 statistics API did not start')
            if args.pebble_node_bin:
                put('/var/tmp/acme-fixtures/pebble',Path(args.pebble_node_bin).read_bytes())
                put('/var/tmp/acme-fixtures/ca-proxy.py',(ROOT/'scripts/fixtures/node-acme-ca.py').read_bytes())
                shell("""cd /var/tmp/acme-fixtures
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout endpoint.key -out endpoint.csr -subj '/CN=Isolated ACME endpoint' >/dev/null 2>&1
printf 'subjectAltName=DNS:acme-v02.api.letsencrypt.org,IP:127.0.0.1\\nextendedKeyUsage=serverAuth\\n' > endpoint.extensions
openssl x509 -req -in endpoint.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 90 -extfile endpoint.extensions -out endpoint.crt >/dev/null 2>&1
printf '\\n127.0.0.1 acme-v02.api.letsencrypt.org a.example b.example c.example\\n' >> /etc/hosts
chmod 0755 pebble
""")
                put('/var/tmp/acme-fixtures/pebble.json',json.dumps({'pebble':{'listenAddress':'127.0.0.1:14000','managementListenAddress':'127.0.0.1:15000','certificate':'/var/tmp/acme-fixtures/endpoint.crt','privateKey':'/var/tmp/acme-fixtures/endpoint.key','httpPort':18080,'tlsPort':18443,'retryAfter':{'authz':1,'order':1}}}))
                shell('systemd-run --unit=hx-isolated-ca-proxy python3 /var/tmp/acme-fixtures/ca-proxy.py; systemd-run --unit=hx-isolated-ca --setenv=PEBBLE_VA_NOSLEEP=1 --setenv=PEBBLE_WFE_NONCEREJECT=0 --setenv=PEBBLE_AUTHZREUSE=0 /var/tmp/acme-fixtures/pebble -config /var/tmp/acme-fixtures/pebble.json -dnsserver 127.0.0.1:1053')
                for _ in range(100):
                    root=shell('curl -sf --cacert /var/tmp/acme-fixtures/ca.crt https://127.0.0.1:15000/roots/0',False)
                    if root.returncode==0 and 'BEGIN CERTIFICATE' in root.stdout:break
                    time.sleep(.1)
                else:raise RuntimeError('isolated CA did not start')
                put('/usr/local/share/ca-certificates/hysteriax-pebble-test.crt',root.stdout)
                shell('update-ca-certificates >/dev/null')
                assert shell('getent ahostsv4 acme-v02.api.letsencrypt.org').stdout.startswith('127.0.0.1')
                directory=json.loads(shell('curl -sf --noproxy "*" --cacert /var/tmp/acme-fixtures/ca.crt https://acme-v02.api.letsencrypt.org/directory').stdout)
                assert all(directory[key].startswith('https://acme-v02.api.letsencrypt.org/') for key in ('newNonce','newAccount','newOrder'))
                manual=json.loads(json.dumps(config))
                config['tls']={'cert':public+'/current/cert.pem','key':public+'/current/key.pem'}
                stage(True);apply();health()
                state=json.loads(shell('/opt/hysteriax/hysteriax-acme status /etc/hysteriax/acme.json').stdout)
                assert state['state']=='ready' and state['last_success'] and len(state['domains'])==2
                print('PASS: first ACME deployment issues a real SAN through the isolated CA',flush=True)
                original_component=json.loads(json.dumps(component)); original_public=public
                good_digest=shell('sha256sum /etc/hysteriax/config.yaml').stdout
                component['policy_id']='b'*64
                component['state_dir']='/var/lib/hysteriax-acme/'+component['policy_id']
                public='/var/lib/hysteriax/certificates/'+component['policy_id'];component['publish_dir']=public
                component['acme']['domains'].append('c.example')
                config['tls']={'cert':public+'/current/cert.pem','key':public+'/current/key.pem'}
                shell('touch /var/tmp/acme-fixtures/delay-ca')
                stage(True);started=time.monotonic()
                failed=shell('sh /tmp/hx-acme-install.sh',False)
                elapsed=time.monotonic()-started
                assert failed.returncode!=0 and 170 <= elapsed < 210, (elapsed,failed.stdout,failed.stderr)
                health();assert shell('sha256sum /etc/hysteriax/config.yaml').stdout==good_digest
                shell('systemctl is-active --quiet hysteriax-acme.service; rm -f /var/tmp/acme-fixtures/delay-ca')
                print('PASS: real 175-second issuance cancellation restores previous certificate policy and services',flush=True)
                public=original_public;component=original_component;config=manual
                uninstall=json.loads('"'+re.search(r'let script = "(set -eu\\nROOT=/opt/hysteriax.*?\\ngroupdel hysteriax \|\| true\\n)"',rust).group(1)+'"')
                put('/var/tmp/acme-initial-uninstall.sh',uninstall);shell('sh /var/tmp/acme-initial-uninstall.sh')
            stage();apply();health()
            print('PASS: initial manual TLS installation',flush=True)
            # A compatible CertMagic SAN cache is owned by the original node user.
            shell('mkdir -p /var/lib/hysteriax/acme/certificates/test/a.example; cp /var/tmp/acme-fixtures/server.crt /var/lib/hysteriax/acme/certificates/test/a.example/a.example.crt; cp /var/tmp/acme-fixtures/server.key /var/lib/hysteriax/acme/certificates/test/a.example/a.example.key; chown -R hysteriax:hysteriax /var/lib/hysteriax/acme')
            config['tls']={'cert':public+'/current/cert.pem','key':public+'/current/key.pem','clientCA':'/etc/hysteriax/resources/ca.crt'}
            stage(True);apply();health()
            shell('systemctl is-active --quiet hysteriax-acme.service')
            assert shell('runuser -u hysteriax -- test -r '+public+'/current/key.pem',False).returncode==0
            assert shell('runuser -u hysteriax -- test -r /etc/hysteriax/acme.json',False).returncode!=0
            assert shell('runuser -u hysteriax -- test -r '+component['state_dir']+'/status.json',False).returncode!=0
            assert shell('runuser -u hysteriax -- test -r /var/lib/hysteriax/acme',False).returncode!=0
            print('PASS: SAN migration, ACME service, mTLS config and private/read-only permissions',flush=True)
            invocation=shell('systemctl show hysteriax.service -p InvocationID --value').stdout
            stage(True)
            put('/var/tmp/acme-resume.sh',"STAGE=/tmp/hx-acme-stage\n"+(ROOT/'deploy/node-acme-resume.sh').read_text())
            previous=shell('sha256sum /opt/hysteriax/config.previous').stdout
            shell('systemctl stop hysteriax-acme.service; sh /var/tmp/acme-resume.sh');health()
            assert shell('sha256sum /opt/hysteriax/config.previous').stdout==previous
            assert shell('systemctl show hysteriax.service -p InvocationID --value').stdout==invocation
            print('PASS: certificate process restart leaves Hy2 running',flush=True)
            # Validate both SAN names with the unmodified release and mTLS+ECH.
            shell('/opt/hysteriax/hysteria ech --public-name decoy.example --output /var/tmp/acme-fixtures/ech.pem')
            shell('install -o root -g hysteriax -m 0640 /var/tmp/acme-fixtures/ech.pem /etc/hysteriax/resources/ech.pem')
            config['ech']={'keyPath':'/etc/hysteriax/resources/ech.pem'}
            stage(True);apply();health()
            ech=shell("sed -n '/BEGIN ECH CONFIGS/,/END ECH CONFIGS/p' /var/tmp/acme-fixtures/ech.pem | sed '1d;$d' | tr -d '\\n'").stdout
            def transfer(domain):
                client={'server':'127.0.0.1:14443','auth':'fixture','tls':{'sni':domain,'ca':'/var/tmp/acme-fixtures/ca.crt','clientCertificate':'/var/tmp/acme-fixtures/client.crt','clientKey':'/var/tmp/acme-fixtures/client.key','ech':ech},'tcpForwarding':[{'listen':'127.0.0.1:19000','remote':'127.0.0.1:19780'}]}
                put('/var/tmp/acme-fixtures/client.json',json.dumps(client))
                shell("systemctl stop hx-acme-client.service 2>/dev/null || true; systemctl reset-failed hx-acme-client.service 2>/dev/null || true; systemd-run --unit=hx-acme-client /opt/hysteriax/hysteria client -c /var/tmp/acme-fixtures/client.json")
                for _ in range(60):
                    if shell("curl -sf --max-time 1 -H 'Authorization: fixture' http://127.0.0.1:19000/online",False).returncode==0:return
                    time.sleep(.2)
                raise RuntimeError('SAN/mTLS/ECH proxy failed: '+shell('journalctl -u hx-acme-client -n 20 --no-pager',False).stdout)
            transfer('a.example');transfer('b.example')
            print('PASS: original Hy2, both SAN domains, mTLS and ECH proxy traffic',flush=True)
            # Exercise the same durable publication recovery used after an interrupted
            # renewal while original Hy2 clients establish concurrent handshakes.
            shell('systemctl stop hysteriax-acme.service')
            invocation=shell('systemctl show hysteriax.service -p InvocationID --value').stdout
            fingerprint=shell("openssl x509 -in /var/tmp/acme-fixtures/server2.crt -noout -fingerprint -sha256").stdout.strip().split('=')[-1].replace(':','').lower()
            generation=public+'/versions/'+fingerprint
            shell(f'mkdir -p {generation}; cp /var/tmp/acme-fixtures/server2.crt {generation}/cert.pem; cp /var/tmp/acme-fixtures/server2.key {generation}/key.pem; chown -R hysteriax-acme:hysteriax {generation}; chmod 2750 {generation}; chmod 0640 {generation}/*.pem')
            stress_code = """import json,subprocess,time
base=json.load(open('/var/tmp/acme-fixtures/client.json'))
for batch in range(4):
 processes=[]
 try:
  for index in range(6):
   base['tcpForwarding'][0]['listen']='127.0.0.1:'+str(19100+index)
   base['tls']['sni']='a.example' if index%2==0 else 'b.example'
   path='/var/tmp/acme-fixtures/stress-'+str(index)+'.json'
   json.dump(base,open(path,'w'))
   processes.append(subprocess.Popen(['/opt/hysteriax/hysteria','client','-c',path],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
  for index in range(6):
   for attempt in range(60):
    result=subprocess.run(['curl','-sf','--max-time','1','-H','Authorization: fixture','http://127.0.0.1:'+str(19100+index)+'/online'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    if result.returncode==0:break
    time.sleep(.05)
   else:raise RuntimeError('concurrent SAN/mTLS/ECH handshake failed')
 finally:
  for process in processes:process.terminate()
  for process in processes:process.wait()
"""
            put('/var/tmp/acme-fixtures/stress.py',stress_code)
            stress=subprocess.Popen(['docker','exec',name,'python3','/var/tmp/acme-fixtures/stress.py'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            time.sleep(.1)
            put(component['state_dir']+'/publish.json',json.dumps(fingerprint))
            shell('chown hysteriax-acme:hysteriax-acme '+component['state_dir']+'/publish.json; chmod 0600 '+component['state_dir']+'/publish.json')
            shell('setpriv --reuid=hysteriax-acme --regid=hysteriax-acme --init-groups --inh-caps=+net_bind_service --ambient-caps=+net_bind_service --bounding-set=-all,+net_bind_service --no-new-privs /opt/hysteriax/hysteriax-acme ensure /etc/hysteriax/acme.json')
            out,err=stress.communicate(timeout=60)
            assert stress.returncode==0,out+err
            assert shell('systemctl show hysteriax.service -p InvocationID --value').stdout==invocation
            assert shell("curl -sf --max-time 2 -H 'Authorization: fixture' http://127.0.0.1:19000/online",False).returncode==0
            assert shell('readlink '+public+'/current').stdout.strip().endswith(fingerprint)
            shell('systemctl start hysteriax-acme.service')
            print('PASS: atomic certificate rotation with concurrent handshakes and an existing mTLS+ECH connection; Hy2 did not restart',flush=True)
            before=shell('sha256sum /etc/hysteriax/config.yaml').stdout
            stage(True,bad=True)
            assert shell('sh /tmp/hx-acme-install.sh',False).returncode!=0
            health();assert shell('sha256sum /etc/hysteriax/config.yaml').stdout==before
            shell('systemctl is-active --quiet hysteriax-acme.service')
            print('PASS: service-switch failure restores component, config and services',flush=True)
            config.pop('ech');config['tls']={'cert':'/etc/hysteriax/resources/server.crt','key':'/etc/hysteriax/resources/server.key'}
            stage();apply();health()
            assert shell('systemctl is-active --quiet hysteriax-acme.service',False).returncode!=0
            assert shell('test -d '+public).returncode==0
            print('PASS: switch back to manual TLS stops certificate service and retains recovery materials',flush=True)
            # Run the exact management uninstall script, preserving an external sentinel.
            shell('touch /var/tmp/hysteriax-external-sentinel')
            uninstall=json.loads('"'+re.search(r'let script = "(set -eu\\nROOT=/opt/hysteriax.*?\\ngroupdel hysteriax \|\| true\\n)"',rust).group(1)+'"')
            put('/var/tmp/acme-uninstall.sh',uninstall)
            shell('sh /var/tmp/acme-uninstall.sh')
            assert shell('test -e /opt/hysteriax',False).returncode != 0
            assert shell('test -e /var/lib/hysteriax-acme',False).returncode != 0
            assert shell('getent passwd hysteriax-acme',False).returncode != 0
            shell('test -e /var/tmp/hysteriax-external-sentinel')
            print('PASS: full managed uninstall preserves unrelated files',flush=True)
            print('Systemd node ACME acceptance passed.',flush=True)
        finally:
            if args.keep: print('Disposable node kept for diagnosis:',name)
            else: subprocess.run(['docker','rm','-f',name],capture_output=True)

if __name__=='__main__':main()
