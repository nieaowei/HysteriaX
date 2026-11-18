"""Configuration preservation and recovery invariants for the production crossover."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('pool_comparison', ROOT / 'scripts/ops/compare-production-pool.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PoolComparisonTests(unittest.TestCase):
    def test_owner_waits_for_drained_instance_heartbeat_to_expire(self):
        comparison = object.__new__(MODULE.Comparison)
        current = [{'instance_id': 'current'}]
        comparison.rows = unittest.mock.Mock(side_effect=[current + [{'instance_id': 'drained'}], current])
        with patch.object(MODULE.time, 'sleep') as sleep:
            self.assertEqual(comparison.current_owner(), current)
        sleep.assert_called_once_with(2)

    def test_owner_does_not_pick_arbitrary_instance(self):
        comparison = object.__new__(MODULE.Comparison)
        comparison.rows = unittest.mock.Mock(return_value=[{'instance_id': 'one'}, {'instance_id': 'two'}])
        with patch.object(MODULE.time, 'monotonic', side_effect=[0, 76]):
            with self.assertRaises(AssertionError):
                comparison.current_owner()

    def test_candidate_changes_only_pool_and_image_then_restores_exact_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root, output = Path(temp) / 'root', Path(temp) / 'evidence'
            root.mkdir()
            output.mkdir()
            original_env = (b'DATABASE_URL=postgres://fixture:fixture@localhost/fixture\n'
                            b"HYSTERIAX_BACKGROUND_WORKER_PARALLELISM='16'\n"
                            b"HYSTERIAX_SAMPLING_INTERVAL_SECONDS='60'\n"
                            b'HYSTERIAX_DB_POOL_MAX_CONNECTIONS=32\nNEXT_SETTING=preserve\n')
            for name in MODULE.CONFIGS:
                file = root / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes(original_env if name == 'api.env' else
                                 MODULE.ORIGINAL.encode() if name == 'compose.yaml' else b'preserve\n')
            comparison = MODULE.Comparison(SimpleNamespace(root=root, output=output))
            comparison.drain = lambda directory: None
            comparison.compose = lambda *args: None

            def inspect():
                config = MODULE.parse_env((root / 'api.env').read_text())
                return [{'Config': {'Env': [k + '=' + v for k, v in config.items()],
                                    'Image': (root / 'compose.yaml').read_text()},
                         'State': {'Pid': 123}, 'Id': 'fixture'}]

            comparison.inspect = inspect
            response = unittest.mock.MagicMock()
            response.__enter__.return_value.status = 200
            response.__enter__.return_value.read.return_value = b'{"status":"ready"}'
            with patch.object(MODULE.urllib.request, 'urlopen', return_value=response):
                for pool in (32, 48, 64):
                    directory = output / str(pool)
                    directory.mkdir()
                    comparison.switch(pool, directory)
                    self.assertEqual((root / 'api.env').read_bytes(), original_env.replace(
                        b'CONNECTIONS=32', b'CONNECTIONS=' + str(pool).encode()))
                    for name in MODULE.CONFIGS:
                        if name not in ('api.env', 'compose.yaml'):
                            self.assertEqual((root / name).read_bytes(), comparison.original[name])
                directory = output / 'restore'
                directory.mkdir()
                comparison.switch(32, directory, restore=True)
                self.assertTrue(all((root / f).read_bytes() == raw for f, raw in comparison.original.items()))

    def test_main_restores_after_transition_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)
            marker = output / 'backup-local-verified'
            marker.write_text('')  # A transferred marker can be visible before its contents.
            (output / 'backup.json').write_text(json.dumps({'sha256': 'fixture-hash'}))
            fake = unittest.mock.MagicMock()
            fake.out = output
            fake.changed = False

            def failing_switch(*args):
                fake.changed = True
                raise RuntimeError('injected transition failure')

            def restore():
                fake.changed = False

            fake.switch.side_effect = failing_switch
            fake.restore.side_effect = restore
            with patch.object(MODULE, 'Comparison', return_value=fake), \
                 patch('sys.argv', ['compare-production-pool.py', '--root', temp, '--output', temp]), \
                 patch.object(MODULE.os, 'umask'), \
                 patch.object(MODULE.time, 'sleep', side_effect=lambda _: marker.write_text('fixture-hash\n')), \
                 patch.object(MODULE.signal, 'signal'):
                with self.assertRaisesRegex(RuntimeError, 'injected transition failure'):
                    MODULE.main()
            fake.restore.assert_called_once()
            result = json.loads((output / 'finished.json').read_text())
            self.assertFalse(result['comparison_completed'])
            self.assertTrue(result['original_configuration_restored'])


if __name__ == '__main__':
    unittest.main()
