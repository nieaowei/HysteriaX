#!/usr/bin/env python3
"""Compare actual pre-change/current workers in disposable PostgreSQL schemas.

Full acceptance is four sequential 15-minute cases (baseline/current x one/three
instances), 100k users, 10k nodes, 1m assignments, and 25ms response delay. The
profile exercises package, analytics and leased billing workers, not SSH or HTTP.
Use --smoke only to debug the fixture; smoke results never satisfy acceptance.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import time

ROOT = Path(__file__).resolve().parents[1]


def compile_tests(source, output, label):
    artifact_log = output / (label + '-compile.jsonl')
    with artifact_log.open('w') as log:
        result = subprocess.run(['cargo', 'test', '--locked', '-p', 'hysteriax-server',
                                 '--no-run', '--message-format=json', '--target-dir', str(ROOT / 'target')],
                                cwd=source, stdout=log, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError('Test build failed; inspect ' + str(artifact_log))
    executable = None
    for line in artifact_log.read_text().splitlines():
        try:
            item = json.loads(line)
        except ValueError:
            continue
        if item.get('reason') == 'compiler-artifact' and item.get('executable') and item.get('profile', {}).get('test'):
            if item.get('target', {}).get('name') == 'hysteriax-server':
                executable = Path(item['executable'])
    if executable is None:
        raise RuntimeError('Compiled test executable was not reported')
    destination = output / (label + '-tests')
    shutil.copy2(executable, destination)
    return destination


def baseline_source(output, commit):
    source = output / 'baseline-source'
    if source.exists():
        raise RuntimeError('Use a new output directory; baseline source already exists')
    source.mkdir()
    archive = output / 'baseline-source.tar'
    subprocess.run(['git', 'archive', '--format=tar', '--output', str(archive), commit], cwd=ROOT, check=True)
    with tarfile.open(archive) as bundle:
        bundle.extractall(source)
    # Inject only the common acceptance fixture, never implementation changes.
    for relative in ['crates/hysteriax-server/src/db/test_relay.rs',
                     'crates/hysteriax-server/src/node_limits/contention_benchmark.rs']:
        target = source / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / relative, target)
    with (source / 'crates/hysteriax-server/src/db.rs').open('a') as file:
        file.write('\n#[cfg(test)]\npub mod test_relay;\n')
    with (source / 'crates/hysteriax-server/src/node_limits.rs').open('a') as file:
        file.write('\n#[cfg(test)]\nmod contention_benchmark;\n')
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--baseline-commit', default='fb883ed')
    parser.add_argument('--smoke', action='store_true')
    parser.add_argument('--seconds', type=int)
    parser.add_argument('--instances', choices=['1', '3', 'both'], default='both')
    parser.add_argument('--reuse-baseline', action='store_true',
                        help='Keep completed baseline measurements and rebuild/rerun current cases in the same output directory')
    parser.add_argument('--reuse-completed-current', action='store_true',
                        help='With --reuse-baseline, keep successful current cases only if profile and binary SHA256 still match')
    args = parser.parse_args()
    if args.reuse_completed_current and not args.reuse_baseline:
        parser.error('--reuse-completed-current requires --reuse-baseline')
    if not os.environ.get('TEST_DATABASE_URL'):
        parser.error('TEST_DATABASE_URL must point to a disposable PostgreSQL database')
    seconds = args.seconds if args.seconds is not None else (15 if args.smoke else 900)
    if seconds < 900 and not args.smoke:
        parser.error('Full acceptance requires at least 900 seconds per case')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    previous = json.loads((output / 'report.json').read_text()) if args.reuse_completed_current else {}
    commit = subprocess.check_output(['git', 'rev-parse', args.baseline_commit], cwd=ROOT, text=True).strip()
    instances = [1, 3] if args.instances == 'both' else [int(args.instances)]
    reused = []
    if args.reuse_baseline:
        with (output / 'baseline-source.tar').open('rb') as archive:
            archived_commit = subprocess.check_output(['git', 'get-tar-commit-id'], stdin=archive,
                                                       cwd=ROOT, text=True).strip()
        if archived_commit != commit:
            raise RuntimeError('Archived baseline commit does not match the requested comparison')
        old_binary = output / 'baseline-tests'
        digest = hashlib.sha256(old_binary.read_bytes()).hexdigest()
        for count in instances:
            case = json.loads((output / f'baseline-{count}.json').read_text())
            expected = {'schema': 9, 'users': 1000 if args.smoke else 100000,
                        'nodes': 100 if args.smoke else 10000, 'instances': count,
                        'database_response_delay_ms': 25, 'measured_seconds': seconds}
            if any(case.get(key) != value for key, value in expected.items()):
                raise RuntimeError('Completed baseline profile does not match the requested comparison')
            case.update(label='baseline', executable_sha256=digest,
                        reused_baseline=True, exit_code=None)
            reused.append(case)
        print('Reusing completed baseline cases from verified git archive', flush=True)
    else:
        old_source = baseline_source(output, commit)
        old_binary = compile_tests(old_source, output, 'baseline')
    new_binary = compile_tests(ROOT, output, 'current')
    current_digest = hashlib.sha256(new_binary.read_bytes()).hexdigest()
    cases = reused
    runs = [('current', new_binary)] if args.reuse_baseline else [('baseline', old_binary), ('current', new_binary)]
    for label, executable in runs:
        for count in instances:
            cached = next((case for case in previous.get('cases', [])
                           if case.get('label') == 'current' and case.get('instances') == count
                           and case.get('exit_code') == 0 and case.get('executable_sha256') == current_digest
                           and case.get('measured_seconds') == seconds
                           and case.get('users') == (1000 if args.smoke else 100000)
                           and case.get('nodes') == (100 if args.smoke else 10000)
                           and case.get('database_response_delay_ms') == 25), None) if label == 'current' else None
            if cached is not None:
                cases.append(dict(cached, reused_current=True))
                print(f'Reusing completed current, {count} instance(s), matching profile and binary SHA256', flush=True)
                continue
            report_path = output / f'{label}-{count}.json'
            report_path.unlink(missing_ok=True)
            env = dict(os.environ, RUST_MIN_STACK='16777216',
                       HYSTERIAX_LOCK_BENCH_USERS='1000' if args.smoke else '100000',
                       HYSTERIAX_LOCK_BENCH_NODES='100' if args.smoke else '10000',
                       HYSTERIAX_LOCK_BENCH_SECONDS=str(seconds),
                       HYSTERIAX_LOCK_BENCH_INSTANCES=str(count),
                       HYSTERIAX_LOCK_BENCH_DELAY_MS='25',
                       HYSTERIAX_LOCK_BENCH_REPORT=str(report_path))
            print(f'Running {label}, {count} instance(s), {seconds}s', flush=True)
            with (output / f'{label}-{count}.log').open('w') as log:
                result = subprocess.run([str(executable), 'node_limits::contention_benchmark::benchmark_lock_contention',
                                         '--ignored', '--exact', '--nocapture', '--test-threads=1'],
                                        cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT,
                                        timeout=seconds+900)
            case = json.loads(report_path.read_text()) if report_path.exists() else {'report_missing': True}
            case.update(label=label, instances=count, exit_code=result.returncode,
                        executable_sha256=hashlib.sha256(executable.read_bytes()).hexdigest())
            cases.append(case)
    gates = []
    for count in instances:
        old = next(c for c in cases if c['label'] == 'baseline' and c.get('instances') == count)
        new = next(c for c in cases if c['label'] == 'current' and c.get('instances') == count)
        baseline_waits = old.get('observed_node_lock_waits', {})
        new_waits = new.get('observed_node_lock_waits', {})
        old_p95 = baseline_waits.get('p95_ms', 0)
        new_p95 = new_waits.get('p95_ms', 0)
        reduction = 1-new_p95/old_p95 if old_p95 and 'observed_node_lock_waits' in new else None
        gates.append({'instances': count,
                      'current_completed': new.get('exit_code') == 0,
                      'initial_due_within_60s': new.get('first_due_burst_seconds') is not None and new['first_due_burst_seconds'] <= 60,
                      'node_wait_p95_reduction': reduction,
                      'node_wait_p95_reduced_70_percent': reduction is not None and reduction >= .7,
                      'baseline_wait_observations': baseline_waits.get('count', 0),
                      'queues_drained': new.get('pending_after_drain') == 0,
                      'no_worker_or_source_errors': new.get('errors') == 0,
                      'billing_exact': new.get('usage_bytes') == new.get('ledger_rows') == new.get('ready_batches_injected') and new.get('unbilled_items') == 0})
    required = ['current_completed', 'initial_due_within_60s', 'node_wait_p95_reduced_70_percent',
                'queues_drained', 'no_worker_or_source_errors', 'billing_exact']
    selected_passed = all(all(g[key] for key in required) for g in gates)
    result = {'baseline_commit': commit, 'smoke': args.smoke, 'cases': cases, 'gates': gates,
              'selected_cases_passed': selected_passed,
              'acceptance_passed': not args.smoke and instances == [1, 3] and selected_passed,
              'production_changed': False, 'created_at_epoch': time.time()}
    (output / 'report.json').write_text(json.dumps(result, indent=2))
    print(json.dumps({'acceptance_passed': result['acceptance_passed'], 'smoke': args.smoke, 'gates': gates}, indent=2))
    return 0 if selected_passed or (args.smoke and all(c['exit_code'] == 0 for c in cases if c['label'] == 'current')) else 1


if __name__ == '__main__':
    sys.exit(main())
