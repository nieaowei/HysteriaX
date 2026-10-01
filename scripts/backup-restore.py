#!/usr/bin/env python3
"""Create and restore verified HysteriaX data archives through Compose."""

import argparse
import base64
import hashlib
import json
import os
import pathlib
import shutil
import sqlite3
import subprocess
import tarfile
import tempfile
import time
from datetime import datetime, timezone
from pathlib import PurePosixPath


ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = "hysteriax-backup.json"


def read_master_key(project_root):
    env_path = project_root / ".env"
    if not env_path.is_file():
        raise RuntimeError(".env is required so the archive can be tied to its encryption key")
    encoded = None
    for line in env_path.read_text().splitlines():
        if line.startswith("HYSTERIAX_MASTER_KEY="):
            encoded = line.split("=", 1)[1].strip().strip('"').strip("'")
            break
    if not encoded:
        raise RuntimeError("HYSTERIAX_MASTER_KEY is missing from .env")
    try:
        key = base64.b64decode(encoded + "=" * (-len(encoded) % 4), validate=True)
    except (ValueError, base64.binascii.Error) as error:
        raise RuntimeError("HYSTERIAX_MASTER_KEY is not valid base64") from error
    if len(key) != 32:
        raise RuntimeError("HYSTERIAX_MASTER_KEY must decode to exactly 32 bytes")
    return key


def data_file_manifest(data_dir):
    database = data_dir / "hysteriax.db"
    if not database.is_file():
        raise RuntimeError(f"SQLite database not found: {database}")
    files = {}
    for path in sorted(data_dir.rglob("*")):
        if path.is_symlink():
            raise RuntimeError(f"refusing to archive a symbolic link: {path}")
        if not path.is_file():
            continue
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        files[path.relative_to(data_dir).as_posix()] = {
            "size": path.stat().st_size,
            "sha256": digest.hexdigest(),
        }
    return files


def verify_sqlite(database_path):
    if not database_path.is_file():
        raise RuntimeError("backup archive does not contain data/hysteriax.db")
    uri = database_path.resolve().as_uri() + "?mode=ro"
    try:
        with sqlite3.connect(uri, uri=True, timeout=5) as database:
            result = database.execute("PRAGMA quick_check").fetchone()
    except sqlite3.Error as error:
        raise RuntimeError(f"SQLite backup check failed: {error}") from error
    if not result or result[0] != "ok":
        raise RuntimeError(f"SQLite backup is not consistent: {result}")


def create_archive(project_root, archive_path, master_key):
    data_dir = project_root / "data"
    if not data_dir.is_dir():
        raise RuntimeError("data/ directory does not exist")
    archive_path = archive_path.resolve()
    if archive_path == data_dir or data_dir in archive_path.parents:
        raise RuntimeError("backup archive must be outside data/")
    if archive_path.exists():
        raise RuntimeError(f"refusing to overwrite existing backup: {archive_path}")
    verify_sqlite(data_dir / "hysteriax.db")
    files = data_file_manifest(data_dir)
    metadata = {
        "format_version": 1,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "master_key_sha256": hashlib.sha256(master_key).hexdigest(),
        "files": files,
    }
    archive_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hysteriax-backup-manifest-") as folder:
        manifest_path = pathlib.Path(folder) / MANIFEST
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
        if path.parts[0] not in ("data", MANIFEST):
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
    destination.mkdir(parents=True, exist_ok=True)
    try:
        with tarfile.open(archive_path, "r:gz") as archive:
            members = validate_archive_members(archive)
            manifest_member = next(member for member in members if member.name == MANIFEST)
            manifest_stream = archive.extractfile(manifest_member)
            if manifest_stream is None:
                raise RuntimeError("backup manifest is unreadable")
            metadata = json.loads(manifest_stream.read())
            if metadata.get("format_version") != 1:
                raise RuntimeError("unsupported backup archive format")
            expected_key_hash = hashlib.sha256(master_key).hexdigest()
            if metadata.get("master_key_sha256") != expected_key_hash:
                raise RuntimeError("backup does not match the HYSTERIAX_MASTER_KEY in .env")
            archive.extractall(destination, members=members)
    except (tarfile.TarError, json.JSONDecodeError) as error:
        raise RuntimeError(f"backup archive could not be read: {error}") from error

    data_dir = destination / "data"
    actual_files = data_file_manifest(data_dir)
    if actual_files != metadata.get("files"):
        raise RuntimeError("backup file checksums do not match the archive manifest")
    verify_sqlite(data_dir / "hysteriax.db")
    return data_dir


def compose(project_root, *arguments, check=True):
    return subprocess.run(
        ["docker", "compose", "--project-directory", str(project_root), *arguments],
        check=check,
        capture_output=True,
        text=True,
    )


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
    return project_root / "backups" / f"hysteriax-data-{stamp}.tar.gz"


def backup(project_root, requested_path=None):
    project_root = project_root.resolve()
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
    print(f"Created verified backup: {archive_path}")
    print(f"Master-key fingerprint: {metadata['master_key_sha256']}")
    print("The master key itself is not included; keep its protected copy separately.")


def unique_restore_path(project_root, label):
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    candidate = project_root / f"data.{label}-{stamp}"
    if candidate.exists():
        candidate = project_root / f"data.{label}-{stamp}-{os.getpid()}"
    return candidate


def restore(project_root, archive_path):
    project_root = project_root.resolve()
    master_key = read_master_key(project_root)
    archive_path = pathlib.Path(archive_path).resolve()
    if not archive_path.is_file():
        raise RuntimeError(f"backup archive not found: {archive_path}")

    with tempfile.TemporaryDirectory(prefix=".hysteriax-restore-", dir=project_root) as folder:
        staged_data = extract_verified_archive(archive_path, pathlib.Path(folder), master_key)
        if not api_is_ready(project_root):
            raise RuntimeError("API must be running and ready before restore")
        compose(project_root, "stop", "--timeout", "60", "api")
        current_data = project_root / "data"
        preserved_data = unique_restore_path(project_root, "before-restore")
        failed_data = unique_restore_path(project_root, "failed-restore")
        if current_data.exists():
            os.replace(current_data, preserved_data)
        try:
            os.replace(staged_data, current_data)
            start_api(project_root)
        except Exception:
            compose(project_root, "stop", "--timeout", "60", "api", check=False)
            if current_data.exists():
                os.replace(current_data, failed_data)
            if preserved_data.exists():
                os.replace(preserved_data, current_data)
            start_api(project_root)
            raise
    print(f"Restored verified backup: {archive_path}")
    if preserved_data.exists():
        print(f"Previous data preserved at: {preserved_data}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    backup_parser = commands.add_parser("backup", help="stop the API and create a verified archive")
    backup_parser.add_argument("archive", nargs="?", help="output path (default: backups/hysteriax-data-<UTC>.tar.gz)")
    restore_parser = commands.add_parser("restore", help="restore a verified archive after checking its master-key fingerprint")
    restore_parser.add_argument("archive", help="backup archive path")
    arguments = parser.parse_args()
    if arguments.command == "backup":
        backup(ROOT, arguments.archive)
    else:
        restore(ROOT, arguments.archive)


if __name__ == "__main__":
    main()
