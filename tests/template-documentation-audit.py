#!/usr/bin/env python3
"""Regression checks for documentation extraction and scoped schema evidence."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


order = module('doc_order_test', 'scripts/generate-template-documentation-order.py')
audit = module('doc_audit_test', 'scripts/audit-template-documentation.py')
website = module('doc_website_test', 'scripts/audit-template-website.py')


class DocumentationAuditTests(unittest.TestCase):
    def test_grouped_fields_share_the_real_heading_anchor(self):
        fields, anchors = order.extract('#### `up_mbps`, `down_mbps`\n')
        self.assertEqual(fields, ['up_mbps', 'down_mbps'])
        self.assertEqual(anchors, {'up_mbps': 'up_mbps-down_mbps', 'down_mbps': 'up_mbps-down_mbps'})

    def test_duplicate_headings_do_not_guess_a_direction(self):
        fields, anchors = order.extract('#### enabled\n#### padding\n#### enabled\n#### padding\n')
        self.assertEqual(fields, ['enabled', 'padding'])
        self.assertEqual(anchors, {'enabled': '', 'padding': ''})

    def test_examples_are_not_fields_or_defaults(self):
        fields, _ = order.extract('### Structure\n```json\n{"level":"debug"}\n```\n#### level\n')
        self.assertEqual(fields, ['level'])

    def test_incomplete_or_wrong_source_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            source = Path(folder) / 'configuration'
            source.mkdir()
            (source.parent / 'schema.json').write_text('{}')
            with self.assertRaises(ValueError): order.verify_source(source)
            shipped = ROOT / 'crates/hysteriax-server/src/api/subscriptions/templates/singbox.schema.json'
            (source.parent / 'schema.json').write_bytes(shipped.read_bytes())
            with self.assertRaises(ValueError): order.verify_source(source)

    def test_protocol_scope_does_not_accept_another_protocols_field(self):
        schema = json.loads((ROOT / 'crates/hysteriax-server/src/api/subscriptions/templates/singbox.schema.json').read_text())
        selected = audit.scope_for('dns/server/fakeip', schema)
        self.assertEqual(len(selected), 1)
        node, pointer = selected[0]
        evidence = audit.fields(node, schema, pointer)
        self.assertIn('inet6_range', evidence)
        self.assertNotIn('server_port', evidence)
        for path in evidence['inet6_range']:
            resolved = schema
            for key in path.lstrip('/').split('/'):
                resolved = resolved[int(key)] if isinstance(resolved, list) else resolved[key]
            self.assertEqual(resolved['type'], 'string')

    def test_nested_union_branches_keep_valid_evidence_paths(self):
        schema = {'$defs': {'Thing': {'oneOf': [{'oneOf': [{'properties': {'type': {'const': 'snell'}, 'version': {'const': 6}}}]}]}}}
        located = list(audit.located_alternatives(schema['$defs']['Thing'], schema, '/$defs/Thing'))
        self.assertEqual(located[0][1], '/$defs/Thing/oneOf/0/oneOf/0')

    def test_live_navigation_is_not_treated_as_configuration_fields(self):
        page = website.Page()
        page.feed('<nav><h3>wrong_field</h3></nav><article><h4 id="right_field">right_field<a>¶</a></h4><p>自 1.15.0 起</p></article>')
        self.assertEqual(page.headings, ['right_field'])
        self.assertIn('1.15.0', ''.join(page.text))


if __name__ == '__main__': unittest.main()
