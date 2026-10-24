"""Deterministic SIGSTOP during a real, journaled remote installation."""
import hashlib
import shlex
import subprocess
import time
import uuid


def wait_for(predicate, label, seconds=120):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.2)
    raise TimeoutError(label)


def run_installation_pause(database, services, nodes, prepare_node, expect, wait_job):
    node, peer = nodes[:2]
    container = node["container"]
    control = "/var/tmp/hx0034-fault-" + uuid.uuid4().hex

    def shell(script, check=True):
        return subprocess.run(["docker", "exec", container, "sh", "-c", script],
                              check=check, capture_output=True, text=True, timeout=10)

    # Only this owned node's first install daemon-reload is held. Preflight
    # observations and all later retries use the actual systemctl unchanged.
    shim = f'''#!/bin/sh
set -eu
control={shlex.quote(control)}
if [ "${{1:-}}" = daemon-reload ]; then
    printf 'entered\\n' >> "$control/calls"
    if [ -f "$control/arm" ]; then
        rm -- "$control/arm"
        : > "$control/entered"
        while [ ! -f "$control/release" ]; do sleep 0.05; done
    fi
fi
exec /usr/bin/systemctl "$@"
'''
    shell(f"mkdir -m 700 {shlex.quote(control)}; touch {shlex.quote(control+'/arm')}; test ! -e /usr/local/bin/systemctl")
    subprocess.run(["docker", "exec", "-i", container, "sh", "-c",
                    "cat > /usr/local/bin/systemctl; chmod 755 /usr/local/bin/systemctl"],
                   input=shim, text=True, check=True, capture_output=True)
    paused = None

    def rows(query, parameters=()):
        with database.connect() as connection:
            return connection.execute(query, parameters).fetchall()

    def calls():
        return shell(f"cat {shlex.quote(control+'/calls')}").stdout.splitlines()

    try:
        prepare_node(node)
        wait_for(lambda: shell(f"test -f {shlex.quote(control+'/entered')}", False).returncode == 0,
                 "install did not enter the controlled remote command")
        operation = rows("SELECT e.operation_id,e.owner,e.generation,e.epoch,e.work_id,e.resource_key FROM execution_operations e JOIN jobs j ON j.id=e.work_id WHERE j.node_id=%s AND j.kind='sync' AND e.state='dispatching' AND e.intent->>'kind'='ssh-command' ORDER BY e.created_at DESC LIMIT 1", (node["node_id"],))
        if len(operation) != 1:
            raise AssertionError("held installation has no exact persisted intent")
        operation_id, owner, generation, epoch, work, resource = operation[0]
        index = services.instance_ids.index(owner)
        before_count = rows("SELECT count(*) FROM execution_operations WHERE work_id=%s", (work,))[0][0]
        started = time.monotonic()
        services.pause(index)
        paused = index
        # No healthy peer job can have been claimed by the stopped owner: it is
        # admitted only after that process has been stopped and removed from LB.
        prepare_node(peer)
        peer_job = next(j for j in expect("/api/v2/jobs") if j.get("node_id") == peer["node_id"] and j["kind"] == "sync" and j.get("target_revision") == peer["revision"])
        if wait_job(peer_job["id"])["job"]["status"] != "succeeded":
            raise AssertionError("unrelated node did not complete during SIGSTOP")
        peer_before_expiry = rows("SELECT expires_at>clock_timestamp() FROM resource_leases WHERE resource_key=%s", (resource,))[0][0]
        wait_for(lambda: rows("SELECT status FROM jobs WHERE id=%s", (work,))[0][0] == "failed",
                 "expired remote work was not quarantined by a healthy process", 100)
        quarantined_after = time.monotonic() - started
        if not rows("SELECT blocked FROM resource_leases WHERE resource_key=%s", (resource,))[0][0]:
            raise AssertionError("uncertain remote work released its resource")
        if calls() != ["entered"] or rows("SELECT count(*) FROM execution_operations WHERE work_id=%s", (work,))[0][0] != before_count:
            raise AssertionError("unknown remote installation was replayed")
        body = {"expected_epoch": epoch, "expected_generation": generation}
        path = f"/api/v2/execution-operations/{operation_id}/reverify"
        busy = expect(path, "POST", body)
        if busy["state"] != "needs_attention" or not busy["resource_blocked"] or busy["retry_allowed"]:
            raise AssertionError("busy remote command was incorrectly accepted as complete")
        shell(f"touch {shlex.quote(control+'/release')}")
        receipt = "/root/.hysteriax-executions-v2/" + hashlib.sha256(resource.encode()).hexdigest() + "/ops/" + operation_id + "/result.json"
        wait_for(lambda: shell(f"test -f {shlex.quote(receipt)}", False).returncode == 0,
                 "released remote command did not complete")
        verified = expect(path, "POST", body)
        if verified["state"] != "verified" or verified["resource_blocked"] or not verified["retry_allowed"]:
            raise AssertionError("completed remote command could not be fenced and verified")
        if rows("SELECT status FROM jobs WHERE id=%s", (work,))[0][0] != "failed":
            raise AssertionError("one completion proof marked the whole deployment successful")
        event_count = rows("SELECT count(*) FROM job_events WHERE job_id=%s", (work,))[0][0]
        with database.connect() as connection:
            resumed_at = connection.execute("SELECT clock_timestamp()").fetchone()[0]
        services.resume(index)
        paused = None
        wait_for(lambda: rows("SELECT heartbeat_at>%s FROM service_instances WHERE instance_id=%s", (resumed_at, owner))[0][0],
                 "resumed old instance did not heartbeat", 30)
        if rows("SELECT count(*) FROM execution_operations WHERE work_id=%s", (work,))[0][0] != before_count:
            raise AssertionError("resumed old executor dispatched another step")
        if rows("SELECT status FROM jobs WHERE id=%s", (work,))[0][0] != "failed" or rows("SELECT count(*) FROM job_events WHERE job_id=%s", (work,))[0][0] != event_count:
            raise AssertionError("resumed old executor changed terminal progress")
        if calls() != ["entered"]:
            raise AssertionError("original installation was repeated")
        original_payload_executions = len(calls())
        retry = expect(f"/api/v2/jobs/{work}/retry", "POST", {"expected_revision": node["revision"]}, (202,))
        if wait_job(retry["job_id"])["job"]["status"] != "succeeded":
            raise AssertionError("separate linked retry did not finish after verification")
        if len(calls()) != original_payload_executions + 1:
            raise AssertionError("linked retry did not perform exactly one new installation")
        node["accepted_deployment_job"] = retry["job_id"]
        print("SIGSTOP during remote installation: unrelated node completed, expired work stayed blocked, busy proof refused, completed proof fenced, stale executor rejected, explicit retry passed", flush=True)
        return {"paused_instance": owner, "operation_id": operation_id, "original_job_id": work,
                "retry_job_id": retry["job_id"], "quarantined_after_seconds": round(quarantined_after, 3),
                "unrelated_node_finished_before_original_expiry": bool(peer_before_expiry),
                "original_operation_count": before_count, "original_payload_executions": original_payload_executions,
                "daemon_reload_calls_after_retry": len(calls()),
                "busy_resource_blocked": True, "completion_fenced": True, "old_progress_rejected": True,
                "explicit_retry_succeeded": True}
    finally:
        shell(f"touch {shlex.quote(control+'/release')}", False)
        if paused is not None:
            services.resume(paused)
        shell("rm -f -- /usr/local/bin/systemctl", False)
