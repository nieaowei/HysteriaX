import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]


class MigrationScriptBinaryPathTests(unittest.TestCase):
    def test_relative_dot_slash_binary_resolves_to_absolute_path_for_subprocess(self):
        for script_rel in (
            "scripts/migrations/migrate-drop-users-exhausted-index-test-db.py",
            "scripts/migrations/migrate-scale-indexes-test-db.py",
        ):
            text = (REPO_ROOT / script_rel).read_text(encoding="utf-8")
            self.assertIn("Path(args.binary).resolve()", text)

        with tempfile.TemporaryDirectory() as tmp:
            fake_bin = Path(tmp) / "fake-server"
            fake_bin.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            os.chmod(fake_bin, 0o755)
            # Ensure a ./relative path stays runnable via resolve() rather than
            # collapsing to a bare filename that relies on PATH lookup.
            resolved = Path("./fake-server")
            self.assertEqual(str(resolved), "fake-server")
            old_cwd = os.getcwd()
            try:
                os.chdir(tmp)
                abs_bin = resolved.resolve()
                self.assertTrue(abs_bin.is_file())
                self.assertEqual(abs_bin, fake_bin.resolve())
                proc = subprocess.run([str(abs_bin)], check=False)
                self.assertEqual(proc.returncode, 0)
            finally:
                os.chdir(old_cwd)


if __name__ == "__main__":
    unittest.main()
