#!/usr/bin/env python3
"""Compare frozen Rust test binaries against a disposable loopback PostgreSQL."""
import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--database-url-file", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--delay-ms", type=float, default=25)
    parser.add_argument("--iterations", type=int, default=10)
    parser.add_argument("--candidate-first", action="store_true")
    args = parser.parse_args()
    if not 10 <= args.iterations <= 200 or not 0 <= args.delay_ms <= 100:
        parser.error("iterations must be 10..200 and delay-ms 0..100")
    if args.baseline.resolve() == args.candidate.resolve():
        parser.error("provide distinct frozen baseline and candidate binary paths")
    backend = urlsplit(args.database_url_file.read_text().strip())
    if backend.scheme not in ("postgres", "postgresql") or backend.hostname not in (
        "localhost", "127.0.0.1", "::1"
    ):
        parser.error("a disposable loopback PostgreSQL URL is required")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    peers = set()

    async def relay(reader, writer, delay):
        while data := await reader.read(65536):
            if delay:
                await asyncio.sleep(delay)
            writer.write(data)
            await writer.drain()

    async def connection(reader, writer):
        current = asyncio.current_task()
        peers.add(current)
        remote = None
        pending = []
        try:
            upstream, remote = await asyncio.open_connection(backend.hostname, backend.port or 5432)
            pending = [asyncio.create_task(relay(reader, remote, 0)),
                       asyncio.create_task(relay(upstream, writer, args.delay_ms / 1000))]
            await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in pending:
                task.cancel()
            await asyncio.gather(*pending, return_exceptions=True)
            for stream in (writer, remote):
                if stream:
                    stream.close()
                    await stream.wait_closed()
            peers.discard(current)

    server = await asyncio.start_server(connection, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    credentials = backend.netloc.rsplit("@", 1)[0] + "@" if "@" in backend.netloc else ""
    options = dict(parse_qsl(backend.query))
    options["sslmode"] = "disable"
    environment = dict(os.environ)
    for key in ("DATABASE_URL", "REDIS_URL", "TEST_REDIS_URL"):
        environment.pop(key, None)
    environment["TEST_DATABASE_URL"] = urlunsplit((
        backend.scheme, credentials + "127.0.0.1:" + str(port), backend.path,
        urlencode(options), ""
    ))
    environment["HX_MAINTENANCE_BENCH_ITERATIONS"] = str(args.iterations)
    environment["RUST_TEST_THREADS"] = "1"
    environment.setdefault("RUST_MIN_STACK", "16777216")
    results = {}
    try:
        binaries = [("baseline", args.baseline), ("candidate", args.candidate)]
        if args.candidate_first:
            binaries.reverse()
        for name, binary in binaries:
            binary = binary.resolve()
            before_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
            log = args.output_dir / (name + ".log")
            with log.open("wb") as output:
                process = await asyncio.create_subprocess_exec(
                    str(binary), "--ignored", "--exact",
                    "credentials::retention::tests::benchmarks::maintenance_transaction_comparison",
                    "--nocapture", "--test-threads=1", env=environment,
                    stdout=output, stderr=asyncio.subprocess.STDOUT,
                )
                try:
                    code = await asyncio.wait_for(process.wait(), timeout=900)
                except BaseException:
                    process.kill()
                    await process.wait()
                    raise
            if code:
                raise RuntimeError(name + " benchmark failed; inspect " + str(log))
            scenarios = []
            for line in log.read_text().splitlines():
                if "MAINTENANCE_BENCHMARK " in line:
                    scenarios.append(json.loads(line.split("MAINTENANCE_BENCHMARK ", 1)[1]))
            if {item["scenario"] for item in scenarios} != {"empty", "nonempty", "wake_from_idle"}:
                raise RuntimeError(name + " did not report all three benchmark scenarios")
            if hashlib.sha256(binary.read_bytes()).hexdigest() != before_hash:
                raise RuntimeError(name + " binary changed; provide frozen copies")
            results[name] = {
                "binary_sha256": before_hash,
                "scenarios": {item["scenario"]: item for item in scenarios},
            }
            print(json.dumps({"completed": name, **results[name]}), flush=True)
    finally:
        server.close()
        await server.wait_closed()
        for peer in list(peers):
            peer.cancel()
        await asyncio.gather(*list(peers), return_exceptions=True)
    comparison = {}
    for scenario in ("empty", "nonempty", "wake_from_idle"):
        before = results["baseline"]["scenarios"][scenario]
        after = results["candidate"]["scenarios"][scenario]
        assert before["removed"] == after["removed"]
        comparison[scenario] = {
            "query_ratio": after["queries"] / before["queries"],
            "mean_duration_ratio": after["mean_us"] / before["mean_us"],
            "p95_duration_ratio": after["p95_us"] / before["p95_us"],
        }
    result = {"iterations": args.iterations, "response_delay_ms": args.delay_ms,
              "test_stack_bytes": int(environment["RUST_MIN_STACK"]),
              "results": results, "comparison": comparison,
              "scope": "local isolated database; no contention or capacity acceptance"}
    (args.output_dir / "comparison.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(comparison), flush=True)


if __name__ == "__main__":
    asyncio.run(main())
