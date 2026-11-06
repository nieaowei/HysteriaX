#!/usr/bin/env python3
"""Run factorial maintenance comparisons in task-owned, cloned local databases."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import uuid


VARIANTS = ["baseline", "release_only", "idle_only", "combined"]
CASE = "credentials::retention::tests::benchmarks::background_contention_comparison"
FIELD = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\[[^\]]*\]|[^\s]+)')
COUNTERS = ["calls", "attempts", "conflicts", "succeeded", "retried_success", "failed",
            "retry_exhausted", "cancelled", "duration_us"]


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def snapshots(lines):
    values = {}
    for line in lines:
        if "database transaction summary" not in line:
            continue
        fields = {}
        for key, raw in FIELD.findall(line):
            try:
                fields[key] = json.loads(raw)
            except ValueError:
                fields[key] = raw
        values[fields["operation"], fields["isolation"]] = fields
    return values


def measured(log):
    lines = log.read_text().splitlines()
    if any("panicked at" in line for line in lines):
        raise RuntimeError("A fixture task panicked; inspect " + str(log))
    a, b = lines.index("BACKGROUND_WINDOW_START"), lines.index("BACKGROUND_WINDOW_OPEN")
    c, d = lines.index("BACKGROUND_WINDOW_END"), lines.index("BACKGROUND_WINDOW_CLOSED")
    before, after = snapshots(lines[a + 1:b]), snapshots(lines[c + 1:d])
    result = []
    for key, last in after.items():
        first = before.get(key, {})
        delta = {name: last[name] - first.get(name, 0) for name in COUNTERS}
        assert all(value >= 0 for value in delta.values()), key
        completed = delta["succeeded"] + delta["failed"] + delta["cancelled"]
        result.append({"operation": key[0], "isolation": key[1], **delta,
                       "mean_completed_ms": delta["duration_us"] / completed / 1000 if completed else None,
                       "retried_success_per_call": delta["retried_success"] / delta["calls"] if delta["calls"] else None,
                       "in_flight_delta": delta["calls"] - completed})
    metadata = [json.loads(line.split("BACKGROUND_BENCHMARK ", 1)[1])
                for line in lines if "BACKGROUND_BENCHMARK " in line]
    assert len(metadata) == 1
    return {"operations": result, "fixture": metadata[0]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--sources", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=180)
    parser.add_argument("--warmup", type=int, default=60)
    parser.add_argument("--replicates", type=int, default=2)
    parser.add_argument("--paired-admission", action="store_true",
                        help="compare baseline/candidate differing only in cancelled::scan")
    args = parser.parse_args()
    if not 90 <= args.seconds <= 600 or not 0 < args.warmup < args.seconds or not 2 <= args.replicates <= 6:
        parser.error("seconds must be 90..600, 0 < warmup < seconds, and replicates 2..6")
    binaries = json.loads(args.binaries.read_text())
    sources = json.loads(args.sources.read_text())["variants"]
    names = ["baseline", "candidate"] if args.paired_admission else VARIANTS
    assert set(binaries) == set(sources) == set(names)
    assert len({digest(binaries[name]["binary"]) for name in names}) == len(names)
    assert len({sources[name]["benchmark_sha256"] for name in names}) == 1
    retention = "crates/hysteriax-server/src/credentials/retention.rs"
    if args.paired_admission:
        before, after = [sources[name]["product_hashes"] for name in names]
        assert before.keys() == after.keys()
        assert {file for file in before if before[file] != after[file]} == {
            "crates/hysteriax-server/src/credentials/rotations/cancelled.rs"}
    else:
        for source in sources["baseline"]["product_hashes"]:
            hashes = {name: sources[name]["product_hashes"][source] for name in VARIANTS}
            if source == retention:
                assert hashes["baseline"] == hashes["release_only"] != hashes["idle_only"] == hashes["combined"]
            else:
                assert hashes["baseline"] == hashes["idle_only"] != hashes["release_only"] == hashes["combined"]
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)

    def save(name, value):
        file = output / name
        file.write_text(json.dumps(value, indent=2) + "\n")
        file.chmod(0o600)

    token = uuid.uuid4().hex[:12]
    container = "hx-maintenance-causality-" + token
    password = uuid.uuid4().hex
    container_id = None
    runs = []
    try:
        container_id = subprocess.check_output([
            "docker", "run", "-d", "--name", container,
            "--label", "hysteriax.causality=" + token,
            "--memory", "2g", "--shm-size", "256m", "-p", "127.0.0.1::5432",
            "-e", "POSTGRES_PASSWORD=" + password, "postgres:18.6-alpine",
            "-c", "max_connections=128", "-c", "fsync=off",
            "-c", "synchronous_commit=off", "-c", "full_page_writes=off",
        ], text=True).strip()
        info = json.loads(subprocess.check_output(["docker", "inspect", container_id]))[0]
        assert info["Config"]["Labels"]["hysteriax.causality"] == token
        port = info["NetworkSettings"]["Ports"]["5432/tcp"][0]["HostPort"]
        save("container.json", {"id": container_id, "name": container, "token": token, "port": port})

        def sql(statement, database="postgres"):
            return subprocess.check_output([
                "docker", "exec", container_id, "psql", "-X", "-q", "-A", "-t",
                "-U", "postgres", "-d", database, "-v", "ON_ERROR_STOP=1", "-c", statement,
            ], text=True).strip()

        for _ in range(60):
            probe = subprocess.run(["docker", "exec", container_id, "pg_isready", "-U", "postgres"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if probe.returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError("Task-owned PostgreSQL did not become ready")

        def environment(database):
            env = dict(os.environ)
            for key in ["DATABASE_URL", "REDIS_URL", "TEST_REDIS_URL"]:
                env.pop(key, None)
            env.update(TEST_DATABASE_URL=f"postgresql://postgres:{password}@127.0.0.1:{port}/{database}?sslmode=disable",
                       RUST_MIN_STACK="16777216", RUST_TEST_THREADS="1",
                       HX_MAINTENANCE_BENCH_SECONDS=str(args.seconds),
                       HX_MAINTENANCE_BENCH_WARMUP=str(args.warmup))
            for key in ["HX_MAINTENANCE_BENCH_PREPARE_ONLY", "HX_MAINTENANCE_BENCH_SCHEMA"]:
                env.pop(key, None)
            return env

        def execute(name, filename, env):
            binary = binaries[name]
            assert digest(binary["binary"]) == binary["sha256"]
            with filename.open("wb") as stream:
                subprocess.run([binary["binary"], "--ignored", "--exact", CASE,
                                "--nocapture", "--test-threads=1"], env=env, stdout=stream,
                               stderr=subprocess.STDOUT, check=True, timeout=args.seconds + 120)
            assert digest(binary["binary"]) == binary["sha256"]

        seed = "hx_seed_" + token
        sql("CREATE DATABASE " + seed)
        env = environment(seed)
        env["HX_MAINTENANCE_BENCH_PREPARE_ONLY"] = "1"
        execute(names[-1], output / "prepare.log", env)
        fixtures = [json.loads(line.split("BACKGROUND_FIXTURE ", 1)[1])
                    for line in (output / "prepare.log").read_text().splitlines()
                    if "BACKGROUND_FIXTURE " in line]
        assert len(fixtures) == 1
        schema = fixtures[0]["schema"]
        assert re.fullmatch(r"hysteriax_test_[0-9a-f]{32}", schema)

        def fingerprint(database):
            return json.loads(sql("SELECT json_build_object('nodes_md5',md5((SELECT string_agg(row_to_json(n)::text,'' ORDER BY id) FROM " + schema + ".nodes n)),'work',(SELECT count(*) FROM " + schema + ".background_work),'leases',(SELECT count(*) FROM " + schema + ".resource_leases),'jobs',(SELECT count(*) FROM " + schema + ".jobs))", database))

        fixture = fingerprint(seed)
        save("fixture.json", {"schema": schema, "fingerprint": fixture,
                              "method": "CREATE DATABASE ... TEMPLATE; identical seeded rows and physical layout"})
        save("inputs.json", {"binaries": binaries, "sources": sources, "seconds": args.seconds,
                             "warmup": args.warmup, "replicates": args.replicates,
                             "paired_admission": args.paired_admission})
        for replicate in range(args.replicates):
            order = names if replicate % 2 == 0 else list(reversed(names))
            phase = 2 + (replicate % 2) * 30
            for name in order:
                database = f"hx_run_{token}_{len(runs)}"
                sql("CREATE DATABASE " + database + " TEMPLATE " + seed)
                assert fingerprint(database) == fixture
                # Equal wall-clock phase within each block controls minute-based
                # sampling slots; reversed order counterbalances warm caches.
                time.sleep((phase - time.time() % 60) % 60)
                started = datetime.datetime.now(datetime.timezone.utc).isoformat()
                env = environment(database)
                env["HX_MAINTENANCE_BENCH_SCHEMA"] = schema
                filename = output / f"r{replicate + 1}-{name}.log"
                execute(name, filename, env)
                result = {"replicate": replicate + 1, "variant": name, "started_at": started,
                          "start_phase_seconds": phase, **measured(filename)}
                assert result["fixture"]["seconds"] == args.seconds
                assert result["fixture"]["warmup_seconds"] == args.warmup
                runs.append(result)
                save("results.json", {"runs": runs, "scope": "local synthetic contention; not production or capacity acceptance"})
                print(json.dumps({"completed": name, "replicate": replicate + 1,
                                  "measured_seconds": args.seconds - args.warmup}), flush=True)
                sql("DROP DATABASE " + database + " WITH (FORCE)")
    finally:
        if container_id:
            current = json.loads(subprocess.check_output(["docker", "inspect", container_id]))[0]
            assert current["Id"] == container_id and current["Config"]["Labels"]["hysteriax.causality"] == token
            subprocess.run(["docker", "rm", "-fv", container_id], check=True)
            save("cleanup.json", {"task_container_removed": container_id, "anonymous_volume_removed": True})


if __name__ == "__main__":
    main()
