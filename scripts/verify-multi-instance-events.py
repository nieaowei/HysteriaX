#!/usr/bin/env python3
"""Verify durable event publication/SSE replay with three owned full servers.

Outbox rows are test fixtures in an isolated database; production triggers remain
active. Kills only owned service PIDs; never stops shared PostgreSQL or Redis.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import psycopg
from psycopg import sql

from multi_instance_test_fixture import MultiInstanceServer, free_port


def inspect(name):
    return json.loads(subprocess.check_output(["docker", "inspect", name]))[0]


def wait_for(predicate, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError("event verification deadline exceeded")


def get(origin, token, path):
    req = urllib.request.Request(origin + path, headers={"Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=5) as response:
        return json.loads(response.read())


def read_events(origin, token, cursor, count, path="/api/v2/events"):
    req = urllib.request.Request(origin + path, headers={"Authorization": "Bearer " + token, "Last-Event-ID": cursor})
    events, fields = [], {}
    with urllib.request.urlopen(req, timeout=10) as response:
        assert response.headers["Content-Type"].startswith("text/event-stream")
        while len(events) < count:
            line = response.readline(65537)
            assert line and len(line) <= 65536, "invalid or ended SSE record"
            line = line.decode().rstrip("\r\n")
            if not line:
                if fields.get("event") and fields["event"] != "stream.cursor":
                    assert fields["event"] != "stream.reset", "unexpected SSE reset"
                    events.append((fields["id"], json.loads(fields["data"])["event_id"]))
                fields = {}
            elif not line.startswith(":"):
                key, _, value = line.partition(":")
                fields[key] = value.lstrip(" ")
    return events


def kill_backend(fixture, index):
    with fixture.lock:
        fixture.active_ports.remove(fixture.ports[index])
    fixture.processes[index].kill()
    fixture.processes[index].wait(timeout=5)


def verify(connection, url, fixture, token):
    origin = "http://127.0.0.1:" + fixture.environment["HYSTERIAX_LISTEN_ADDR"].rsplit(":", 1)[1]
    start = get(origin, token, "/api/v2/events/head")["cursor"]
    for job in ("late-source", "early-commit", "publication-fault"):
        connection.execute("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES(%s,'sync','succeeded','complete',now(),now(),now())", (job,))

    def insert(target, job, label):
        return target.execute("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES(%s,'test.durable',%s::jsonb,now()) RETURNING id", (job, json.dumps({"job_id": job, "label": label}))).fetchone()[0]

    # Distinct job rows, original triggers and ordinary publisher tasks all stay active.
    with psycopg.connect(url) as early:
        early.execute("SET LOCAL statement_timeout='3s'")
        first = insert(early, "late-source", "allocated-first-committed-later")
        second = insert(connection, "early-commit", "committed-first")
        assert first < second
        wait_for(lambda: connection.execute("SELECT publication_seq FROM event_publications WHERE event_id=%s", (second,)).fetchone())
        assert not connection.execute("SELECT 1 FROM event_publications WHERE event_id=%s", (first,)).fetchone()
        received = read_events(f"http://127.0.0.1:{fixture.ports[0]}", token, start, 1)
        assert received[0][1] == second
        early.commit()
    wait_for(lambda: connection.execute("SELECT publication_seq FROM event_publications WHERE event_id=%s", (first,)).fetchone())
    resumed = read_events(f"http://127.0.0.1:{fixture.ports[1]}", token, received[-1][0], 1)
    assert resumed[0][1] == first
    assert int(resumed[0][0].rsplit(":", 1)[1]) == int(received[0][0].rsplit(":", 1)[1]) + 1
    # Replaying the previously acknowledged position deliberately repeats one
    # stable event ID, so a consumer can deduplicate after reconnecting elsewhere.
    duplicate = read_events(f"http://127.0.0.1:{fixture.ports[2]}", token, received[-1][0], 1)
    assert duplicate == resumed
    assert len({source for _, source in received + resumed + duplicate}) == 2

    # Freeze publication only, enqueue >256 rows, then kill an owned process
    # before publication can commit. The other publishers must drain the outbox.
    with psycopg.connect(url) as gate:
        gate.execute("SELECT head FROM event_publication_stream WHERE id FOR UPDATE")
        batch = [row[0] for row in connection.execute("INSERT INTO job_events(job_id,event_type,payload_json,created_at) SELECT 'publication-fault','test.durable',jsonb_build_object('job_id','publication-fault','index',n),now() FROM generate_series(1,600) n RETURNING id")]
        assert connection.execute("SELECT count(*) FROM event_publications WHERE event_id=ANY(%s)", (batch,)).fetchone()[0] == 0
        kill_backend(fixture, 0)
        gate.commit()
    wait_for(lambda: connection.execute("SELECT count(*) FROM event_publications WHERE event_id=ANY(%s)", (batch,)).fetchone()[0] == 600)
    rows = connection.execute("SELECT publication_seq,event_id FROM event_publications ORDER BY publication_seq").fetchall()
    assert len(rows) == 602 and len({row[1] for row in rows}) == 602
    assert [row[0] for row in rows] == list(range(rows[0][0], rows[0][0] + 602))
    assert connection.execute("SELECT count(*) FROM event_publication_pending").fetchone()[0] == 0
    # A second process dies after durable publication but before the reader loads
    # those rows. The remaining process must replay every committed event.
    kill_backend(fixture, 1)
    replay = read_events(origin, token, resumed[-1][0], 600)
    assert [source for _, source in replay] == batch
    filtered = read_events(origin, token, start, 1, "/api/v2/jobs/late-source/events")
    assert filtered == resumed
    invalid = urllib.request.Request(origin + "/api/v2/events", headers={"Authorization": "Bearer " + token, "Last-Event-ID": f"v2:{uuid.uuid4()}:0"})
    try:
        urllib.request.urlopen(invalid, timeout=5)
        raise AssertionError("expired event epoch accepted")
    except urllib.error.HTTPError as error:
        assert error.code == 410
    return {"source_events": 602, "unique_publications": len(rows), "late_source_id": first,
            "earlier_commit_source_id": second, "sse_replayed_after_two_process_kills": len(replay),
            "duplicate_delivery_deduplicated_by_source_id": True,
            "checks": ["smaller-source-id-late-commit", "different-process-resume", "stable-duplicate-ID", "kill-before-publication-commit", "kill-after-publication-commit-before-delivery", "multi-page-publication-and-SSE", "job-filter", "expired-epoch-410"],
            "limitations": ["outbox rows inserted as fixtures through active production triggers", "publication gate uses an isolated DB row lock; no arbitrary instruction-level crash during COMMIT is claimed", "dedup assertion is a test consumer; macOS decoder has separate tests"], "passed": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/hysteriax-server")
    parser.add_argument("--pg-container", default="hx-uitest-pg")
    parser.add_argument("--redis-container", default="hx-0034-reference-check")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    pg, redis = inspect(args.pg_container), inspect(args.redis_container)
    settings = dict(item.split("=", 1) for item in pg["Config"]["Env"] if "=" in item)
    user = settings.get("POSTGRES_USER", "postgres")
    port = pg["NetworkSettings"]["Ports"]["5432/tcp"][0]["HostPort"]
    params = {"host": "127.0.0.1", "port": port, "user": user, "password": settings["POSTGRES_PASSWORD"], "connect_timeout": 5}
    name = "hx0034_events_" + uuid.uuid4().hex
    url = "postgres://{}:{}@127.0.0.1:{}/{}".format(urllib.parse.quote(user, safe=""), urllib.parse.quote(params["password"], safe=""), port, name)
    logs = Path(tempfile.mkdtemp(prefix="hx0034-events-"))
    token = secrets.token_urlsafe(40)
    env = {**os.environ, "DATABASE_URL": url, "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{free_port()}",
           "HYSTERIAX_MASTER_KEY": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "HYSTERIAX_ADMIN_TOKEN": token,
           "HYSTERIAX_REDIS_URL": "redis://127.0.0.1:" + redis["NetworkSettings"]["Ports"]["6379/tcp"][0]["HostPort"] + "/", "RUST_LOG": "warn"}
    fixture = None
    report = {"scope": "three-full-process-event-faults", "passed": False, "private_logs": str(logs)}
    with psycopg.connect(**params, dbname=settings.get("POSTGRES_DB", user), autocommit=True) as admin:
        admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
        try:
            with (logs / "migration.log").open("wb") as output:
                fixture = MultiInstanceServer(args.server.resolve(), env, logs, 3, output)
            with psycopg.connect(url, autocommit=True) as connection:
                connection.execute("SET statement_timeout='3s'")
                report.update(verify(connection, url, fixture, token))
                report["backends"] = fixture.report()
        finally:
            if fixture:
                fixture.terminate()
                try:
                    fixture.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait(timeout=5)
            admin.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2) + "\n")
    print("three-process event verification passed", flush=True)


if __name__ == "__main__":
    main()
