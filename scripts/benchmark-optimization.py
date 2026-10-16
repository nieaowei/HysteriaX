#!/usr/bin/env python3
"""Compare local binaries on disposable schemas; never uses a production schema.

TEST_DATABASE_URL must point to a disposable local PostgreSQL database.
Example: python scripts/benchmark-optimization.py --before /tmp/hysteriax-0042-before \
    --output docs/diagnostics/0042_optimization_verification.json
The optional Rust benchmark uses unchanged accounting code for both binaries;
its separate synthetic pool workload is not HTTP pool telemetry.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shutil
import tarfile
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_encrypt
from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parent.parent
KEY = bytes(32)
TOKEN = "local-disposable-optimization-benchmark-do-not-use-in-production"
CREDENTIAL = "00000000-0000-4000-8000-000000000042"


def encrypt(value):
    nonce = bytes(range(24))  # Fixed test fixture only; never used outside this schema.
    return base64.b64encode(nonce + crypto_aead_xchacha20poly1305_ietf_encrypt(
        json.dumps(value).encode(), b"", nonce, KEY)).decode().rstrip("=")


def environment(database, port=0):
    env = os.environ.copy()
    env.update(DATABASE_URL=database.url, HYSTERIAX_LISTEN_ADDR=f"127.0.0.1:{port}",
               HYSTERIAX_PUBLIC_URL="https://benchmark.invalid", HYSTERIAX_ADMIN_TOKEN=TOKEN,
               HYSTERIAX_MASTER_KEY=base64.b64encode(KEY).decode().rstrip("="),
               RUST_LOG="warn,sqlx::pool::acquire=info", NO_COLOR="1")
    env.pop("HYSTERIAX_REDIS_URL", None)
    return env


def migrate(binary, database, success=True):
    result = subprocess.run([str(binary), "--migrate-only"], env=environment(database),
                            capture_output=True, timeout=45)
    assert (result.returncode == 0) == success, result.stderr.decode()[-2000:]
    return result


@contextmanager
def server(binary, database):
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    with tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(binary)], env=environment(database, port), stdout=log, stderr=log)
        base = f"http://127.0.0.1:{port}"
        try:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read().decode()[-2000:])
                try:
                    with urllib.request.urlopen(base + "/readyz", timeout=1):
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(.1)
            else:
                raise TimeoutError("local benchmark server readiness")
            yield base, log
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def seed(database):
    with database.connect() as c:
        c.execute("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES(%s,'Benchmark SSH','ssh_password',now(),now())", (CREDENTIAL,))
        c.execute("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES(%s,1,%s,'{}',now())", (CREDENTIAL, encrypt({"secret": "fixture-only"})))
        c.execute("""INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,
          public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,
          desired_config_enc,ssh_credential_id,ssh_credential_version,created_at,updated_at)
          SELECT 'bench-node-'||i,'Bench '||i,'127.0.0.1',9,'root','benchmark.invalid',443,':443',
          'hash-'||i,%s,%s,%s,%s,1,now(),now() FROM generate_series(0,99)i""",
                  (encrypt("node-token"), encrypt("stats"), encrypt({}), CREDENTIAL))
        c.execute("INSERT INTO users(id,name,created_at,updated_at) SELECT 'bench-user-'||i,'Bench user '||i,now(),now() FROM generate_series(0,999)i")
        c.execute("""INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at)
          SELECT 'bench-user-'||i,'bench-node-'||(i%%100),'hash-'||i,%s,now() FROM generate_series(0,999)i""", (encrypt("fixture-only"),))
        c.execute("""INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at)
          SELECT id,'bench-node-0',id,%s,now() FROM users WHERE id NOT IN
          (SELECT user_id FROM node_assignments WHERE node_id='bench-node-0')""", (encrypt("fixture-only"),))
        # Seed 42 determines reproducible source row order, values and timestamps.
        ids = list(range(100000))
        random.Random(42).shuffle(ids)
        with c.cursor().copy("COPY traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at,baseline_only) FROM STDIN") as copy:
            import datetime as dt
            start = dt.datetime(2026, 10, 1, tzinfo=dt.timezone.utc)
            for i in ids:
                copy.write_row((f"bench-traffic-{i}", f"bench-node-{i%100}", f"bench-user-{i%1000}",
                                "seed-instance", 0, 0, 1, 2, start + dt.timedelta(seconds=i), False))
        for table in ["nodes", "users", "traffic_records", "node_assignments", "credentials", "credential_versions"]:
            c.execute(f"ANALYZE {table}")


def summary(values):
    values = sorted(values)
    return {"n": len(values), "p50_ms": values[len(values)//2],
            "p95_ms": values[min(len(values)-1, int(len(values)*.95))]}


def request(base, path):
    started = time.perf_counter()
    req = urllib.request.Request(base + path, headers={"Authorization": "Bearer " + TOKEN, "Cache-Control": "no-cache"})
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            response.read()
            status = response.status
    except urllib.error.HTTPError as error:
        status = error.code
    except (OSError, urllib.error.URLError):
        status = 0
    return (time.perf_counter()-started)*1000, status


def http_workload(base, count):
    paths = ["/readyz", "/api/v1/nodes?page_size=50", "/api/v1/credentials?page_size=50",
             f"/api/v1/credentials/{CREDENTIAL}", f"/api/v1/credentials/{CREDENTIAL}/references"]
    with ThreadPoolExecutor(max_workers=12) as workers:
        results = list(workers.map(lambda i: (paths[i % len(paths)], request(base, paths[i % len(paths)])), range(count)))
    return {path: {**summary([v[0] for p,v in results if p == path]),
                   "errors": sum(v[1] != 200 for p,v in results if p == path)} for path in paths}


def plans(database):
    queries = {
        "latest_user_node": "SELECT max(sampled_at) FROM traffic_records WHERE user_id='bench-user-42' AND node_id='bench-node-42'",
        "user_fk_lookup": "SELECT 1 FROM traffic_records WHERE user_id='bench-user-42'",
        "node_fk_lookup": "SELECT 1 FROM traffic_records WHERE node_id='bench-node-42'",
    }
    with database.connect() as c:
        return {name: c.execute("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) " + query).fetchone()[0] for name, query in queries.items()}


def source_snapshot(c):
    return c.execute("""SELECT (SELECT count(*) FROM nodes),(SELECT count(*) FROM users),
        (SELECT count(*) FROM traffic_records),(SELECT coalesce(sum(usage_bytes),0) FROM users),
        (SELECT coalesce(sum(tx+rx),0) FROM user_node_usage),
        (SELECT coalesce(sum(tx+rx),0) FROM traffic_minute_totals WHERE source='users')""").fetchone()


def verify_migration(database, after):
    rollback = (ROOT / "scripts/rollback-traffic-record-indexes.sql").read_text()
    with database.connect() as c:
        original = source_snapshot(c)
        # Real cascades execute all triggers, then rollback leaves the fixture intact.
        for table, identity in [("users", "bench-user-42"), ("nodes", "bench-node-43")]:
            with c.transaction(force_rollback=True):
                expected_billing = original[3]
                if table == "users":
                    expected_billing -= c.execute("SELECT usage_bytes FROM users WHERE id=%s", (identity,)).fetchone()[0]
                c.execute(f"DELETE FROM {table} WHERE id=%s", (identity,))
                source = c.execute("SELECT coalesce(sum(delta_tx+delta_rx),0) FROM traffic_records").fetchone()[0]
                assert source == c.execute("SELECT coalesce(sum(tx+rx),0) FROM user_node_usage").fetchone()[0]
                assert source == c.execute("SELECT coalesce(sum(tx+rx),0) FROM traffic_minute_totals WHERE source='users'").fetchone()[0]
                assert c.execute("SELECT coalesce(sum(usage_bytes),0) FROM users").fetchone()[0] == expected_billing
        assert source_snapshot(c) == original
    # The artificial future migration tests the rollback guard.
    with database.connect() as c:
        c.execute("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(30,'guard fixture',true,''::bytea,0)")
    import psycopg
    with database.connect() as c:
        c.autocommit = True
        try:
            c.execute(rollback)
            raise AssertionError("rollback must refuse a newer migration")
        except psycopg.errors.RaiseException:
            c.execute("ROLLBACK")
        assert c.execute("SELECT to_regclass('traffic_records_node_time_idx')").fetchone()[0]
        c.execute("DELETE FROM _sqlx_migrations WHERE version=30")
        c.execute(rollback)
        assert source_snapshot(c) == original
    # CREATE INDEX conflicts with a writer; timeout must leave neither index nor migration record.
    with database.connect() as blocker:
        blocker.execute("LOCK TABLE traffic_records IN ROW EXCLUSIVE MODE")
        migrate(after, database, success=False)
        with database.connect() as c:
            assert c.execute("SELECT to_regclass('traffic_records_user_node_time_idx')").fetchone()[0] is None
            assert not c.execute("SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=29)").fetchone()[0]
    migrate(after, database)
    migrate(after, database)
    with database.connect() as c:
        assert source_snapshot(c) == original
        assert c.execute("SELECT count(*) FROM pg_indexes WHERE schemaname=current_schema() AND indexname IN ('traffic_records_user_node_time_idx','traffic_records_node_time_idx')").fetchone()[0] == 2
    return {"populated_upgrade": True, "repeat_startup": True, "rollback_preserves_source": True,
            "newer_migration_guard": True, "lock_timeout_atomic_retry": True, "user_node_cascade_totals_and_billing": True}


def measure(binary, database, rust_test):
    with server(binary, database) as (base, log):
        def pool_times_since(offset):
            end = os.fstat(log.fileno()).st_size
            raw = os.pread(log.fileno(), end-offset, offset).decode(errors="replace")
            return [float(value)*1000 for value in re.findall(r"aquired_after_secs=([0-9.eE+-]+)", raw)]
        offset = os.fstat(log.fileno()).st_size
        first = http_workload(base, 25)
        first_pool = pool_times_since(offset)
        offset = os.fstat(log.fileno()).st_size
        warm = http_workload(base, 150)
        warm_pool = pool_times_since(offset)
        offset = os.fstat(log.fileno()).st_size
        lock_observations = []
        stop = threading.Event()
        def observe():
            with database.connect() as c:
                c.autocommit = True
                while not stop.wait(.02):
                    lock_observations.append(c.execute("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND query LIKE '%%pg_advisory_xact_lock%%' AND wait_event='advisory'").fetchone()[0])
        watcher = threading.Thread(target=observe)
        watcher.start()
        try:
            env = os.environ.copy()
            env["HYSTERIAX_BENCH_DATABASE_URL"] = database.url
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = executor.submit(http_workload, base, 150)
                result = subprocess.run([str(rust_test), "optimization_sampling_benchmark", "--ignored", "--nocapture"],
                                        env=env, capture_output=True, text=True, timeout=120)
                concurrent_http = future.result()
            assert result.returncode == 0, result.stdout + result.stderr
            metrics = json.loads(next(line.split("=",1)[1] for line in result.stdout.splitlines() if line.startswith("OPTIMIZATION_SAMPLE_METRICS=")))
        finally:
            stop.set()
            watcher.join()
        return {"first_pass_not_os_cold": first, "warm": warm, "during_sampling": concurrent_http,
                "sample_and_synthetic_pool": metrics,
                "service_shared_pool_acquire_ms": {
                    "first_pass": first_pool, "warm": warm_pool,
                    "during_sampling_and_synthetic_workload": pool_times_since(offset)},
                "advisory_wait_observations": {"count":len(lock_observations),"max_waiters": max(lock_observations, default=0)}}


def profile_binaries(before_ref, folder):
    """Enable SQLx's existing timing log in temporary source copies only."""
    copies = []
    archive = folder / "before.tar"
    with archive.open("wb") as output:
        subprocess.run(["git", "archive", before_ref, "Cargo.toml", "Cargo.lock", "crates", "openapi"],
                       cwd=ROOT, stdout=output, check=True)
    for label in ["before", "after"]:
        source = folder / label
        source.mkdir()
        if label == "before":
            with tarfile.open(archive) as tar:
                tar.extractall(source)
            # Preserve the user's unrelated local subscription assets in both copies.
            for name in ["formats.rs", "singbox-template.json"]:
                path = Path("crates/hysteriax-server/src/api/subscriptions") / name
                if (ROOT / path).exists():
                    shutil.copy2(ROOT / path, source / path)
        else:
            for name in ["Cargo.toml", "Cargo.lock"]:
                shutil.copy2(ROOT / name, source / name)
            for name in ["crates", "openapi"]:
                shutil.copytree(ROOT / name, source / name)
        db = source / "crates/hysteriax-server/src/db.rs"
        text = db.read_text()
        assert text.count("PgPoolOptions::new()\n        .max_connections(10)") == 1
        db.write_text(text.replace("PgPoolOptions::new()\n        .max_connections(10)",
            'PgPoolOptions::new()\n        .acquire_time_level("info".parse().expect("constant log level"))\n        .max_connections(10)'))
        subprocess.run(["cargo", "build", "--locked", "--manifest-path", str(source / "Cargo.toml"),
                        "--target-dir", str(ROOT / "target"), "-p", "hysteriax-server"], check=True)
        binary = folder / (label + "-profiled")
        shutil.copy2(ROOT / "target/debug/hysteriax-server", binary)
        copies.append(binary)
    return copies


