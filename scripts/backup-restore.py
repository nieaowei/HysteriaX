#!/usr/bin/env python3
"""Create and restore verified PostgreSQL archives through Docker Compose."""

import argparse
import base64
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid
from datetime import datetime, timezone
from pathlib import PurePosixPath


ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = "hysteriax-backup.json"
DUMP_RELATIVE_PATH = "data/hysteriax.dump"
FORMAT_VERSION = 2
DATABASE_NAME = "hysteriax"
ADMIN_ROLE = "postgres"


def read_env(project_root):
    env_path = project_root / ".env"
    if not env_path.is_file():
        raise RuntimeError(".env is required for the database and encryption-key settings")
    values = {}
    for line in env_path.read_text().splitlines():
        if not line or line.lstrip().startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def read_master_key(project_root):
    encoded = read_env(project_root).get("HYSTERIAX_MASTER_KEY")
    if not encoded:
        raise RuntimeError("HYSTERIAX_MASTER_KEY is missing from .env")
    try:
        key = base64.b64decode(encoded + "=" * (-len(encoded) % 4), validate=True)
    except (ValueError, base64.binascii.Error) as error:
        raise RuntimeError("HYSTERIAX_MASTER_KEY is not valid base64") from error
    if len(key) != 32:
        raise RuntimeError("HYSTERIAX_MASTER_KEY must decode to exactly 32 bytes")
    return key


def app_role(project_root):
    role = read_env(project_root).get("HYSTERIAX_DB_USER", "hysteriax")
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,62}", role):
        raise RuntimeError("HYSTERIAX_DB_USER must be a simple PostgreSQL role name")
    return role


def data_file_manifest(data_dir):
    dump_path = data_dir / "hysteriax.dump"
    if not dump_path.is_file():
        raise RuntimeError("PostgreSQL dump file is missing")
    with dump_path.open("rb") as stream:
        magic = stream.read(5)
    if dump_path.stat().st_size < 5 or magic != b"PGDMP":
        raise RuntimeError("file is not a PostgreSQL custom-format dump")
    digest = hashlib.sha256()
    with dump_path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return {
        "hysteriax.dump": {
            "size": dump_path.stat().st_size,
            "sha256": digest.hexdigest(),
        }
    }


def compose_command(project_root, *arguments):
    return [
        "docker",
        "compose",
        "--project-directory",
        str(project_root),
        *arguments,
    ]


def compose(project_root, *arguments, check=True):
    return subprocess.run(
        compose_command(project_root, *arguments),
        check=check,
        capture_output=True,
        text=True,
    )


def compose_bytes(project_root, *arguments, input_bytes=None):
    return subprocess.run(
        compose_command(project_root, *arguments),
        check=True,
        capture_output=True,
        input=input_bytes,
    )


def current_migration_version(project_root, database=DATABASE_NAME):
    result = compose(
        project_root,
        "exec",
        "-T",
        "postgres",
        "psql",
        "--username",
        app_role(project_root),
        "--dbname",
        database,
        "--tuples-only",
        "--no-align",
        "--set=ON_ERROR_STOP=1",
        "--command",
        "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success",
    )
    try:
        return int(result.stdout.strip())
    except ValueError as error:
        raise RuntimeError("could not read the PostgreSQL schema version") from error


def database_dump(project_root):
    return compose_bytes(
        project_root,
        "exec",
        "-T",
        "postgres",
        "pg_dump",
        "--username",
        app_role(project_root),
        "--dbname",
        DATABASE_NAME,
        "--format=custom",
        "--no-owner",
        "--no-acl",
    ).stdout


def create_archive(project_root, archive_path, master_key, dump_bytes=None, schema_version=None):
    archive_path = pathlib.Path(archive_path).resolve()
    if archive_path.exists():
        raise RuntimeError(f"refusing to overwrite existing backup: {archive_path}")
    if dump_bytes is None:
        dump_bytes = database_dump(project_root)
    if not dump_bytes.startswith(b"PGDMP"):
        raise RuntimeError("pg_dump did not return a PostgreSQL custom-format archive")
    if schema_version is None:
        schema_version = current_migration_version(project_root)

    archive_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hysteriax-pg-backup-") as folder:
        temporary = pathlib.Path(folder)
        data_dir = temporary / "data"
        data_dir.mkdir(mode=0o700)
        (data_dir / "hysteriax.dump").write_bytes(dump_bytes)
        files = data_file_manifest(data_dir)
        metadata = {
            "format_version": FORMAT_VERSION,
            "created_at": datetime.now(timezone.utc).isoformat(),
            "schema_version": schema_version,
            "master_key_sha256": hashlib.sha256(master_key).hexdigest(),
            "files": files,
        }
        manifest_path = temporary / MANIFEST
        manifest_path.write_text(json.dumps(metadata, indent=2) + "\n")
        with tarfile.open(archive_path, "w:gz") as archive:
            archive.add(data_dir, arcname="data", recursive=True)
            archive.add(manifest_path, arcname=MANIFEST)
    archive_path.chmod(0o600)
    return metadata


