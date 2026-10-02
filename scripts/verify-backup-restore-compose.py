#!/usr/bin/env python3
"""Exercise verified PostgreSQL backup and restore in an isolated Compose project."""

import base64
import importlib.util
import os
import pathlib
import secrets
import shutil
import subprocess
import tempfile
import uuid


ROOT = pathlib.Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "hysteriax_backup_restore", ROOT / "scripts" / "backup-restore.py"
)
backup_restore = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup_restore)


def main():
    if not shutil.which("docker"):
        raise RuntimeError("Docker is required for the Compose backup/restore integration")

    project_name = f"hysteriax-backup-restore-{uuid.uuid4().hex[:10]}"
    image = f"hysteriax-backup-restore-test:{uuid.uuid4().hex[:12]}"
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
                    "depends_on": {"postgres": {"condition": "service_healthy"}},
                    "environment": {
                        "DATABASE_URL": "postgresql://hysteriax:${HYSTERIAX_DB_PASSWORD}@postgres:5432/hysteriax",
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
            backup_restore.compose(project, "up", "-d", "--build", "api")
            backup_restore.wait_ready(project)
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
            backup_restore.backup(project, archive)

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
            backup_restore.restore(project, archive)
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
            print("Compose PostgreSQL backup and restore passed, including staging validation and readiness.")
        finally:
            backup_restore.compose(project, "down", "--volumes", "--remove-orphans", check=False)
            subprocess.run(["docker", "image", "rm", image], check=False, capture_output=True)


if __name__ == "__main__":
    main()
