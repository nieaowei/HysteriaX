#!/usr/bin/env python3
"""Exercise verified PostgreSQL backup and restore in an isolated Compose project."""

import base64
import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import secrets
import shutil
import subprocess
import tempfile
import uuid
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import urlopen, Request


ROOT = pathlib.Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "hysteriax_backup_restore", ROOT / "scripts" / "backup-restore.py"
)
backup_restore = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup_restore)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()
    if not shutil.which("docker"):
        raise RuntimeError("Docker is required for the Compose backup/restore integration")

    project_name = f"hysteriax-backup-restore-{uuid.uuid4().hex[:10]}"
    image = os.environ.get("HYSTERIAX_TEST_IMAGE") or f"hysteriax-backup-restore-test:{uuid.uuid4().hex[:12]}"
    owns_image = not os.environ.get("HYSTERIAX_TEST_IMAGE")
    report = {"scope": "isolated-compose-backup-restore", "passed": False}
    with tempfile.TemporaryDirectory(prefix="hysteriax-backup-restore-compose-") as folder:
        project = pathlib.Path(folder)
        backups = project / "backups"
        backups.mkdir(mode=0o700)
        admin_token = "hx_" + secrets.token_urlsafe(36)
        master_key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        app_password = secrets.token_hex(32)
        admin_password = secrets.token_hex(32)
        (project / ".env").write_text(
            "\n".join(
                (
                    "HYSTERIAX_DOMAIN=backup-restore.example.test",
                    f"HYSTERIAX_ADMIN_TOKEN={admin_token}",
                    f"HYSTERIAX_MASTER_KEY={master_key}",
                    f"HYSTERIAX_DB_ADMIN_PASSWORD={admin_password}",
                    "HYSTERIAX_DB_USER=hysteriax",
                    f"HYSTERIAX_DB_PASSWORD={app_password}",
                    f"HYSTERIAX_REDIS_PASSWORD={secrets.token_hex(32)}",
                    f"HYSTERIAX_UID={os.getuid()}",
                    f"HYSTERIAX_GID={os.getgid()}",
                    f"HYSTERIAX_IMAGE={image.split(':', 1)[0]}",
                    "HYSTERIAX_VERSION=dev",
                    "",
                )
            )
        )
        (project / ".env").chmod(0o600)
        compose_file = {
            "services": {
                "redis": {
                    "image": "redis:8-alpine",
                    "environment": {"HYSTERIAX_REDIS_PASSWORD": "${HYSTERIAX_REDIS_PASSWORD}"},
                    "command": ["sh", "-ec", 'exec redis-server --requirepass "$$HYSTERIAX_REDIS_PASSWORD"'],
                    "healthcheck": {
                        "test": ["CMD-SHELL", 'REDISCLI_AUTH="$$HYSTERIAX_REDIS_PASSWORD" redis-cli ping | grep -q PONG'],
                        "interval": "2s", "timeout": "3s", "retries": 10,
                    },
                },
                "postgres": {
                    "image": "postgres:18-bookworm",
                    "restart": "unless-stopped",
                    "environment": {
                        "POSTGRES_DB": "hysteriax",
                        "POSTGRES_USER": "postgres",
                        "POSTGRES_PASSWORD": "${HYSTERIAX_DB_ADMIN_PASSWORD}",
                        "HYSTERIAX_DB_USER": "${HYSTERIAX_DB_USER}",
                        "HYSTERIAX_DB_PASSWORD": "${HYSTERIAX_DB_PASSWORD}",
                    },
                    "volumes": [
                        "postgres_data:/var/lib/postgresql",
                        f"{ROOT / 'scripts' / 'provision-postgres.sh'}:/docker-entrypoint-initdb.d/10-provision-hysteriax.sh:ro",
                    ],
                    "healthcheck": {
                        "test": ["CMD-SHELL", "pg_isready -U \"$${POSTGRES_USER}\" -d \"$${POSTGRES_DB}\""],
                        "interval": "5s",
                        "timeout": "5s",
                        "retries": 12,
                    },
                },
                "api": {
                    "build": {"context": str(ROOT)},
                    "image": image,
                    "restart": "unless-stopped",
                    "depends_on": {"postgres": {"condition": "service_healthy"}, "redis": {"condition": "service_healthy"}},
                    "environment": {
                        "DATABASE_URL": "postgresql://hysteriax:${HYSTERIAX_DB_PASSWORD}@postgres:5432/hysteriax",
                        "HYSTERIAX_REDIS_URL": "redis://:${HYSTERIAX_REDIS_PASSWORD}@redis:6379/0",
                        "HYSTERIAX_LISTEN_ADDR": "0.0.0.0:8080",
                        "HYSTERIAX_PUBLIC_URL": "http://127.0.0.1:8080",
                        "HYSTERIAX_ADMIN_TOKEN": "${HYSTERIAX_ADMIN_TOKEN}",
                        "HYSTERIAX_MASTER_KEY": "${HYSTERIAX_MASTER_KEY}",
                        "RUST_LOG": "warn",
                    },
                    "healthcheck": {
                        "test": ["CMD", "curl", "--fail", "http://127.0.0.1:8080/readyz"],
                        "interval": "5s",
                        "timeout": "5s",
                        "retries": 12,
                    },
                },
            },
            "volumes": {"postgres_data": {}},
        }
        (project / "compose.yaml").write_text(__import__("json").dumps(compose_file, indent=2) + "\n")

        try:
            if owns_image:
                backup_restore.compose(project, "build", "api")
            backup_restore.compose(project, "up", "-d", "--wait", "--wait-timeout", "90", "postgres", "redis")
            backup_restore.prepare_restored_api(project)
            backup_restore.compose(project, "up", "-d", "--no-build", "--scale", "api=3", "api")
            backup_restore.wait_ready(project)
            def query(command):
                return backup_restore.compose(project, "exec", "-T", "postgres", "psql", "--username", "hysteriax",
                                              "--dbname", "hysteriax", "--tuples-only", "--no-align",
                                              "--set=ON_ERROR_STOP=1", "--command", command).stdout.strip()

            def epochs():
                return query("SELECT json_build_array((SELECT epoch FROM cache_version_epoch WHERE id),(SELECT epoch FROM event_publication_stream WHERE id),hysteriax_execution_epoch())::text")

            before_epochs = json.loads(epochs())
            def api(method, path, body=None, identity=None):
                arguments = ["exec", "-T", "api", "curl", "--fail", "--silent", "--show-error", "-X", method,
                             "-H", "Authorization: Bearer " + admin_token,
                             "-H", "Content-Type: application/json"]
                if body is not None:
                    arguments += ["-H", "Idempotency-Key: " + (identity or str(uuid.uuid4())), "--data-binary", json.dumps(body)]
                result = backup_restore.compose(project, *arguments, "http://127.0.0.1:8080" + path, check=False)
                if result.returncode:
                    raise RuntimeError("isolated management HTTP request failed; response withheld")
                return json.loads(result.stdout)

            def retained_rows():
                # Operational epochs/heartbeats may change. These business rows,
                # ciphertext, stable source IDs and links must remain identical.
                result = {}
                for table in ("users", "credentials", "credential_versions", "jobs", "job_events", "audit_records", "management_receipts"):
                    result[table] = json.loads(query(f"SELECT coalesce(json_agg(value ORDER BY value::text),'[]') FROM (SELECT to_jsonb(t) value FROM {table} t) rows"))
                return result

            user = api("POST", "/api/v2/users", {"name": "Retained backup user"})
            credential_key = str(uuid.uuid4())
            credential_body = {"name": "Retained encrypted credential", "kind": "api_token", "payload": {"token": secrets.token_urlsafe(40)}}
            credential = api("POST", "/api/v2/credentials", credential_body, credential_key)
            query("UPDATE users SET usage_bytes=1337,revision=7 WHERE id='" + user["id"] + "'")
            query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at,finished_at) VALUES('retained-history','sync','succeeded','complete',now(),now(),now(),now()); INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('retained-history','retained.event','{\"retained\":true}',now())")
            backup_restore.compose(
                project,
                "exec",
                "-T",
                "postgres",
                "psql",
                "--username",
                "hysteriax",
                "--dbname",
                "hysteriax",
                "--set=ON_ERROR_STOP=1",
                "--command",
                "CREATE TABLE backup_fixture (id integer PRIMARY KEY, content text NOT NULL); INSERT INTO backup_fixture VALUES (1, 'before backup')",
            )
            archive = backups / "integration.tar.gz"
            retained = retained_rows()
            backup_restore.backup(project, archive)
            if json.loads(epochs()) != before_epochs:
                raise RuntimeError("ordinary backup/resume unexpectedly rewrote epochs")

            backup_restore.compose(
                project,
                "exec",
                "-T",
                "postgres",
                "psql",
                "--username",
                "hysteriax",
                "--dbname",
                "hysteriax",
                "--set=ON_ERROR_STOP=1",
                "--command",
                "UPDATE backup_fixture SET content = 'after backup' WHERE id = 1",
            )
            redis_shell = 'REDISCLI_AUTH="$HYSTERIAX_REDIS_PASSWORD" redis-cli '
            backup_restore.compose(project, "exec", "-T", "redis", "sh", "-c", redis_shell + 'SET restore-marker stale')
            # A failed maintenance attempt must return to the retained current
            # DB, then rotate its incarnations before starting ordinary replicas.
            prepare = backup_restore.prepare_restored_api
            attempts = []
            def fail_once(root):
                attempts.append(True)
                if len(attempts) == 1:
                    raise RuntimeError("injected maintenance failure")
                prepare(root)
            backup_restore.prepare_restored_api = fail_once
            try:
                try:
                    backup_restore.restore(project, archive)
                except RuntimeError as error:
                    if str(error) != "injected maintenance failure":
                        raise
                else:
                    raise RuntimeError("injected restore failure was ignored")
            finally:
                backup_restore.prepare_restored_api = prepare
            if len(attempts) != 2 or not backup_restore.api_is_ready(project):
                raise RuntimeError("restore rollback did not complete maintenance before restart")
            if query("SELECT content FROM backup_fixture WHERE id=1") != "after backup" or retained_rows() != retained:
                raise RuntimeError("restore rollback lost the retained current business data")
            rollback_epochs = json.loads(epochs())
            if any(old == new for old, new in zip(before_epochs, rollback_epochs)):
                raise RuntimeError("restore rollback retained a stale incarnation")
            backup_restore.restore(project, archive)
            restored_epochs = json.loads(epochs())
            if any(old == new for old, new in zip(before_epochs, restored_epochs)):
                raise RuntimeError("restored snapshot retained a stale incarnation")
            if retained_rows() != retained:
                raise RuntimeError("restore changed retained business rows or ciphertext")
            if api("POST", "/api/v2/credentials", credential_body, credential_key) != credential:
                raise RuntimeError("restored encrypted idempotency response did not replay exactly")
            restored_user = api("GET", "/api/v2/users/" + user["id"])
            if restored_user["usage_bytes"] != 1337 or restored_user["revision"] != 7:
                raise RuntimeError("restored user usage/revision diverged")
            if api("GET", "/api/v2/jobs/retained-history")["job"]["id"] != "retained-history":
                raise RuntimeError("retained task history is no longer queryable")
            containers = backup_restore.compose(project, "ps", "--status", "running", "--quiet", "api").stdout.splitlines()
            if len(containers) != 3:
                raise RuntimeError("restore did not restart all three API replicas")
            for index in range(1, 4):
                ready = backup_restore.compose(project, "exec", "--index", str(index), "-T", "api", "curl", "--fail", "--silent", "http://127.0.0.1:8080/readyz")
                if json.loads(ready.stdout)["management_protocol"] != 2:
                    raise RuntimeError("restored replica is not protocol-ready")
            remaining = backup_restore.compose(project, "exec", "-T", "redis", "sh", "-c", redis_shell + 'EXISTS restore-marker').stdout.strip()
            if remaining != "0":
                raise RuntimeError("restore retained stale Redis cache")
            restored = backup_restore.compose(
                project,
                "exec",
                "-T",
                "postgres",
                "psql",
                "--username",
                "hysteriax",
                "--dbname",
                "hysteriax",
                "--tuples-only",
                "--no-align",
                "--command",
                "SELECT content FROM backup_fixture WHERE id = 1",
            ).stdout.strip()
            if restored != "before backup":
                raise RuntimeError("PostgreSQL restore did not recover the verified snapshot")
            if not backup_restore.api_is_ready(project):
                raise RuntimeError("API did not become ready after restoring the verified archive")
            external_calls = []
            class ExternalFixture(BaseHTTPRequestHandler):
                def log_message(self, *_):
                    pass
                def do_POST(self):
                    external_calls.append(self.path)
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b'{}')
            external = ThreadingHTTPServer(("127.0.0.1", 0), ExternalFixture)
            thread = threading.Thread(target=external.serve_forever, daemon=True)
            thread.start()
            refused_timelines = []
            try:
                for state in ("dispatching", "verified"):
                    resource = "restore-fixture:" + state
                    operation = "post-backup-" + state
                    query("INSERT INTO resource_leases(resource_key,owner,generation,work_id,expires_at,blocked) VALUES('" + resource + "','restore-fixture',1,'" + operation + "',now()-interval '1 minute',true)")
                    query("INSERT INTO execution_operations(operation_id,resource_key,owner,generation,work_id,step,state,intent) VALUES('" + operation + "','" + resource + "','restore-fixture',1,'" + operation + "','http','dispatching','{\"kind\":\"fixture-http\"}')")
                    with urlopen(Request("http://127.0.0.1:" + str(external.server_port) + "/change", method="POST"), timeout=3) as response:
                        if response.status != 200:
                            raise RuntimeError("external mutation fixture failed")
                    # dispatching deliberately loses its receipt. The other
                    # case commits confirmation; neither exists in the backup.
                    if state == "verified":
                        query("UPDATE execution_operations SET state='verified',evidence='{\"fixture_receipt\":true}' WHERE operation_id='" + operation + "'")
                        query("UPDATE resource_leases SET owner=NULL,work_id=NULL,expires_at=NULL,blocked=false WHERE resource_key='" + resource + "'")
                    query("UPDATE backup_fixture SET content='post-backup-external-" + state + "' WHERE id=1")
                    calls_before_restore = len(external_calls)
                    try:
                        backup_restore.restore(project, archive)
                    except RuntimeError as error:
                        if "external timeline differs" not in str(error):
                            raise
                    else:
                        raise RuntimeError("restore discarded post-backup external evidence")
                    if len(external_calls) != calls_before_restore:
                        raise RuntimeError("refused restore replayed an external mutation")
                    if query("SELECT content FROM backup_fixture WHERE id=1") != "post-backup-external-" + state:
                        raise RuntimeError("refused restore replaced the current database")
                    if query("SELECT state FROM execution_operations WHERE operation_id='" + operation + "'") != state:
                        raise RuntimeError("refused restore lost the external intent/receipt")
                    expected_blocked = state == "dispatching"
                    if query("SELECT blocked FROM resource_leases WHERE resource_key='" + resource + "'") != ("t" if expected_blocked else "f"):
                        raise RuntimeError("refused restore changed the resource barrier")
                    if not backup_restore.api_is_ready(project):
                        raise RuntimeError("refused restore did not resume the retained current database")
                    for index in range(1, 4):
                        backup_restore.compose(project, "exec", "--index", str(index), "-T", "api", "curl", "--fail", "--silent", "http://127.0.0.1:8080/readyz")
                    if api("GET", "/api/v2/users/" + user["id"])["usage_bytes"] != 1337:
                        raise RuntimeError("refused restore broke unrelated business reads")
                    refused_timelines.append({"state":state,"current_database_retained":True,"resource_blocked":expected_blocked,"ready_replicas":3,"external_calls_during_restore":0})
                    # Only remove our isolated, source-less fixture rows. This
                    # is test teardown, not an application unlock operation.
                    query("DELETE FROM execution_operations WHERE operation_id='" + operation + "'; DELETE FROM resource_leases WHERE resource_key='" + resource + "'")
            finally:
                external.shutdown()
                external.server_close()
                thread.join(timeout=3)
            # If maintenance also fails on the retained database, remain down.
            def always_fail(_):
                raise RuntimeError("injected persistent maintenance failure")
            backup_restore.prepare_restored_api = always_fail
            try:
                try:
                    backup_restore.restore(project, archive)
                except RuntimeError as error:
                    if str(error) != "injected persistent maintenance failure":
                        raise
                else:
                    raise RuntimeError("persistent maintenance failure was ignored")
            finally:
                backup_restore.prepare_restored_api = prepare
            if backup_restore.api_is_ready(project):
                raise RuntimeError("API restarted after both maintenance attempts failed")
            report.update(passed=True, full_service_replicas=3, before_epochs=before_epochs, rollback_epochs=rollback_epochs,
                          refused_external_timelines=refused_timelines, external_mutation_requests=len(external_calls),
                          restored_epochs=restored_epochs, retained_counts={key: len(value) for key, value in retained.items()},
                          retained_sha256=hashlib.sha256(json.dumps(retained, sort_keys=True).encode()).hexdigest(),
                          checks=["explicit-fresh-migration", "ordinary-backup-resume-no-epoch-rewrite", "restore-epoch-rotation",
                                  "business-rows-and-ciphertext-retained", "same-key-encrypted-response-replay", "usage-and-task-query",
                                  "Redis-cleared", "maintenance-failure-restores-current-DB", "rollback-maintenance-rotation",
                                  "persistent-maintenance-failure-leaves-API-stopped", "post-backup-confirmed-and-uncertain-intents-prevent-restore",
                                  "refusal-retains-current-DB-and-resource-barrier", "no-external-replay-during-refused-restore"],
                          limitations=["fixture covers listed tables, not all historical production business tables",
                                       "external mutations use an isolated real HTTP fixture; this does not claim provider-specific reconciliation or remote rollback"])
            print("Compose PostgreSQL backup and restore passed, including staging validation and readiness.")
        finally:
            backup_restore.compose(project, "down", "--volumes", "--remove-orphans", check=False)
            if owns_image:
                subprocess.run(["docker", "image", "rm", image], check=False, capture_output=True)
            if args.report:
                args.report.parent.mkdir(parents=True, exist_ok=True)
                args.report.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
