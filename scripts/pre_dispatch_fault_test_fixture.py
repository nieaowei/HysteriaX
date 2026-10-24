"""Automatic takeover before any mutating dispatch has been persisted."""
import shlex
import subprocess
import time
import uuid

from process_fault_test_fixture import wait_for


def run_pre_dispatch_pause(database, services, nodes, prepare_node, expect, wait_job):
    node, peer = nodes[:2]
    container = node["container"]
    control = "/var/tmp/hx0034-pre-dispatch-" + uuid.uuid4().hex
    paused = None
    installed = False

    def shell(script, check=True):
        return subprocess.run(["docker", "exec", container, "sh", "-c", script],
                              check=check, capture_output=True, text=True, timeout=10)

    def rows(query, parameters=()):
        with database.connect() as connection:
            return connection.execute(query, parameters).fetchall()

    def arm(_):
        nonlocal installed
        shell(f"mkdir -m 700 {shlex.quote(control)}; touch {shlex.quote(control+'/arm')}; test ! -e /usr/local/bin/systemctl")
        installed = True
        shim = f'''#!/bin/sh
set -eu
control={shlex.quote(control)}
if [ "${{1:-}}" = --version ]; then
    printf 'observed\\n' >> "$control/calls"
    if [ -f "$control/arm" ]; then
        rm -- "$control/arm"
        : > "$control/entered"
        while [ ! -f "$control/release" ]; do sleep 0.05; done
    fi
fi
exec /usr/bin/systemctl "$@"
'''
        subprocess.run(["docker", "exec", "-i", container, "sh", "-c",
                        "cat > /usr/local/bin/systemctl; chmod 755 /usr/local/bin/systemctl"],
                       input=shim, text=True, check=True, capture_output=True)

    try:
        # Arm only after the original SSH fingerprint/environment test finishes.
        prepare_node(node, before_config=arm)
        wait_for(lambda: shell(f"test -f {shlex.quote(control+'/entered')}", False).returncode == 0,
                 "deployment did not enter the held read-only preflight")
        current = rows("SELECT j.id,l.owner,l.generation,l.epoch,l.expires_at,j.attempts FROM jobs j JOIN resource_leases l ON l.resource_key='node:'||j.node_id AND l.work_id=j.id WHERE j.node_id=%s AND j.kind='sync' AND j.status='running' ORDER BY j.created_at DESC LIMIT 1", (node["node_id"],))
        if len(current) != 1:
            raise AssertionError("held preflight has no exact active deployment lease")
        work, owner, generation, epoch, expires_at, attempts = current[0]
        if rows("SELECT count(*) FROM execution_operations WHERE work_id=%s", (work,))[0][0] != 0:
            raise AssertionError("pre-dispatch fixture already sent a mutating operation")
        index = services.instance_ids.index(owner)
        started = time.monotonic()
        services.pause(index)
        paused = index
        prepare_node(peer)
        peer_job = next(j for j in expect("/api/v2/jobs") if j.get("node_id") == peer["node_id"] and j["kind"] == "sync" and j.get("target_revision") == peer["revision"])
        if wait_job(peer_job["id"])["job"]["status"] != "succeeded":
            raise AssertionError("unrelated node did not finish while preflight owner was stopped")
        if not rows("SELECT expires_at>clock_timestamp() FROM resource_leases WHERE resource_key=%s", ("node:"+node["node_id"],))[0][0]:
            raise AssertionError("unrelated node progress waited for original lease expiry")
        wait_for(lambda: rows("SELECT status FROM jobs WHERE id=%s", (work,))[0][0] == "succeeded",
                 "safe pre-dispatch work was not taken over automatically", 150)
        completed_after = time.monotonic() - started
        operations = rows("SELECT owner,generation,epoch,created_at FROM execution_operations WHERE work_id=%s ORDER BY created_at,operation_id", (work,))
        if not operations or any(row[0] == owner or row[1] <= generation or row[2] != epoch for row in operations):
            raise AssertionError("mutating steps were not fenced to a new execution generation")
        if operations[0][3] < expires_at:
            raise AssertionError("another executor dispatched before the original database lease expired")
        if rows("SELECT count(*) FROM jobs WHERE retry_of_job_id=%s", (work,))[0][0] != 0:
            raise AssertionError("automatic takeover created a separate manual retry job")
        event_count = rows("SELECT count(*) FROM job_events WHERE job_id=%s", (work,))[0][0]
        shell(f"touch {shlex.quote(control+'/release')}")
        with database.connect() as connection:
            resumed_at = connection.execute("SELECT clock_timestamp()").fetchone()[0]
        services.resume(index)
        paused = None
        wait_for(lambda: rows("SELECT heartbeat_at>%s FROM service_instances WHERE instance_id=%s", (resumed_at, owner))[0][0],
                 "original preflight owner did not resume", 30)
        if rows("SELECT status FROM jobs WHERE id=%s", (work,))[0][0] != "succeeded":
            raise AssertionError("old executor overwrote the takeover result")
        if rows("SELECT count(*) FROM execution_operations WHERE work_id=%s", (work,))[0][0] != len(operations) or rows("SELECT count(*) FROM job_events WHERE job_id=%s", (work,))[0][0] != event_count:
            raise AssertionError("resumed preflight executor appended stale progress or mutations")
        after_attempts = rows("SELECT attempts FROM jobs WHERE id=%s", (work,))[0][0]
        node["accepted_deployment_job"] = work
        print("SIGSTOP before mutating dispatch: unrelated node completed before expiry, original job automatically taken over, new-generation operations only, resumed old executor rejected", flush=True)
        return {"paused_instance": owner, "original_job_id": work, "original_mutating_operations": 0,
                "completed_original_job_without_manual_retry": True, "original_attempts": attempts,
                "completed_attempts": after_attempts, "takeover_first_dispatch_after_expiry_seconds": round((operations[0][3]-expires_at).total_seconds(), 3),
                "completed_after_pause_seconds": round(completed_after, 3), "completed_operation_count": len(operations),
                "new_execution_owners": sorted({row[0] for row in operations}),
                "old_progress_rejected": True, "unrelated_node_completed_before_expiry": True}
    finally:
        if installed:
            shell(f"touch {shlex.quote(control+'/release')}", False)
        if paused is not None:
            services.resume(paused)
        if installed:
            shell("rm -f -- /usr/local/bin/systemctl", False)