def validate_archive_members(archive):
    members = archive.getmembers()
    names = set()
    for member in members:
        path = PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts:
            raise RuntimeError(f"unsafe path in backup archive: {member.name}")
        if member.name not in ("data", DUMP_RELATIVE_PATH, "data/hysteriax.db", MANIFEST):
            raise RuntimeError(f"unexpected path in backup archive: {member.name}")
        if member.name in names:
            raise RuntimeError(f"duplicate path in backup archive: {member.name}")
        names.add(member.name)
        if not (member.isdir() or member.isfile()):
            raise RuntimeError(f"unsupported file type in backup archive: {member.name}")
    if "data" not in names or MANIFEST not in names:
        raise RuntimeError("backup archive is missing data/ or its manifest")
    return members


def extract_verified_archive(archive_path, destination, master_key):
    destination = pathlib.Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    try:
        with tarfile.open(archive_path, "r:gz") as archive:
            members = validate_archive_members(archive)
            manifest_member = next(member for member in members if member.name == MANIFEST)
            manifest_stream = archive.extractfile(manifest_member)
            if manifest_stream is None:
                raise RuntimeError("backup manifest is unreadable")
            metadata = json.loads(manifest_stream.read())
            if metadata.get("format_version") != FORMAT_VERSION:
                raise RuntimeError("unsupported backup archive format; SQLite archives cannot be restored")
            if DUMP_RELATIVE_PATH not in {member.name for member in members}:
                raise RuntimeError("backup archive does not contain a PostgreSQL custom-format dump")
            expected_key_hash = hashlib.sha256(master_key).hexdigest()
            if metadata.get("master_key_sha256") != expected_key_hash:
                raise RuntimeError("backup does not match the HYSTERIAX_MASTER_KEY in .env")
            archive.extractall(destination, members=members)
    except (tarfile.TarError, json.JSONDecodeError) as error:
        raise RuntimeError(f"backup archive could not be read: {error}") from error

    data_dir = destination / "data"
    if data_file_manifest(data_dir) != metadata.get("files"):
        raise RuntimeError("backup file checksums do not match the archive manifest")
    return data_dir / "hysteriax.dump", metadata


