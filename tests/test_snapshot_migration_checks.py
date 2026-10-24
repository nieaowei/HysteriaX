import datetime
import importlib.util
from pathlib import Path
import unittest
import uuid

SPEC = importlib.util.spec_from_file_location('snapshot_migration_checks',Path(__file__).resolve().parents[1]/'scripts/snapshot_migration_checks.py')
checks = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checks)


class SampleReferenceTests(unittest.TestCase):
    def test_cross_node_dedup_and_frozen_coverage(self):
        cycle = uuid.uuid4()
        now = datetime.datetime(2026,1,1,tzinfo=datetime.timezone.utc)
        values = checks.cycle_values([(cycle,'a',now,'ok','ok',{'u':2},3),
                                      (cycle,'b',now,'ok','failed',{'u':3,'v':4},3)])
        self.assertEqual(values[(cycle,'')],(cycle,'',now,2,9,2,3,True,1,True))
        self.assertEqual(values[(cycle,'b')],(cycle,'b',now,2,7,1,1,False,0,True))

    def test_successful_zero_and_failure_remain_distinct(self):
        cycle = uuid.uuid4()
        now = datetime.datetime(2026,1,1,tzinfo=datetime.timezone.utc)
        zero = checks.cycle_values([(cycle,'a',now,'ok','ok',{},1)])
        failed = checks.cycle_values([(cycle,'a',now,'failed','failed',{},1)])
        self.assertEqual(zero[(cycle,'')],(cycle,'',now,0,0,1,1,False,1,False))
        self.assertEqual(failed[(cycle,'')],(cycle,'',now,None,None,0,1,True,0,True))
        self.assertEqual(failed[(cycle,'a')],(cycle,'a',now,None,None,0,1,True,0,True))


if __name__=='__main__':
    unittest.main()
