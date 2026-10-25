import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "management_test_http", Path(__file__).resolve().parents[1] / "scripts/management_test_http.py"
)
http = importlib.util.module_from_spec(spec)
spec.loader.exec_module(http)


class ManagementRequests(unittest.TestCase):
    def test_each_new_write_has_a_key_but_reads_and_public_paths_do_not(self):
        first = http.management_request("http://fixture/api/v2/users", data=b"{}")
        second = http.management_request("http://fixture/api/v2/users", data=b"{}")
        self.assertNotEqual(first.get_header("Idempotency-key"), second.get_header("Idempotency-key"))
        for method in ("POST", "PUT", "PATCH", "DELETE"):
            request = http.management_request("http://fixture/api/v2/users/one", method=method)
            self.assertTrue(request.get_header("Idempotency-key"))
        for url in ("http://fixture/api/v2/users", "http://fixture/sub/test"):
            request = http.management_request(url)
            self.assertIsNone(request.get_header("Idempotency-key"))
            self.assertEqual(request.full_url, url)

    def test_explicit_or_body_identity_is_retained_for_retries_without_mutating_headers(self):
        headers = {"Authorization": "Bearer fixture", "Idempotency-Key": "same-intent"}
        request = http.management_request("http://fixture/api/v2/users/one", method="DELETE", headers=headers)
        self.assertEqual(request.get_header("Idempotency-key"), "same-intent")
        self.assertEqual(len(headers), 2)
        for _ in range(2):
            request = http.management_request("http://fixture/api/v2/dns/records", data=b'{"idempotency_key":"dns-intent"}')
            self.assertEqual(request.get_header("Idempotency-key"), "dns-intent")
        request = http.management_request("http://fixture/api/v2/users", data=b"{}", idempotency_key="explicit")
        self.assertEqual(request.get_header("Idempotency-key"), "explicit")

    def test_failed_maintenance_cannot_start_workers(self):
        with patch.object(http.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "maintenance")), \
                patch.object(http.subprocess, "Popen") as start:
            with self.assertRaises(subprocess.CalledProcessError):
                http.start_test_server(["fixture"], env={"DATABASE_URL": "fixture"}, stdout=None)
            start.assert_not_called()

    def test_maintenance_precedes_start_with_the_same_database_environment(self):
        calls = []
        with patch.object(http.subprocess, "run", side_effect=lambda *a, **k: calls.append(("maintenance", a, k))), \
                patch.object(http.subprocess, "Popen", side_effect=lambda *a, **k: calls.append(("workers", a, k))):
            http.start_test_server(["fixture"], env={"DATABASE_URL": "fixture"}, stdout=None)
        self.assertEqual([c[0] for c in calls], ["maintenance", "workers"])
        self.assertEqual(calls[0][1][0], ["fixture", "--migrate-only"])
        self.assertEqual(calls[0][2]["env"], calls[1][2]["env"])


if __name__ == "__main__":
    unittest.main()
