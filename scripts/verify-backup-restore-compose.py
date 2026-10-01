#!/usr/bin/env python3
"""Exercise verified backup and restore against an isolated Compose API."""

import base64
import importlib.util
import json
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

    image = f"hysteriax-backup-restore-test:{uuid.uuid4().hex[:12]}"
    with tempfile.TemporaryDirectory(prefix="hysteriax-backup-restore-compose-") as folder:
        project = pathlib.Path(folder)
        for name in ("data", "backups"):
            directory = project / name
            directory.mkdir(mode=0o700)
            os.chown(directory, os.getuid(), os.getgid())

        admin_token = "hx_" + secrets.token_urlsafe(36)
        master_key = base64.b64encode(secrets.token_bytes(32)).decode().rstrip("=")
        (project / ".env").write_text(
            "\n".join(
                (
                    "HYSTERIAX_DOMAIN=backup-restore.example.test",
                    f"HYSTERIAX_ADMIN_TOKEN={admin_token}",
                    f"HYSTERIAX_MASTER_KEY={master_key}",
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
                "api": {
                    "build": {"context": str(ROOT)},
                    "image": image,
                    "restart": "unless-stopped",
                    "user": f"{os.getuid()}:{os.getgid()}",
                    "environment": {
                        "DATABASE_URL": "sqlite:///data/hysteriax.db?mode=rwc",
                        "HYSTERIAX_MIGRATION_BACKUP_DIR": "/backups/migrations",
                        "HYSTERIAX_LISTEN_ADDR": "0.0.0.0:8080",
                        "HYSTERIAX_PUBLIC_URL": "http://127.0.0.1:8080",
                        "HYSTERIAX_ADMIN_TOKEN": "${HYSTERIAX_ADMIN_TOKEN}",
                        "HYSTERIAX_MASTER_KEY": "${HYSTERIAX_MASTER_KEY}",
                        "RUST_LOG": "warn",
                    },
                    "volumes": ["./data:/data", "./backups:/backups"],
                }
            }
        }
        (project / "compose.yaml").write_text(json.dumps(compose_file, indent=2) + "\n")

        resource = project / "data" / "resource.fixture.enc"
        original = b"encrypted resource before backup"
        resource.write_bytes(original)
        try:
            backup_restore.compose(project, "up", "-d", "--build", "api")
            backup_restore.wait_ready(project)
            archive = project / "backups" / "integration.tar.gz"
            backup_restore.backup(project, archive)

            resource.write_bytes(b"changed after the backup")
            backup_restore.restore(project, archive)
            if resource.read_bytes() != original:
                raise RuntimeError("restore did not recover the resource file from the verified archive")
            if not backup_restore.api_is_ready(project):
                raise RuntimeError("API did not become ready after restoring the verified archive")
            print("Compose backup and restore passed; SQLite, encrypted resource, and API readiness recovered.")
        finally:
            backup_restore.compose(project, "down", "--volumes", "--remove-orphans", check=False)
            subprocess.run(["docker", "image", "rm", image], check=False, capture_output=True)


if __name__ == "__main__":
    main()
