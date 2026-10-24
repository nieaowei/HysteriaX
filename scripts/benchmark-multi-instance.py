#!/usr/bin/env python3
"""Compare one/three full native servers on identical logical user workloads.

Creates and drops only uniquely named databases in an existing test PostgreSQL.
Never resets shared statistics or stops shared containers. No HTTP write retries.
This is a local HTTP microbenchmark, not a production capacity certification.
"""
import argparse
import concurrent.futures
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import secrets
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import psycopg
from psycopg import sql

from multi_instance_test_fixture import MultiInstanceServer, free_port


def container(name):
    return json.loads(subprocess.check_output(["docker", "inspect", name]))[0]


def request(origin, token, method, path, payload=None):
    headers = {"Authorization": "Bearer " + token, "Content-Type": "application/json"}
    if method != "GET":
        headers["Idempotency-Key"] = str(uuid.uuid4())
    data = None if payload is None else json.dumps(payload).encode()
    started = time.perf_counter()
    req = urllib.request.Request(origin + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=45) as response:
            status, body = response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        status = error.code
        try:
            body = {"error_code": json.loads(error.read(65536)).get("error", {}).get("code", "unknown")}
        except (ValueError, AttributeError):
            body = {"error_code": "non_json_http_error"}
    except (OSError, urllib.error.URLError):
        status, body = 0, {"error_code": "transport_error"}
    return status, body, (time.perf_counter() - started) * 1000


def require(result, status=200):
    if result[0] != status:
        raise AssertionError(f"unexpected HTTP status {result[0]}; inspect private logs")
    return result[1]


def distribution(values):
    values = sorted(values)
    def percentile(fraction):
        return values[max(0, math.ceil(len(values) * fraction) - 1)]
    return {"count": len(values), "mean_ms": sum(values) / len(values),
            "p50_ms": percentile(.5), "p95_ms": percentile(.95), "p99_ms": percentile(.99),
            "max_ms": values[-1]}


class Monitor:
    """Sample only this run's DB and owned PIDs; no SQL text or credentials stored."""
    def __init__(self, url, processes):
        self.stop_event = threading.Event()
        self.result = {"samples": 0, "errors": 0, "lock_wait_backend_samples": 0,
                       "samples_with_lock_wait": 0, "max_lock_waiters": 0,
                       "max_db_connections": 0, "max_service_rss_kib": 0, "wait_events": {}}
        self.thread = threading.Thread(target=self.run, args=(url, processes), daemon=True)
        self.thread.start()

    def run(self, url, processes):
        try:
            with psycopg.connect(url, autocommit=True, connect_timeout=5) as connection:
                connection.execute("SET statement_timeout='2s'")
                last_memory = 0
                while not self.stop_event.is_set():
                    rows = connection.execute("SELECT state,wait_event_type,wait_event FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND backend_type='client backend'").fetchall()
                    waiters = sum(kind == "Lock" for _, kind, _ in rows)
                    result = self.result
                    result["samples"] += 1
                    result["lock_wait_backend_samples"] += waiters
                    result["samples_with_lock_wait"] += int(waiters > 0)
                    result["max_lock_waiters"] = max(result["max_lock_waiters"], waiters)
                    result["max_db_connections"] = max(result["max_db_connections"], len(rows))
                    for state, kind, event in rows:
                        if state == "active" and kind:
                            key = kind + ":" + str(event)
                            result["wait_events"][key] = result["wait_events"].get(key, 0) + 1
                    if time.monotonic() - last_memory >= 1:
                        output = subprocess.check_output(["ps", "-o", "rss=", "-p", ",".join(str(p.pid) for p in processes)], text=True)
                        result["max_service_rss_kib"] = max(result["max_service_rss_kib"], sum(int(v) for v in output.split()))
                        last_memory = time.monotonic()
                    self.stop_event.wait(.05)
        except Exception:
            self.result["errors"] += 1

    def stop(self):
        self.stop_event.set()
        self.thread.join(timeout=5)
        self.result["stopped"] = not self.thread.is_alive()
        return self.result


def cache_snapshot(fixture, token):
    return [require(request(f"http://127.0.0.1:{port}", token, "GET", "/api/v2/cache"))["caches"] for port in fixture.ports]


def cache_delta(before, after):
    # Route counters distinguish computation from reuse across either memory or Redis.
    totals = {key: 0 for key in ("requests", "computed", "reused", "errors", "total_us")}
    for old, new in zip(before, after):
        for route, stats in new["routes"].items():
            if route not in ("/api/v2/users", "/api/v2/users/:id"):
                continue
            for key in totals:
                totals[key] += stats[key] - old["routes"].get(route, {}).get(key, 0)
    totals["reuse_fraction"] = totals["reused"] / max(1, totals["requests"])
    return totals


