import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC=importlib.util.spec_from_file_location('fixture_network_guard',Path(__file__).resolve().parents[1]/'scripts/fixture_network_guard.py')
guard=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)
OWNER='a'*32


class FixtureNetworkGuardTests(unittest.TestCase):
    def test_invalid_owner_never_queries_or_mutates_docker(self):
        with patch.object(guard,'docker') as docker:
            with self.assertRaisesRegex(RuntimeError,'fresh UUID'):
                guard.apply_guard('target','pg','redis',owner='shared',image='fixture')
            docker.assert_not_called()

    def test_unowned_container_is_rejected_before_running_a_privileged_helper(self):
        data={'Config':{'Labels':{guard.OWNER_LABEL:'another-owner'}}}
        with patch.object(guard,'docker',return_value=json.dumps([data])) as docker:
            with self.assertRaisesRegex(RuntimeError,'not owned'):
                guard.apply_guard('target','pg','redis',owner=OWNER,image='fixture')
            self.assertEqual(docker.call_count,1)
            self.assertEqual(docker.call_args.args,('inspect','target'))

    def test_host_or_shared_namespace_or_privileged_target_is_rejected(self):
        for mode,privileged in [('host',False),('container:another',False),('private',True)]:
            data={'Config':{'Labels':{guard.OWNER_LABEL:OWNER}},'HostConfig':{'NetworkMode':mode,'Privileged':privileged}}
            with self.subTest(mode=mode,privileged=privileged),patch.object(guard,'docker',return_value=json.dumps([data])) as docker:
                with self.assertRaisesRegex(RuntimeError,'own non-host'):
                    guard.apply_guard('target','pg','redis',owner=OWNER,image='fixture')
                self.assertEqual(docker.call_count,1)

    def test_an_unowned_network_prevents_privileged_dispatch(self):
        data={'Config':{'Labels':{guard.OWNER_LABEL:OWNER}},'HostConfig':{'NetworkMode':'private','Privileged':False},'NetworkSettings':{'Networks':{'private':{'IPAddress':'192.0.2.2'}}}}
        with patch.object(guard,'docker',side_effect=[json.dumps([data]),json.dumps([{'Labels':{}}])]) as docker:
            with self.assertRaisesRegex(RuntimeError,'unowned fixture network'):
                guard.apply_guard('target','pg','redis',owner=OWNER,image='fixture')
            self.assertEqual([call.args[0] for call in docker.call_args_list],['inspect','network'])

    def test_database_outage_or_restore_rejects_invalid_owner_before_docker(self):
        for available in (False,True):
            with self.subTest(available=available),patch.object(guard,'docker') as docker:
                with self.assertRaisesRegex(RuntimeError,'fresh UUID'):
                    guard.set_postgres_available('target','pg','redis',owner='shared',image='fixture',available=available)
                docker.assert_not_called()

    def test_database_outage_or_restore_cannot_mutate_an_unowned_container(self):
        data={'Config':{'Labels':{guard.OWNER_LABEL:'another-owner'}}}
        for available in (False,True):
            with self.subTest(available=available),patch.object(guard,'docker',return_value=json.dumps([data])) as docker:
                with self.assertRaisesRegex(RuntimeError,'not owned'):
                    guard.set_postgres_available('target','pg','redis',owner=OWNER,image='fixture',available=available)
                self.assertEqual(docker.call_args.args,('inspect','target'))
                self.assertEqual(docker.call_count,1)


if __name__=='__main__':unittest.main()
