import importlib.util
import io
import json
import pathlib
import tarfile
import tempfile
import unittest
from unittest.mock import patch, Mock
from contextlib import ExitStack


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "hysteriax_backup_restore", ROOT / "scripts" / "backup-restore.py"
)
backup_restore = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup_restore)


class BackupArchiveTests(unittest.TestCase):
    def test_backup_extracts_verified_postgresql_custom_dump(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-test-") as folder:
            root = pathlib.Path(folder)
            key = b"k" * 32
            archive = root / "backups" / "test.tar.gz"
            dump = b"PGDMP" + b"encrypted PostgreSQL archive fixture"
            metadata = backup_restore.create_archive(
                root, archive, key, dump_bytes=dump, schema_version=1
            )

            restored, restored_metadata = backup_restore.extract_verified_archive(
                archive, root / "stage", key
            )

            self.assertEqual(restored.read_bytes(), dump)
            self.assertEqual(restored_metadata, metadata)
            self.assertEqual(len(metadata["master_key_sha256"]), 64)
            self.assertEqual(metadata["schema_version"], 1)
            self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
            with tarfile.open(archive, "r:gz") as stored:
                self.assertNotIn(".env", stored.getnames())
                self.assertIn("data/hysteriax.dump", stored.getnames())

    def test_restore_rejects_a_different_master_key(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-key-test-") as folder:
            root = pathlib.Path(folder)
            archive = root / "backup.tar.gz"
            backup_restore.create_archive(
                root, archive, b"a" * 32, dump_bytes=b"PGDMP fixture", schema_version=1
            )

            with self.assertRaisesRegex(RuntimeError, "does not match"):
                backup_restore.extract_verified_archive(
                    archive, root / "restore", b"b" * 32
                )

    def test_restore_rejects_archive_path_traversal(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-path-test-") as folder:
            root = pathlib.Path(folder)
            archive = root / "malicious.tar.gz"
            with tarfile.open(archive, "w:gz") as stored:
                member = tarfile.TarInfo("../outside")
                member.size = 1
                stored.addfile(member, io.BytesIO(b"x"))

            with self.assertRaisesRegex(RuntimeError, "unsafe path"):
                backup_restore.extract_verified_archive(
                    archive, root / "restore", b"k" * 32
                )

    def test_restore_rejects_modified_dump_content(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-integrity-test-") as folder:
            root = pathlib.Path(folder)
            original = root / "original.tar.gz"
            backup_restore.create_archive(
                root, original, b"k" * 32, dump_bytes=b"PGDMP original", schema_version=1
            )
            tampered = root / "tampered.tar.gz"
            with tarfile.open(original, "r:gz") as source, tarfile.open(
                tampered, "w:gz"
            ) as destination:
                for member in source.getmembers():
                    stream = source.extractfile(member) if member.isfile() else None
                    if member.name == "data/hysteriax.dump":
                        content = b"PGDMP tampered"
                        member.size = len(content)
                        stream = io.BytesIO(content)
                    if member.isfile():
                        destination.addfile(member, stream)
                    else:
                        destination.addfile(member)

            with self.assertRaisesRegex(RuntimeError, "checksums"):
                backup_restore.extract_verified_archive(
                    tampered, root / "restore", b"k" * 32
                )

    def test_legacy_sqlite_archive_is_not_accepted(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-legacy-test-") as folder:
            root = pathlib.Path(folder)
            archive = root / "legacy.tar.gz"
            metadata = {
                "format_version": 1,
                "master_key_sha256": backup_restore.hashlib.sha256(b"k" * 32).hexdigest(),
                "files": {},
            }
            with tarfile.open(archive, "w:gz") as stored:
                manifest = json.dumps(metadata).encode()
                member = tarfile.TarInfo(backup_restore.MANIFEST)
                member.size = len(manifest)
                stored.addfile(member, io.BytesIO(manifest))
                directory = tarfile.TarInfo("data")
                directory.type = tarfile.DIRTYPE
                stored.addfile(directory)

            with self.assertRaisesRegex(RuntimeError, "SQLite archives"):
                backup_restore.extract_verified_archive(
                    archive, root / "restore", b"k" * 32
                )


class RestoreTimelineTests(unittest.TestCase):
    def test_matching_journals_allow_restore(self):
        with patch.object(backup_restore, "external_columns", return_value=["id", "state"]), patch.object(
            backup_restore, "external_fingerprint", return_value=(2, "identical")
        ):
            backup_restore.check_external_timeline(pathlib.Path("fixture"), "staging")

    def test_new_journal_cannot_hide_behind_absence_in_old_schema(self):
        with patch.object(backup_restore, "external_columns", side_effect=[["operation_id"], []]), patch.object(
            backup_restore, "external_fingerprint", side_effect=[(1, "new-send"), (0, "empty")]
        ) as fingerprint:
            with self.assertRaisesRegex(RuntimeError, "execution_operations"):
                backup_restore.check_external_timeline(pathlib.Path("fixture"), "staging")
            self.assertEqual(fingerprint.call_args_list[0].args[3], ["operation_id"])
            self.assertEqual(fingerprint.call_args_list[1].args[3], [])

    def test_migration_columns_do_not_invalidate_unchanged_original_evidence(self):
        def columns(_, database, table):
            return ["id", "state", "new_epoch"] if database == "hysteriax" else ["state", "id"]
        with patch.object(backup_restore, "external_columns", side_effect=columns), patch.object(
            backup_restore, "external_fingerprint", return_value=(1, "original-fields")
        ) as fingerprint:
            backup_restore.check_external_timeline(pathlib.Path("fixture"), "staging")
            self.assertTrue(all(call.args[3] == ["id", "state"] for call in fingerprint.call_args_list))

    def test_failed_audit_does_not_rename_databases_and_resumes_current_after_maintenance(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-timeline-order-") as folder, ExitStack() as stack:
            root = pathlib.Path(folder)
            archive = root / "backup.tar.gz"
            archive.touch()
            actions = []
            def fail_audit(*_):
                actions.append("audit")
                raise RuntimeError("timeline mismatch")
            replacements = {
                "read_master_key": Mock(return_value=b"k" * 32),
                "extract_verified_archive": Mock(return_value=(root / "dump", {"schema_version": 70})),
                "api_is_ready": Mock(return_value=True),
                "current_migration_version": Mock(return_value=70),
                "_create_restore_database": Mock(), "_restore_dump": Mock(), "_drop_database": Mock(),
                "_psql_admin": Mock(), "_clear_redis_cache": Mock(),
                "_rename_database": Mock(),
                "compose": Mock(side_effect=lambda *args, **kwargs: actions.append("stop")),
                "check_external_timeline": Mock(side_effect=fail_audit),
                "prepare_restored_api": Mock(side_effect=lambda *args: actions.append("maintenance")),
                "start_api": Mock(side_effect=lambda *args: actions.append("resume")),
            }
            for name, replacement in replacements.items():
                stack.enter_context(patch.object(backup_restore, name, replacement))
            with self.assertRaisesRegex(RuntimeError, "timeline mismatch"):
                backup_restore.restore(root, archive)
            replacements["_rename_database"].assert_not_called()
            replacements["_drop_database"].assert_called_once()
            self.assertEqual(actions, ["stop", "audit", "stop", "maintenance", "resume"])


if __name__ == "__main__":
    unittest.main()