def run_case(args, admin, connection_args, redis_url, directory, instances):
    name = "hx0034_perf_" + uuid.uuid4().hex
    admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
    params = {**connection_args, "dbname": name}
    url = psycopg.conninfo.make_conninfo(**params)
    token = secrets.token_urlsafe(40)
    # The Rust driver accepts URLs rather than libpq keyword strings.
    database_url = "postgres://{}:{}@127.0.0.1:{}/{}".format(
        urllib.parse.quote(params["user"], safe=""), urllib.parse.quote(params["password"], safe=""), params["port"], name)
    environment = {**os.environ, "DATABASE_URL": database_url,
                   "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{free_port()}",
                   "HYSTERIAX_MASTER_KEY": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                   "HYSTERIAX_ADMIN_TOKEN": token, "HYSTERIAX_REDIS_URL": redis_url,
                   "RUST_LOG": "warn,hysteriax_server::db=debug", "NO_COLOR": "1"}
    fixture, monitor = None, None
    try:
        directory.mkdir(mode=0o700)
        with (directory / "migration.log").open("wb") as migration:
            fixture = MultiInstanceServer(args.server.resolve(), environment, directory, instances, migration)
        origin = "http://127.0.0.1:" + environment["HYSTERIAX_LISTEN_ADDR"].rsplit(":", 1)[1]
        ids = []
        # Seeding is excluded from measurements and deliberately sequential.
        for index in range(args.users):
            user = require(request(origin, token, "POST", "/api/v2/users", {"name": f"bench-{index:04d}", "quota_bytes": 1000000 + index}), 201)
            ids.append(user["id"])
        paths = [f"/api/v2/users/{ids[i % args.users]}" if i % 4 else f"/api/v2/users?page_size=50&page={1 + i % math.ceil(args.users / 50)}" for i in range(args.users * 3)]
        # Same warmup requests on each backend, outside the timed section.
        for port in fixture.ports:
            for path in paths[:12]:
                require(request(f"http://127.0.0.1:{port}", token, "GET", path))
        before = cache_snapshot(fixture, token)
        offsets = [(directory / f"instance-{i}.log").stat().st_size for i in range(instances)]
        hits_before = fixture.report()["round_robin_hits"]
        monitor = Monitor(url, fixture.processes)
        workload = [("PATCH", f"/api/v2/users/{user}", {"expected_revision": 1, "name": f"updated-{i:04d}", "quota_bytes": 2000000 + i}) for i, user in enumerate(ids)]
        workload += [("GET", path, None) for path in paths]
        random.Random(34).shuffle(workload)
        def perform(item):
            method, path, payload = item
            status, body, elapsed = request(origin, token, method, path, payload)
            return method, status, elapsed, path, body.get("error_code") if status != 200 else None
        started = time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as executor:
            results = list(executor.map(perform, workload))
        duration = time.perf_counter() - started
        monitoring = monitor.stop()
        monitor = None
        after = cache_snapshot(fixture, token)
        retries = 0
        for index, offset in enumerate(offsets):
            with (directory / f"instance-{index}.log").open("rb") as output:
                output.seek(offset)
                retries += output.read().count(b"retrying database transaction")
        statuses = {}
        errors = {}
        for method, status, _, _, error in results:
            key = f"{method}:{status}"
            statuses[key] = statuses.get(key, 0) + 1
            if error:
                errors[error] = errors.get(error, 0) + 1
        with psycopg.connect(url, autocommit=True) as connection:
            state = [list(row) for row in connection.execute("SELECT name,quota_bytes,revision,usage_bytes FROM users ORDER BY name")]
            audits = dict(connection.execute("SELECT action,count(*) FROM audit_records WHERE action IN ('user.created','user.updated') GROUP BY action"))
            receipts = connection.execute("SELECT count(*) FROM management_receipts").fetchone()[0]
        expected = [[f"updated-{i:04d}", 2000000 + i, 2, 0] for i in range(args.users)]
        accepted = {path.rsplit("/", 1)[1] for method, status, _, path, _ in results if method == "PATCH" and status == 200}
        accepted_reference = sorted([expected[i] if user in accepted else [f"bench-{i:04d}", 1000000+i, 1, 0] for i, user in enumerate(ids)])
        accepted_checks = {"serial_reference_for_acknowledged_writes": state == accepted_reference,
                           "receipt_count_for_acknowledged_writes": receipts == args.users + len(accepted),
                           "audit_count_for_acknowledged_writes": audits == {"user.created": args.users, "user.updated": len(accepted)}}
        checks = {"serial_reference": state == expected,
                  "receipt_count": receipts == args.users * 2,
                  "audit_count": audits == {"user.created": args.users, "user.updated": args.users}}
        # Exercise committed cache invalidation independently on every backend.
        final_cache_matches = True
        for port in fixture.ports:
            for index in (0, args.users // 2, args.users - 1):
                status, user, _ = request(f"http://127.0.0.1:{port}", token, "GET", f"/api/v2/users/{ids[index]}")
                final_cache_matches &= status == 200 and [user.get(k) for k in ("name", "quota_bytes", "revision", "usage_bytes")] == expected[index]
        checks["final_cache_matches"] = final_cache_matches
        checks["all_requests_succeeded"] = all(status == 200 for _, status, _, _, _ in results)
        checks["monitoring"] = monitoring["errors"] == 0 and monitoring["stopped"]
        cache = cache_delta(before, after)
        checks["cache_observation"] = cache["requests"] == len(paths)
        hits_after = fixture.report()["round_robin_hits"]
        return {"instances": instances, "duration_seconds": duration, "requests_per_second": len(results) / duration,
                "successful_requests_per_second": sum(status == 200 for _, status, _, _, _ in results) / duration,
                "latency": distribution([elapsed for _, _, elapsed, _, _ in results]),
                "by_method": {method: distribution([elapsed for m, _, elapsed, _, _ in results if m == method]) for method in ("GET", "PATCH")},
                "statuses": statuses, "error_codes": errors, "accepted_write_checks": accepted_checks,
                "native_transaction_retry_log_entries": retries,
                "cache_routes": cache, "monitor": monitoring,
                "load_backend_hits": {key: value - hits_before.get(key, 0) for key, value in hits_after.items()},
                "serial_reference_sha256": hashlib.sha256(json.dumps(state, separators=(",", ":")).encode()).hexdigest(),
                "receipt_count_including_seed": receipts, "audits": audits, "checks": checks,
                "passed": all(checks.values())}
    finally:
        if monitor:
            monitor.stop()
        if fixture:
            fixture.terminate()
            try:
                fixture.wait(timeout=15)
            except subprocess.TimeoutExpired:
                fixture.kill()
                fixture.wait(timeout=5)
        admin.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/hysteriax-server")
    parser.add_argument("--pg-container", default="hx-uitest-pg")
    parser.add_argument("--redis-container", default="hx-0034-reference-check")
    parser.add_argument("--users", type=int, default=256)
    parser.add_argument("--concurrency", type=int, default=8)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if not 8 <= args.users <= 1000 or not 1 <= args.concurrency <= 32:
        parser.error("users must be 8..1000 and concurrency 1..32")
    pg, redis = container(args.pg_container), container(args.redis_container)
    settings = dict(item.split("=", 1) for item in pg["Config"]["Env"] if "=" in item)
    user = settings.get("POSTGRES_USER", "postgres")
    connection_args = {"host": "127.0.0.1", "port": pg["NetworkSettings"]["Ports"]["5432/tcp"][0]["HostPort"],
                       "user": user, "password": settings["POSTGRES_PASSWORD"], "connect_timeout": 5}
    redis_url = "redis://127.0.0.1:" + redis["NetworkSettings"]["Ports"]["6379/tcp"][0]["HostPort"] + "/"
    logs = Path(tempfile.mkdtemp(prefix="hx0034-perf-"))
    report = {"scope": "local-native-http-microbenchmark", "passed": False, "private_logs": str(logs),
              "users": args.users, "concurrency": args.concurrency, "seed": 34, "cases": [],
              "runtime": {"machine": platform.machine(), "system": platform.system(), "release": platform.release(),
                          "logical_cpus": os.cpu_count(), "server_sha256": hashlib.sha256(args.server.read_bytes()).hexdigest()},
              "measurement": {"workload": "one unique revisioned PATCH per user and three GETs per user, shuffled with seed 34",
                              "latency": "HTTP call through proxy; includes server conflict retries, excludes executor queue wait",
                              "throughput": "all completed attempts divided by total executor wall time; success rate also reported",
                              "retries": "all service database retry log entries during measured load, including maintenance",
                              "cache": "user list/detail route counter deltas summed over all service processes"},
              "limitations": ["single ordered pair, no statistical confidence or production capacity claim", "same logical data; generated IDs and bucket placement differ", "debug native build, local threaded HTTP proxy, no TLS or remote node workload", "50ms sampled waits are observations, not total lock wait duration", "RSS covers owned service processes only; excludes PostgreSQL, Redis and proxy", "no failure injected: lease takeover is measured separately by process fault verification", "existing unrelated workspace changes are included in the previously built binary"]}
    try:
        with psycopg.connect(**connection_args, dbname=settings.get("POSTGRES_DB", user), autocommit=True) as admin:
            for instances in (1, 3):
                report["cases"].append(run_case(args, admin, connection_args, redis_url, logs / str(instances), instances))
                print(f"completed {instances}-instance benchmark", flush=True)
        report["matching_final_state"] = report["cases"][0]["serial_reference_sha256"] == report["cases"][1]["serial_reference_sha256"]
        report["passed"] = report["matching_final_state"] and all(case["passed"] for case in report["cases"])
    finally:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    if not report["passed"]:
        raise SystemExit("benchmark checks failed; measurements retained in report")


if __name__ == "__main__":
    main()