def wait_ready(project_root, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = compose(
            project_root,
            "exec",
            "-T",
            "api",
            "curl",
            "--fail",
            "--silent",
            "http://127.0.0.1:8080/readyz",
            check=False,
        )
        if result.returncode == 0:
            return
        time.sleep(1)
    raise TimeoutError("HysteriaX API did not become ready")


def api_is_ready(project_root):
    return compose(
        project_root,
        "exec",
        "-T",
        "api",
        "curl",
        "--fail",
        "--silent",
        "http://127.0.0.1:8080/readyz",
        check=False,
    ).returncode == 0


def start_api(project_root):
    compose(project_root, "start", "api")
    wait_ready(project_root)


def default_archive(project_root):
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return project_root / "backups" / f"hysteriax-postgresql-{stamp}.tar.gz"


def backup(project_root, requested_path=None):
    project_root = pathlib.Path(project_root).resolve()
    master_key = read_master_key(project_root)
    archive_path = pathlib.Path(requested_path) if requested_path else default_archive(project_root)
    if not archive_path.is_absolute():
        archive_path = project_root / archive_path
    archive_path = archive_path.resolve()
    if not api_is_ready(project_root):
        raise RuntimeError("API must be running and ready before backup")
    compose(project_root, "stop", "--timeout", "60", "api")
    try:
        metadata = create_archive(project_root, archive_path, master_key)
    finally:
        start_api(project_root)
    print(f"Created verified PostgreSQL backup: {archive_path}")
    print(f"Schema version: {metadata['schema_version']}")
    print(f"Master-key fingerprint: {metadata['master_key_sha256']}")
    print("The master key itself is not included; keep its protected copy separately.")


def _psql_admin(project_root, sql):
    compose(
        project_root,
        "exec",
        "-T",
        "postgres",
        "psql",
        "--username",
        ADMIN_ROLE,
        "--dbname",
        "postgres",
        "--set=ON_ERROR_STOP=1",
        "--command",
        sql,
    )


def _create_restore_database(project_root, database):
    _psql_admin(
        project_root,
        f'CREATE DATABASE "{database}" OWNER "{app_role(project_root)}"',
    )


def _drop_database(project_root, database):
    _psql_admin(
        project_root,
        f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{database}' AND pid <> pg_backend_pid()",
    )
    _psql_admin(project_root, f'DROP DATABASE IF EXISTS "{database}"')


def _restore_dump(project_root, database, dump_path):
    with pathlib.Path(dump_path).open("rb") as dump:
        compose_bytes(
            project_root,
            "exec",
            "-T",
            "postgres",
            "pg_restore",
            "--exit-on-error",
            "--no-owner",
            "--no-acl",
            "--username",
            app_role(project_root),
            "--dbname",
            database,
            input_bytes=dump.read(),
        )


def _rename_database(project_root, old_name, new_name):
    _psql_admin(project_root, f'ALTER DATABASE "{old_name}" RENAME TO "{new_name}"')


def restore(project_root, archive_path):
    project_root = pathlib.Path(project_root).resolve()
    master_key = read_master_key(project_root)
    archive_path = pathlib.Path(archive_path).resolve()
    if not archive_path.is_file():
        raise RuntimeError(f"backup archive not found: {archive_path}")
    backup_dir = project_root / "backups"
    backup_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    marker = uuid.uuid4().hex[:10]
    staging_db = f"hysteriax_restore_{marker}"
    previous_db = f"hysteriax_before_restore_{marker}"
    failed_db = f"hysteriax_failed_restore_{marker}"
    previous_renamed = False
    restored_renamed = False
    created_staging = False
    api_stopped = False

    with tempfile.TemporaryDirectory(prefix=".hysteriax-restore-", dir=backup_dir) as folder:
        dump_path, metadata = extract_verified_archive(archive_path, pathlib.Path(folder), master_key)
        if not api_is_ready(project_root):
            raise RuntimeError("API must be running and ready before restore")
        try:
            _create_restore_database(project_root, staging_db)
            created_staging = True
            _restore_dump(project_root, staging_db, dump_path)
            restored_version = current_migration_version(project_root, staging_db)
            if restored_version != metadata.get("schema_version"):
                raise RuntimeError("restored database schema version does not match its manifest")

            compose(project_root, "stop", "--timeout", "60", "api")
            api_stopped = True
            _psql_admin(
                project_root,
                f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{DATABASE_NAME}' AND pid <> pg_backend_pid()",
            )
            _rename_database(project_root, DATABASE_NAME, previous_db)
            previous_renamed = True
            _rename_database(project_root, staging_db, DATABASE_NAME)
            restored_renamed = True
            start_api(project_root)
        except Exception:
            if api_stopped:
                compose(project_root, "stop", "--timeout", "60", "api", check=False)
            if restored_renamed:
                _rename_database(project_root, DATABASE_NAME, failed_db)
                created_staging = False
                print(
                    f"Restore failed; the failed database is preserved as {failed_db}.",
                    file=sys.stderr,
                )
            if previous_renamed:
                _rename_database(project_root, previous_db, DATABASE_NAME)
            if created_staging:
                _drop_database(project_root, staging_db)
                print(
                    f"Restore failed; temporary database {staging_db} was removed.",
                    file=sys.stderr,
                )
            if api_stopped:
                start_api(project_root)
            raise

    print(f"Restored verified PostgreSQL backup: {archive_path}")
    if previous_renamed:
        print(f"Previous database preserved as: {previous_db}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    backup_parser = commands.add_parser("backup", help="stop the API and create a verified PostgreSQL archive")
    backup_parser.add_argument("archive", nargs="?", help="output path (default: backups/hysteriax-postgresql-<UTC>.tar.gz)")
    restore_parser = commands.add_parser("restore", help="verify and restore a PostgreSQL archive")
    restore_parser.add_argument("archive", help="backup archive path")
    arguments = parser.parse_args()
    if arguments.command == "backup":
        backup(ROOT, arguments.archive)
    else:
        restore(ROOT, arguments.archive)


if __name__ == "__main__":
    main()
