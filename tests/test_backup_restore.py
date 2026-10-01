import importlib.util
import pathlib
import sqlite3
import tarfile
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "hysteriax_backup_restore", ROOT / "scripts" / "backup-restore.py"
)
backup_restore = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup_restore)


class BackupArchiveTests(unittest.TestCase):
    def make_project(self, root):
        data = root / "data"
        data.mkdir()
        with sqlite3.connect(data / "hysteriax.db") as database:
            database.execute("PRAGMA journal_mode=WAL")
            database.execute("CREATE TABLE resources (id TEXT PRIMARY KEY, body BLOB)")
            database.execute(
                "INSERT INTO resources VALUES ('resource-1', ?)", (b"encrypted fixture",)
            )
        (data / "resource.pem.enc").write_bytes(b"encrypted resource fixture")
        return data

    def test_backup_extracts_database_and_encrypted_resources(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-test-") as folder:
            root = pathlib.Path(folder)
            data = self.make_project(root)
            key = b"k" * 32
            archive = root / "backups" / "test.tar.gz"
            metadata = backup_restore.create_archive(root, archive, key)

            stage = root / "stage"
            restored = backup_restore.extract_verified_archive(archive, stage, key)

            with sqlite3.connect(restored / "hysteriax.db") as database:
                value = database.execute(
                    "SELECT body FROM resources WHERE id = 'resource-1'"
                ).fetchone()[0]
            self.assertEqual(value, b"encrypted fixture")
            self.assertEqual(
                (restored / "resource.pem.enc").read_bytes(),
                (data / "resource.pem.enc").read_bytes(),
            )
            self.assertEqual(len(metadata["master_key_sha256"]), 64)
            with tarfile.open(archive, "r:gz") as stored:
                self.assertNotIn(".env", stored.getnames())

    def test_restore_rejects_a_different_master_key(self):
        with tempfile.TemporaryDirectory(prefix="hysteriax-backup-key-test-") as folder:
            root = pathlib.Path(folder)
            self.make_project(root)
            archive = root / "backup.tar.gz"
            backup_restore.create_archive(root, archive, b"a" * 32)

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
                import io

                stored.addfile(member, io.BytesIO(b"x"))

            with self.assertRaisesRegex(RuntimeError, "unsafe path"):
                backup_restore.extract_verified_archive(
                    archive, root / "restore", b"k" * 32
                )


if __name__ == "__main__":
    unittest.main()
