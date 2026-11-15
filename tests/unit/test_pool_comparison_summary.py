import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('pool_summary', ROOT / 'tests/performance/summarize-pool-comparison.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PoolSummaryTests(unittest.TestCase):
    def test_postgres_fractional_timestamp_is_portable(self):
        self.assertEqual(MODULE.epoch('2026-11-08T00:00:00.4812+00:00'),
                         MODULE.epoch('2026-11-08T00:00:00.481200Z'))

    def test_stable_window_excludes_startup_and_settle(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / '01-pool-32'
            directory.mkdir()
            start = 1794096000  # 2026-11-08 00:00:00 UTC
            (directory / 'window.json').write_text(json.dumps({'stable_start_epoch': start, 'stable_end_epoch': start + 180}))
            (directory / 'api.log').write_text(
                '2026-11-07T23:59:59Z WARN pool operation phase timings operation="acquire" total_secs=8 permit_wait_secs=7\n'
                '2026-11-08T00:00:00Z WARN pool operation phase timings operation="acquire" total_secs=3 permit_wait_secs=2 outcome="ok"\n'
                '2026-11-08T00:00:01Z WARN pool operation phase timings operation="release" total_secs=1.5\n'
                '2026-11-08T00:03:00Z WARN pool operation phase timings operation="acquire" total_secs=9 permit_wait_secs=8\n')
            (directory / 'http.jsonl').write_text(json.dumps({'stable': False, 'status': 0, 'seconds': 8}) + '\n' +
                                                   json.dumps({'stable': True, 'status': 200, 'seconds': .3}) + '\n')
            samples = [{'stable': True, 'epoch': start + delta, 'cpu.stat': f'usage_usec {usage}\n',
                        'memory.current': str(1000 + delta), 'database': {'sessions': []}, 'query_seconds': .02}
                       for delta, usage in [(0, 1000000), (5, 2500000)]]
            (directory / 'resources.jsonl').write_text('\n'.join(json.dumps(s) for s in samples))
            (directory / 'quality.json').write_text(json.dumps({'work': [{'total': 18, 'complete': 17}]}))
            result = MODULE.summarize(directory)
            self.assertEqual(result['stable_acquire']['slow_acquire_count'], 1)
            self.assertEqual(result['stable_acquire']['slow_acquire_phase_seconds']['permit_wait'], 2)
            self.assertEqual(result['startup_acquire']['slow_acquire_count'], 1)
            self.assertEqual(result['stable_acquire']['release_over_one_second'], 1)
            self.assertEqual(result['readyz_status'], {'200': 1})
            self.assertAlmostEqual(result['api_cpu']['average_cores'], .3)
            self.assertFalse(result['all_work_complete'])


if __name__ == '__main__':
    unittest.main()
