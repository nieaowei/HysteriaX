import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('maintenance_benchmark', ROOT / 'tests/performance/benchmark-maintenance-scheduler.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class MaintenanceBenchmarkTests(unittest.TestCase):
    def test_window_deltas_separate_db_preflights_from_memory_coalescing(self):
        fixture = {'seconds': 120, 'warmup_seconds': 30, 'pool_max_connections': 100, 'wire_commands': 1000}
        text = '\n'.join([
            'BACKGROUND_WINDOW_START',
            'maintenance poll summary kind="idle" operation="ensure" calls=10 preflight_empty=8',
            'database transaction summary operation="write" isolation="read_committed" calls=5 duration_us=100000',
            'BACKGROUND_WINDOW_OPEN',
            # An intermediate periodic summary must not replace either boundary.
            'maintenance poll summary kind="idle" operation="ensure" calls=9999 preflight_empty=9999',
            'BACKGROUND_WINDOW_END',
            'maintenance poll summary kind="idle" operation="ensure" calls=20 preflight_empty=9 coalesced_empty=7',
            'maintenance poll summary kind="maintenance-scheduler" operation="probe" calls=3 preflight_empty=3',
            'database transaction summary operation="write" isolation="read_committed" calls=8 duration_us=450000',
            'BACKGROUND_WINDOW_CLOSED',
            'BACKGROUND_BENCHMARK ' + json.dumps(fixture),
        ])
        result = MODULE.analyze(text)
        self.assertEqual(result['window_seconds'], 90)
        self.assertEqual(result['actual_empty_preflights'], 4)
        self.assertEqual(result['coalesced_empty'], 7)
        self.assertEqual(result['maintenance_calls_needing_db'], 6)
        self.assertEqual(result['transaction_calls'], 3)
        self.assertAlmostEqual(result['transaction_wall_seconds'], .35)

    def test_incomplete_or_duplicate_windows_are_rejected(self):
        with self.assertRaises(AssertionError):
            MODULE.analyze('BACKGROUND_WINDOW_START\nBACKGROUND_WINDOW_OPEN\n')
        with self.assertRaises(AssertionError):
            MODULE.window('START\nSTART\nEND', 'START', 'END')


if __name__ == '__main__':
    unittest.main()
