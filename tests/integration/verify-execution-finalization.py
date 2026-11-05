#!/usr/bin/env python3
"""Repeat the affected execution suites 30 times, then run all server tests.

Each process gets a new disposable database on an explicitly local PostgreSQL
cluster. Only databases created by this run are removed. Production is unused.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from urllib.parse import urlsplit, urlunsplit
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests/support'))
from rust_test_binary import compile_binary, sources

ISOLATED = 'jobs::node_cleanup_tests::large_record_removal_is_queued_idempotent_and_completes_without_ssh'

FILTERS = ['execution::', 'background::', 'jobs::',
           'api::execution_rechecks::',
           'api::job_admission_tests::',
           'api::managed_mtls::tests::rotation_flow::',
           'api::managed_mtls::tests::publication_preparation::',
           'credentials::rotations::', 'credentials::node_mtls::',
           'dns::probe::persistence::tests::', 'dns::rotation::tests::',
           'dns::worker::records::tests::', 'dns::worker::refresh::tests::',
           'node_cleanup::ownership_tests::', 'node_limits::maintenance::',
           'proxy_monitor::token_tests::', 'deployment::progress_tests::']


def main():
    import psycopg
    from psycopg import sql
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rounds', type=int, default=30)
    parser.add_argument('--threads', type=int, default=4)
    args = parser.parse_args()
    url = os.environ.get('TEST_DATABASE_URL', '')
    parsed = urlsplit(url)
    if parsed.scheme not in ('postgres', 'postgresql') or parsed.hostname not in ('localhost', '127.0.0.1', '::1'):
        parser.error('TEST_DATABASE_URL must point to a disposable local PostgreSQL cluster')
    if args.rounds < 1 or args.threads < 1:
        parser.error('rounds and threads must be positive')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if (output / 'report.json').exists():
        parser.error('Use a new output directory')
    binary, digest, manifest = compile_binary(output)
    names = subprocess.check_output([str(binary), '--list'], text=True).splitlines()
    names = [line[:-6] for line in names if line.endswith(': test')]
    matrix = {prefix: sum(prefix in name for name in names) for prefix in FILTERS}
    if any(count == 0 for count in matrix.values()):
        raise RuntimeError('A required suite filter matched no tests: ' + str(matrix))
    selected = sum(any(prefix in name for prefix in FILTERS) for name in names)
    marker = uuid.uuid4().hex[:16]
    results = []
    report = dict(acceptance_passed=False, executable_sha256=digest, requested_rounds=args.rounds,
                  filters=matrix, selected_tests=selected, cases=results,
                  rust_min_stack=os.environ.get('RUST_MIN_STACK', '16777216'),
                  threads=args.threads, isolated_tests=[ISOLATED])
    for index in range(args.rounds + 1):
        full = index == args.rounds
        label = 'full' if full else f'round-{index + 1:02d}'
        database = 'hx_finish_' + marker + '_' + label.replace('-', '_')
        with psycopg.connect(url, autocommit=True) as admin:
            admin.execute(sql.SQL('CREATE DATABASE {} TEMPLATE template0').format(sql.Identifier(database)))
        child_url = urlunsplit(parsed._replace(path='/' + database))
        env = dict(os.environ, TEST_DATABASE_URL=child_url, RUST_MIN_STACK=report['rust_min_stack'])
        print('Running ' + label, flush=True)
        started = time.monotonic()
        segments = []
        try:
            commands = [
                ('parallel', [str(binary)] + ([] if full else FILTERS)
                 + ['--skip', ISOLATED, f'--test-threads={args.threads}']),
                ('isolated', [str(binary), ISOLATED, '--exact', '--test-threads=1'])]
            for segment, command in commands:
                path = output / (label + '-' + segment + '.log')
                with path.open('w') as log:
                    result = subprocess.run(command, cwd=ROOT, env=env, stdout=log,
                                            stderr=subprocess.STDOUT, timeout=3600)
                text = path.read_text()
                summary = re.search(r'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)
                item = dict(segment=segment, exit_code=result.returncode,
                            summary=summary.group(0) if summary else None)
                if summary:
                    item.update(passed=int(summary[2]), failed=int(summary[3]), ignored=int(summary[4]))
                item['verified'] = (result.returncode == 0 and summary is not None and item['failed'] == 0)
                segments.append(item)
                if not item['verified']:
                    break
        finally:
            with psycopg.connect(url, autocommit=True) as admin:
                admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(database)))
        expected = len(names) if full else selected
        case = dict(label=label, seconds=time.monotonic() - started, segments=segments,
                    passed=sum(item.get('passed', 0) for item in segments),
                    failed=sum(item.get('failed', 0) for item in segments),
                    ignored=sum(item.get('ignored', 0) for item in segments))
        case['verified'] = (len(segments) == 2 and all(item['verified'] for item in segments)
                            and case['passed'] + case['ignored'] == expected)
        case['summary'] = f"{case['passed']} passed; {case['failed']} failed; {case['ignored']} ignored"
        results.append(case)
        report['source_unchanged'] = sources() == manifest
        (output / 'report.json').write_text(json.dumps(report, indent=2))
        print(label + ': ' + str(case['summary']), flush=True)
        if not case['verified'] or not report['source_unchanged']:
            raise SystemExit(1)
    report['acceptance_passed'] = args.rounds >= 30 and all(case['verified'] for case in results)
    (output / 'report.json').write_text(json.dumps(report, indent=2))
    print('Repeated suites and full server regression passed', flush=True)


if __name__ == '__main__':
    main()
