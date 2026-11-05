#!/usr/bin/env python3
"""Isolated finalization acceptance: 100k users / 10k nodes / 1m assignments.

One and three logical instances run sequentially for 15 minutes each, with
independent database pools/owners and 25ms PostgreSQL response delay. This is
worker integration, not an HTTP/SSH or multi-process failover benchmark.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
TEST = 'execution::finalization_benchmark::benchmark_execution_finalization'


sys.path.insert(0, str(ROOT / 'tests/support'))
from rust_test_binary import compile_binary, sources


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--smoke', action='store_true')
    parser.add_argument('--seconds', type=int)
    args = parser.parse_args()
    database = urlsplit(os.environ.get('TEST_DATABASE_URL', ''))
    if (database.scheme not in ('postgres', 'postgresql')
            or database.hostname not in ('localhost', '127.0.0.1', '::1')):
        parser.error('TEST_DATABASE_URL must point to a disposable local PostgreSQL cluster')
    seconds = args.seconds if args.seconds is not None else (10 if args.smoke else 900)
    if seconds < 1 or (not args.smoke and seconds < 900):
        parser.error('Formal acceptance requires at least 900 seconds per case')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if (output / 'report.json').exists():
        parser.error('Use a new output directory to preserve prior evidence')
    executable, digest, manifest = compile_binary(output)
    cases = []
    for instances in [1, 3]:
        report = output / f'instances-{instances}.json'
        env = dict(os.environ, HYSTERIAX_LOCK_BENCH_USERS='1000' if args.smoke else '100000',
                   HYSTERIAX_LOCK_BENCH_NODES='100' if args.smoke else '10000',
                   HYSTERIAX_LOCK_BENCH_SECONDS=str(seconds),
                   HYSTERIAX_LOCK_BENCH_INSTANCES=str(instances),
                   HYSTERIAX_LOCK_BENCH_DELAY_MS='25', HYSTERIAX_FINALIZATION_REPORT=str(report))
        print(f'Running {instances} instance(s), {seconds} measured seconds', flush=True)
        with (output / f'instances-{instances}.log').open('w') as log:
            started = time.monotonic()
            process = subprocess.Popen([str(executable), TEST, '--exact', '--ignored',
                                        '--nocapture', '--test-threads=1'], cwd=ROOT, env=env,
                                       stdout=log, stderr=subprocess.STDOUT)
            while process.poll() is None:
                if time.monotonic() - started > seconds + 600:
                    process.terminate()
                    process.wait(timeout=30)
                    raise RuntimeError('Acceptance process exceeded its bounded setup/drain budget')
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    pass
        case = json.loads(report.read_text()) if report.exists() else {}
        case.update(exit_code=process.returncode, executable_sha256=digest)
        cases.append(case)
        if process.returncode:
            break
    passed = (not args.smoke and len(cases) == 2 and all(
        case.get('exit_code') == 0 and case.get('measured_seconds', 0) >= 900
        and case.get('users') == 100000 and case.get('nodes') == 10000
        and case.get('assignments') == 1000000 and case.get('errors') == 0
        and case.get('pending_after_drain') == 0
        and case.get('execution_outcomes', {}).get('lost') == 0
        and case.get('execution_outcomes', {}).get('recovery_required') == 0 for case in cases))
    final = dict(acceptance_passed=passed, smoke=args.smoke, cases=cases,
                 source_unchanged=sources() == manifest)
    (output / 'report.json').write_text(json.dumps(final, indent=2))
    if (not final['source_unchanged'] or any(case.get('exit_code') != 0 for case in cases)
            or (not args.smoke and not passed)):
        raise SystemExit(1)
    print('Acceptance passed' if passed else 'Smoke only; formal acceptance not claimed', flush=True)


if __name__ == '__main__':
    main()
