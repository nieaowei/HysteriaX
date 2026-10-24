"""Real SSH network reads held across API usage correction on three services."""
import shlex
import subprocess
import time
import uuid

from process_fault_test_fixture import wait_for


def run_network_correction(database, services, nodes, expect):
    node, peer = nodes[:2]
    container = node['container']
    results = []

    def rows(query, parameters=()):
        with database.connect() as connection:
            return connection.execute(query, parameters).fetchall()

    def shell(script, check=True):
        return subprocess.run(['docker','exec',container,'sh','-c',script],check=check,
                              capture_output=True,text=True,timeout=10)

    for invalid, correction, reset in ((False,777,False),(True,888,True)):
        control = '/var/tmp/hx0034-network-' + uuid.uuid4().hex
        installed = False
        try:
            shell(f'test ! -e /usr/local/bin/cat && mkdir -m 700 {shlex.quote(control)}')
            installed = True
            shim = f'''#!/bin/sh
set -eu
control={shlex.quote(control)}
case "${{1:-}}" in
/sys/class/net/*/statistics/tx_bytes)
    value=$(/usr/bin/cat "$1")
    if mkdir "$control/first" 2>/dev/null; then
        : > "$control/entered"
        while [ ! -f "$control/release" ]; do sleep 0.02; done
        : > "$control/returned"
        {'printf invalid-network-counter' if invalid else 'printf %s "$value"'}
        exit 0
    fi
    # Keep later legitimate reads from obscuring the late-result assertion.
    while [ ! -f "$control/release_all" ]; do sleep 0.02; done
    ;;
esac
exec /usr/bin/cat "$@"
'''
            subprocess.run(['docker','exec','-i',container,'sh','-c',
                            'cat > /usr/local/bin/cat; chmod 755 /usr/local/bin/cat'],
                           input=shim,text=True,check=True,capture_output=True,timeout=10)
            entered_after = rows('SELECT clock_timestamp()')[0][0]
            wait_for(lambda: shell(f'test -f {shlex.quote(control+"/entered")}',False).returncode==0,
                     'real network SSH read did not enter the held cat',25)
            active = rows("SELECT w.id,w.owner,w.generation,(w.payload->>'capture_generation')::bigint,(w.payload->>'captured_at')::timestamptz FROM background_work w JOIN resource_leases l ON l.resource_key='node:'||w.node_id AND l.work_id=w.id AND l.owner=w.owner WHERE w.node_id=%s AND w.kind='network' AND w.state='running'",(node['node_id'],))
            if len(active)!=1 or active[0][1] not in services.instance_ids:
                raise AssertionError('held SSH read is not mapped to one owned network executor')
            work, owner, lease_generation, generation, captured_at = active[0]
            wait_for(lambda: rows('SELECT EXISTS(SELECT 1 FROM node_network_samples WHERE node_id=%s AND sampled_at>%s)',(peer['node_id'],entered_after))[0][0],
                     'peer network sampling stopped behind another node read',15)
            previous = rows('SELECT desired_revision FROM nodes WHERE id=%s',(node['node_id'],))[0][0]
            before = rows('SELECT generation,period_id FROM node_packages WHERE node_id=%s',(node['node_id'],))[0]
            if before[0]!=generation:
                raise AssertionError('network capture generation was already stale before the API correction')
            response = expect(f"/api/v2/nodes/{node['node_id']}/usage",'PUT',
                              {'expected_revision':previous,'usage_bytes':correction,'reset':reset},(200,))
            if response['revision']!=previous+1:
                raise AssertionError('manual correction did not advance node revision')
            node['revision']=response['revision']
            corrected = rows('SELECT usage_bytes,generation,boot_id,sampled_at,gap_reason,period_id FROM node_packages WHERE node_id=%s',(node['node_id'],))[0]
            if corrected[:5]!=(correction,generation+1,None,None,'usage_corrected'):
                raise AssertionError('manual correction did not establish a fresh counter generation')
            if (corrected[5]!=before[1])!=reset:
                raise AssertionError('manual correction/reset changed the wrong billing period')
            shell(f'touch {shlex.quote(control+"/release")}')
            outcome='failed' if invalid else 'complete'
            wait_for(lambda: rows('SELECT state FROM background_work WHERE id=%s',(work,))[0][0]==outcome,
                     'held network work did not acknowledge its late result',15)
            if shell(f'test -f {shlex.quote(control+"/returned")}',False).returncode:
                raise AssertionError('fixture did not release the actual SSH counter read')
            after = rows('SELECT usage_bytes,generation,boot_id,sampled_at,gap_reason,period_id FROM node_packages WHERE node_id=%s',(node['node_id'],))[0]
            if after!=corrected:
                raise AssertionError('old network result/failure overwrote corrected usage or baseline quality')
            if rows('SELECT count(*) FROM node_network_samples WHERE node_id=%s AND sampled_at=%s',(node['node_id'],captured_at))[0][0]:
                raise AssertionError('a pre-correction capture created a stale network history row')
            results.append({'late_result':'parse-failure' if invalid else 'valid-counters','work_id':work,
                            'executor':owner,'resource_generation':lease_generation,'capture_generation':generation,
                            'corrected_generation':corrected[1],'usage_bytes':correction,'reset':reset,
                            'final_work_state':outcome,'old_sample_rows':0,'peer_progress':True})
            print('real SSH late '+results[-1]['late_result']+' cannot overwrite API usage correction',flush=True)
        finally:
            if installed:
                shell(f'touch {shlex.quote(control+"/release")} {shlex.quote(control+"/release_all")}',False)
                shell('rm -f -- /usr/local/bin/cat',False)
        # All old scopes must finish before the next round arms a new wrapper.
        wait_for(lambda: not rows("SELECT 1 FROM resource_leases WHERE resource_key=%s AND owner IS NOT NULL AND expires_at>clock_timestamp()",('node:'+node['node_id'],)),
                 'node did not release its network scope after fixture cleanup',20)
    if any(process.poll() is not None for process in services.processes):
        raise AssertionError('one of the three full services exited')
    for target in nodes:
        subprocess.run(['docker','exec',target['container'],'systemctl','is-active','--quiet','hysteriax.service'],
                       check=True,capture_output=True,timeout=10)
    return {'passed':True,'rounds':results,
            'checks':['actual-SSH-network-read','HTTP-manual-correction','HTTP-usage-reset','new-node-counter-generation',
                      'late-valid-sample-rejected','late-failure-rejected','no-stale-history-row','peer-sampling-continues','three-service-processes-remain-live','both-real-node-services-remain-active'],
            'limitations':['only owned systemd Linux nodes are changed','cat wrapper freezes counter delivery; no service process is paused or killed in this scenario']}
