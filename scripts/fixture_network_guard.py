"""Install an outbound allowlist only in an owned test container's namespace."""
import ipaddress
import json
import re
import subprocess
import time

OWNER_LABEL = 'hysteriax.fixture-owner'


def docker(*args):
    return subprocess.run(['docker', *args], check=True, capture_output=True, text=True).stdout


def owned_container(name, owner):
    value = json.loads(docker('inspect', name))[0]
    if (value['Config'].get('Labels') or {}).get(OWNER_LABEL) != owner:
        raise RuntimeError('refusing a container not owned by this fixture')
    host = value['HostConfig']
    if host['NetworkMode'] in ('host', 'none') or host['NetworkMode'].startswith('container:') or host['Privileged']:
        raise RuntimeError('fixture target must have its own non-host network namespace')
    networks = value['NetworkSettings']['Networks']
    if not networks:
        raise RuntimeError('fixture target has no owned network')
    for name in networks:
        network = json.loads(docker('network', 'inspect', name))[0]
        if (network.get('Labels') or {}).get(OWNER_LABEL) != owner:
            raise RuntimeError('refusing an unowned fixture network')
    return value


def apply_guard(target, postgres, redis, *, owner, image):
    if not re.fullmatch('[0-9a-f]{32}', owner):
        raise RuntimeError('fixture owner must be a fresh UUID hex value')
    values = [owned_container(name, owner) for name in (target, postgres, redis)]
    networks = [value['NetworkSettings']['Networks'] for value in values]
    common = set(networks[0]) & set(networks[1]) & set(networks[2])
    if len(common) != 1:
        raise RuntimeError('fixture peers must share exactly one owned network')
    network = common.pop()
    addresses = [str(ipaddress.IPv4Address(value[network]['IPAddress'])) for value in networks[1:]]
    result = docker('run', '--rm', '--network', 'container:' + values[0]['Id'],
                    '--cap-add', 'NET_ADMIN', '--memory', '64m', '--memory-swap', '64m', image, *addresses)
    if '-P OUTPUT DROP' not in result:
        raise RuntimeError('fixture outbound default-deny rule is missing')
    return {'default_deny':True,'postgres_port':5432,'redis_port':6379,'ipv6_disabled':True,'target_namespace_owned':True}


def set_postgres_available(target, postgres, redis, *, owner, image, available):
    """Inject/remove one owned API's DB outage without changing its peers."""
    if not re.fullmatch('[0-9a-f]{32}', owner):
        raise RuntimeError('fixture owner must be a fresh UUID hex value')
    values = [owned_container(name, owner) for name in (target, postgres, redis)]
    networks = [value['NetworkSettings']['Networks'] for value in values]
    common = set(networks[0]) & set(networks[1]) & set(networks[2])
    if len(common) != 1:
        raise RuntimeError('fixture peers must share exactly one owned network')
    address = str(ipaddress.IPv4Address(networks[1][common.pop()]['IPAddress']))
    docker('run','--rm','--network','container:'+values[0]['Id'],
           '--cap-add','NET_ADMIN','--memory','64m','--memory-swap','64m',
           '--entrypoint','iptables',image,'-D' if available else '-I','OUTPUT',
           '-d',address,'-p','tcp','--dport','5432','-m','comment','--comment',
           'hx-db-fault-'+owner,'-j','REJECT','--reject-with','tcp-reset')


class PreparedPostgresOutage:
    """Prepare ownership checks before timing-sensitive fault injection."""
    def __init__(self,target,postgres,redis,*,owner,image,control):
        if not re.fullmatch('[0-9a-f]{32}',owner):
            raise RuntimeError('fixture owner must be a fresh UUID hex value')
        values=[owned_container(name,owner) for name in (target,postgres,redis)]
        networks=[v['NetworkSettings']['Networks'] for v in values]
        common=set(networks[0]) & set(networks[1]) & set(networks[2])
        if len(common)!=1:
            raise RuntimeError('fixture peers must share exactly one owned network')
        address=str(ipaddress.IPv4Address(networks[1][common.pop()]['IPAddress']))
        self.owner,self.control=owner,control
        self.name='hx-db-fault-'+owner+'-'+values[0]['Id'][:12]
        control.mkdir(mode=0o700)
        script='''set -eu
touch /control/ready
while [ ! -f /control/block ]; do
  [ ! -f /control/cancel ] || exit 0
  sleep .01
done
iptables -I OUTPUT 1 -d "$1" -p tcp --dport 5432 -m comment --comment "$2" -j REJECT --reject-with tcp-reset
touch /control/blocked
while [ ! -f /control/restore ]; do sleep .01; done
iptables -D OUTPUT -d "$1" -p tcp --dport 5432 -m comment --comment "$2" -j REJECT --reject-with tcp-reset
touch /control/restored
'''
        docker('run','--rm','-d','--name',self.name,'--label',OWNER_LABEL+'='+owner,
               '--network','container:'+values[0]['Id'],'--cap-add','NET_ADMIN',
               '--memory','64m','--memory-swap','64m','-v',str(control)+':/control',
               '--entrypoint','/bin/sh',image,'-ec',script,'fault',address,'hx-db-fault-'+owner)
        try:
            self.wait_marker('ready')
        except BaseException:
            self.close()
            raise

    def wait_marker(self,name):
        deadline=time.monotonic()+10
        while not (self.control/name).exists():
            if time.monotonic()>deadline:
                raise RuntimeError('prepared database outage helper deadline exceeded')
            time.sleep(.01)

    def block(self):
        (self.control/'block').touch(mode=0o600)
        self.wait_marker('blocked')

    def restore(self):
        (self.control/'restore').touch(mode=0o600)
        self.wait_marker('restored')

    def close(self):
        if (self.control/'blocked').exists() and not (self.control/'restored').exists():
            self.restore()
        (self.control/'cancel').touch(mode=0o600)
        result=subprocess.run(['docker','inspect',self.name],capture_output=True,text=True)
        if result.returncode==0:
            info=json.loads(result.stdout)[0]
            if (info['Config'].get('Labels') or {}).get(OWNER_LABEL)!=self.owner:
                raise RuntimeError('refusing to remove unowned fault helper')
            docker('rm','-f',self.name)
