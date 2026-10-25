"""Install an outbound allowlist only in an owned test container's namespace."""
import ipaddress
import json
import re
import subprocess

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