def plan_uses_index(value, index):
    if isinstance(value, dict):
        if value.get("Index Name") == index and value.get("Actual Loops", 0) > 0:
            return True
        return any(plan_uses_index(child, index) for child in value.values())
    return isinstance(value, list) and any(plan_uses_index(child, index) for child in value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    baseline = parser.add_mutually_exclusive_group(required=True)
    baseline.add_argument("--before", type=Path)
    baseline.add_argument("--profile-before-ref", help="build temporary old/new sources with SQLx pool timing logs")
    parser.add_argument("--after", type=Path, default=ROOT / "target/debug/hysteriax-server")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    profile_folder = tempfile.TemporaryDirectory(prefix="hysteriax-0042-profiling-")
    try:
        if args.profile_before_ref:
            args.before, args.after = profile_binaries(args.profile_before_ref, Path(profile_folder.name))
        run_comparison(args)
    finally:
        profile_folder.cleanup()
        # Profiling builds share the dependency cache, so restore the normal binary.
        if args.profile_before_ref:
            subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)


def run_comparison(args):
    subprocess.run(["cargo", "build", "-p", "hysteriax-server"], cwd=ROOT, check=True)
    build = subprocess.run(["cargo", "test", "--no-run", "--message-format=json"], cwd=ROOT, check=True, capture_output=True, text=True)
    rust_test = next(item["executable"] for line in build.stdout.splitlines()
                     if (item := json.loads(line)).get("reason") == "compiler-artifact" and item.get("executable") and item.get("profile",{}).get("test"))
    report = {"fixture":{"nodes":100,"users":1000,"traffic_records":100000,"seed":42},
              "pool_profiling_before_ref": subprocess.check_output(["git", "rev-parse", args.profile_before_ref], cwd=ROOT, text=True).strip() if args.profile_before_ref else None,
              "pool_timing_scope": "All acquires in the server's shared HTTP/background pool, including connection creation and health checks; not just semaphore wait.",
              "limitations":["Debug binaries; localhost HTTP and Docker PostgreSQL; not production estimates.",
                             "First pass includes cache warmup but does not flush OS/PostgreSQL caches.",
                             "Pool timings are from a separate synthetic production-configured test pool, not the HTTP pool.",
                             "Both sampling runs use identical current Rust accounting code, which this change does not modify."]}
    for label, binary in [("before", args.before.resolve()), ("after", args.after.resolve())]:
        with PostgresTestSchema() as database:
            print(f"{label}: migrating and seeding isolated fixture", flush=True)
            migrate(binary, database)
            seed(database)
            report[label] = {"binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(), "query_plans":plans(database)}
            if label == "after":
                for name, index in [("latest_user_node", "traffic_records_user_node_time_idx"),
                                    ("user_fk_lookup", "traffic_records_user_node_time_idx"),
                                    ("node_fk_lookup", "traffic_records_node_time_idx")]:
                    assert plan_uses_index(report[label]["query_plans"][name], index)
            report[label].update(measure(binary, database, rust_test))
            if args.profile_before_ref:
                assert report[label]["service_shared_pool_acquire_ms"]["warm"], "SQLx profiling logs missing"
            if label == "before":
                # Verify upgrading populated 0028 data to 0029 before dropping this schema.
                migrate(args.after.resolve(), database)
            report[label]["migration_verification"] = verify_migration(database, args.after.resolve())
        print(f"{label}: completed", flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, default=str)+"\n")
    print(f"Saved {args.output}")


if __name__ == "__main__":
    main()
