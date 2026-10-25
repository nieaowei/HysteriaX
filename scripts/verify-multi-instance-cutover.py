#!/usr/bin/env python3
"""Local three-process cutover smoke test against an isolated disposable DB.

Uses the actual server binary, PostgreSQL and Redis, plus a round-robin HTTP
proxy. Covers startup, API writes, SIGSTOP/CONT, restart identity, and retired
    locks. This is a smoke gate, not the complete billing/remote-effect fault suite.
Never prints the database URL, master key, or administrator bearer token.
"""
import argparse
import base64
import concurrent.futures
import http.server
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def inspect_container(name):
    return json.loads(subprocess.check_output(["docker", "inspect", name]))[0]


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for(predicate, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.2)
    raise AssertionError("verification condition did not become true")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/hysteriax-server")
    parser.add_argument("--pg-container", default="hx-uitest-pg")
    parser.add_argument("--redis-container", default="hx-0034-reference-check")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if not args.server.is_file():
        parser.error("build hysteriax-server before running this check")
    pg = inspect_container(args.pg_container)
    pg_env = dict(item.split("=", 1) for item in pg["Config"]["Env"] if "=" in item)
    user = pg_env.get("POSTGRES_USER", "postgres")
    base = pg_env.get("POSTGRES_DB", user)
    pg_port = pg["NetworkSettings"]["Ports"]["5432/tcp"][0]["HostPort"]
    redis = inspect_container(args.redis_container)
    redis_port = redis["NetworkSettings"]["Ports"]["6379/tcp"][0]["HostPort"]
    database = "hx0034_multi_" + uuid.uuid4().hex
    psql = ["docker", "exec", args.pg_container, "psql", "-X", "-U", user, "-v", "ON_ERROR_STOP=1", "-Atq"]

    def sql(command, db=database):
        return subprocess.check_output(psql + ["-d", db, "-c", command], text=True).strip()

    sql("CREATE DATABASE " + database, base)
    logs = Path(tempfile.mkdtemp(prefix="hx0034-multi-"))
    token = secrets.token_urlsafe(40)
    url = "postgres://{}:{}@127.0.0.1:{}/{}".format(
        urllib.parse.quote(user, safe=""), urllib.parse.quote(pg_env["POSTGRES_PASSWORD"], safe=""), pg_port, database)
    env = {**os.environ, "DATABASE_URL": url, "HYSTERIAX_MASTER_KEY": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
           "HYSTERIAX_ADMIN_TOKEN": token, "HYSTERIAX_REDIS_URL": "redis://127.0.0.1:" + redis_port + "/", "RUST_LOG": "warn"}
    processes = []
    files = []
    ports = []
    backend_lock = threading.Lock()
    backends = []
    hits = {}
    proxy = None
    result = {"scope": "three-process-cutover-smoke", "logs": str(logs), "passed": False}

    def request(base_url, path, payload=None, identity=None):
        body = None if payload is None else json.dumps(payload).encode()
        headers = {"Authorization": "Bearer " + token, "Content-Type": "application/json"}
        if payload is not None:
            headers["Idempotency-Key"] = identity or str(uuid.uuid4())
        req = urllib.request.Request(base_url + path, data=body, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=5) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            # The body could contain sensitive material; never include it in a report.
            raise AssertionError("HTTP {} on {}".format(error.code, path)) from None

    def instances():
        return set(json.loads(sql("SELECT coalesce(json_agg(instance_id),'[]') FROM service_instances")))

    def ready(port):
        try:
            return request("http://127.0.0.1:" + str(port), "/readyz")[0] == 200
        except (OSError, AssertionError):
            return False

    def start(index):
        before = instances()
        if index == len(ports):
            ports.append(free_port())
        output = (logs / ("server-{}-{}.log".format(index, len(processes)))).open("wb")
        files.append(output)
        process = subprocess.Popen([str(args.server)], env={**env, "HYSTERIAX_LISTEN_ADDR": "127.0.0.1:" + str(ports[index])}, stdout=output, stderr=subprocess.STDOUT)
        processes.append(process)

        def up():
            if process.poll() is not None:
                raise AssertionError("server exited during startup; inspect isolated logs")
            return ready(ports[index]) and len(instances() - before) == 1

        wait_for(up)
        return process, (instances() - before).pop()

    class Proxy(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass

        def dispatch(self):
            with backend_lock:
                index = hits.get("next", 0)
                port = backends[index % len(backends)]
                hits["next"] = index + 1
                hits[str(port)] = hits.get(str(port), 0) + 1
            body = self.rfile.read(int(self.headers.get("Content-Length", "0"))) or None
            forwarded = urllib.request.Request("http://127.0.0.1:{}{}".format(port, self.path), data=body, method=self.command,
                                               headers={"Authorization": self.headers.get("Authorization", ""), "Content-Type": "application/json",
                                                        "Idempotency-Key": self.headers.get("Idempotency-Key", "")})
            try:
                upstream = urllib.request.urlopen(forwarded, timeout=5)
            except urllib.error.HTTPError as error:
                upstream = error
            try:
                data = upstream.read()
                self.send_response(upstream.code)
                self.send_header("Content-Type", upstream.headers.get("Content-Type", "application/json"))
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            finally:
                upstream.close()

        do_GET = dispatch
        do_POST = dispatch

    def create_users(count, prefix):
        def create(index):
            status, data = request(front, "/api/v2/users", {"name": prefix + "-" + str(index)})
            assert status == 201 and data.get("id"), "user creation did not commit"
            return data["id"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
            return list(executor.map(create, range(count)))

    try:
        # An ordinary process must neither migrate nor initialize an empty DB.
        missing_log = (logs / "ordinary-missing-schema.log").open("wb")
        files.append(missing_log)
        rejected = subprocess.run([str(args.server)], env={**env, "HYSTERIAX_LISTEN_ADDR": "127.0.0.1:" + str(free_port())},
                                  stdout=missing_log, stderr=subprocess.STDOUT, timeout=20)
        assert rejected.returncode != 0, "ordinary startup accepted an unmigrated database"
        assert sql("SELECT count(*) FROM pg_tables WHERE schemaname=current_schema()") == "0", "ordinary startup initialized schema"
        migration_log = (logs / "migration.log").open("wb")
        files.append(migration_log)
        subprocess.run([str(args.server), "--migrate-only"], env=env, stdout=migration_log, stderr=subprocess.STDOUT, check=True, timeout=120)
        wrong_key_log = (logs / "wrong-shared-key.log").open("wb")
        files.append(wrong_key_log)
        rejected = subprocess.run([str(args.server)], env={**env, "HYSTERIAX_LISTEN_ADDR": "127.0.0.1:" + str(free_port()),
                                  "HYSTERIAX_MASTER_KEY": base64.b64encode(bytes([1]) * 32).decode().rstrip("=")},
                                  stdout=wrong_key_log, stderr=subprocess.STDOUT, timeout=20)
        assert rejected.returncode != 0, "ordinary startup accepted another shared key"
        assert not instances(), "rejected startup registered an instance"
        running = []
        ids = []
        for index in range(3):
            process, instance = start(index)
            running.append(process)
            ids.append(instance)
        assert len(set(ids)) == 3
        with backend_lock:
            backends[:] = ports
        proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
        proxy.daemon_threads = True
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
        front = "http://127.0.0.1:" + str(proxy.server_port)
        for port in ports:
            origin = "http://127.0.0.1:" + str(port)
            version = request(origin, "/api/v2/version")[1]
            assert version["api_version"] == "2.0.0" and version["management_protocol"] == 2
            assert version["directory_version"].startswith("b3:")
            assert request(origin, "/readyz")[1]["management_protocol"] == 2
        users = create_users(12, "all-three")
        # The same logical request reaches every independent process. All three
        # must recover the exact original response, without another audit or user.
        identity = str(uuid.uuid4())
        receipts = [request("http://127.0.0.1:" + str(port), "/api/v2/users", {"name":"shared-receipt"}, identity) for port in ports]
        assert receipts[0] == receipts[1] == receipts[2]
        users.append(receipts[0][1]["id"])
        assert sql("SELECT count(*) FROM management_receipts WHERE idempotency_key='{}'".format(identity)) == "1"

        for port in ports:
            assert hits.get(str(port), 0) > 0
            assert request("http://127.0.0.1:" + str(port), "/api/v2/users?page_size=200")[1]["total"] == len(users)
        print("three independent full servers accepted writes through the round-robin proxy", flush=True)

        with backend_lock:
            backends.remove(ports[1])
        running[1].send_signal(signal.SIGSTOP)
        users += create_users(6, "while-paused")
        # Database time, rather than the controller's wall clock, proves expiry.
        wait_for(lambda: sql("SELECT heartbeat_at<clock_timestamp()-interval '61 seconds' FROM service_instances WHERE instance_id='{}'".format(ids[1])) == "t", seconds=80)
        assert sql("SELECT count(*) FROM service_instances WHERE heartbeat_at>clock_timestamp()-interval '20 seconds'") == "2"
        users += create_users(3, "after-paused-expiry")
        running[1].send_signal(signal.SIGCONT)
        wait_for(lambda: sql("SELECT heartbeat_at>clock_timestamp()-interval '5 seconds' FROM service_instances WHERE instance_id='{}'".format(ids[1])) == "t")
        with backend_lock:
            backends.append(ports[1])
        print("a paused process expired without stopping writes on healthy instances, then resumed", flush=True)

        with backend_lock:
            backends.remove(ports[0])
        running[0].kill()
        running[0].wait(timeout=10)
        users += create_users(3, "while-restarting")
        replacement, replacement_id = start(0)
        running[0] = replacement
        assert replacement_id not in ids
        with backend_lock:
            backends.append(ports[0])
        users += create_users(3, "after-restart")
        # Disrupt only this script's disposable DB, never the shared container.
        # No mutation request is in flight: this checks process survival and
        # reconnection, not replay of requests with an unknown commit outcome.
        sql("ALTER DATABASE " + database + " ALLOW_CONNECTIONS false", base)
        sql("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='" + database + "'", base)
        assert not ready(ports[0]), "readiness stayed healthy during database outage"
        assert all(process.poll() is None for process in running), "a service exited on database loss"
        sql("ALTER DATABASE " + database + " ALLOW_CONNECTIONS true", base)
        for port in ports:
            wait_for(lambda port=port: ready(port))
        users += create_users(3, "after-database-recovery")
        print("all service processes survived a scoped database outage and resumed API writes", flush=True)
        for port in ports:
            assert request("http://127.0.0.1:" + str(port), "/api/v2/users?page_size=200")[1]["total"] == len(users)
        assert sql("SELECT count(*) FROM users") == str(len(users))
        assert sql("SELECT count(*) FROM audit_records WHERE action='user.created'") == str(len(users))
        retired = sql("SELECT count(*) FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND locktype='advisory' AND objsubid=1 AND classid::bigint*4294967296+objid::bigint IN (62177345902811,62177345902812)")
        assert retired == "0", "retired runtime locks are still held"
        assert sql("SELECT count(*) FROM pg_proc WHERE pronamespace=current_schema()::regnamespace AND proname='hysteriax_serialize_writes'") == "0"
        result.update(passed=True, process_instances=ids + [replacement_id], users=len(users), unique_audits=len(users),
                      retired_locks=int(retired), round_robin_hits={key: value for key, value in hits.items() if key != "next"},
                      checks=["management-protocol-two", "legacy-management-rejected-before-decoding", "same-write-receipt-across-three-processes", "ordinary-start-does-not-migrate", "migrate-only", "wrong-shared-key-rejected", "three-full-processes", "round-robin-writes-and-cache-reads", "SIGSTOP-expiry", "SIGCONT", "SIGKILL-restart-new-instance-id", "database-outage-reconnect", "no-retired-advisory-locks"])
    finally:
        if proxy:
            proxy.shutdown()
            proxy.server_close()
        for process in processes:
            if process.poll() is None:
                process.send_signal(signal.SIGCONT)
                process.terminate()
        for process in processes:
            try:
                process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        for output in files:
            output.close()
        sql("DROP DATABASE " + database + " WITH (FORCE)", base)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
