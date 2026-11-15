#!/usr/bin/env python3
"""Summarize retained production crossover evidence; never contacts production."""
import argparse
import collections
import datetime
import json
from pathlib import Path
import re
import statistics


def epoch(timestamp):
    # Python 3.9 only accepts three/six fractional digits; PG JSON omits zeros.
    value = timestamp.replace('Z', '+00:00')
    value = re.sub(r'\.(\d+)(?=[+-]\d\d:\d\d$)', lambda m: '.' + m.group(1).ljust(6, '0')[:6], value)
    return datetime.datetime.fromisoformat(value).timestamp()


def distribution(values):
    ordered = sorted(values)
    if not ordered:
        return {'count': 0}

    def percentile(p):
        index = (len(ordered) - 1) * p
        low = int(index)
        return ordered[low] + (ordered[min(low + 1, len(ordered) - 1)] - ordered[low]) * (index - low)

    return {'count': len(ordered), 'median': statistics.median(ordered),
            'p95': percentile(.95), 'p99': percentile(.99), 'max': ordered[-1]}


def fields(line):
    result = {}
    for key, value in re.findall(r'\b([a-z_]+)=("[^"]*"|[^\s]+)', line):
        if value.startswith('"'):
            result[key] = value[1:-1]
        else:
            try:
                result[key] = float(value) if any(c in value for c in '.eE') else int(value)
            except ValueError:
                result[key] = value
    return result


def phase_summary(records):
    acquires = [r for r in records if r.get('operation') == 'acquire']
    releases = [r for r in records if r.get('operation') == 'release']
    phases = ['permit_wait', 'checkout_ping', 'connect', 'initialize',
              'before_acquire', 'connect_backoff', 'discard', 'unattributed']
    return {'slow_acquire_count': len(acquires),
            'slow_acquire_total_seconds': sum(r.get('total_secs', 0) for r in acquires),
            'slow_acquire_max_seconds': max((r.get('total_secs', 0) for r in acquires), default=0),
            'slow_acquire_outcomes': dict(collections.Counter(r.get('outcome') for r in acquires)),
            'slow_acquire_phase_seconds': {p: sum(r.get(p + '_secs', 0) for r in acquires) for p in phases},
            'slow_acquire_max_waiters': max((r.get('pool_waiters_start', 0) for r in acquires), default=0),
            'slow_acquire_examples': sorted(acquires, key=lambda r: r.get('total_secs', 0), reverse=True)[:5],
            'logged_release_count': len(releases),
            'biased_snapshot_count': len(records),
            'biased_snapshot_waiters': distribution([r['pool_waiters_start'] for r in records if 'pool_waiters_start' in r]),
            'biased_snapshot_pool_size': distribution([r['pool_size_start'] for r in records if 'pool_size_start' in r]),
            'biased_snapshot_zero_idle': sum(r.get('pool_idle_start') == 0 for r in records),
            'release_over_one_second': sum(r.get('total_secs', 0) >= 1 for r in releases),
            'release_max_seconds': max((r.get('total_secs', 0) for r in releases), default=0)}


def summarize(directory):
    window = json.loads((directory / 'window.json').read_text())
    start, end = window['stable_start_epoch'], window['stable_end_epoch']
    raw = re.sub(r'\x1b\[[0-9;]*m', '', (directory / 'api.log').read_text())
    stable_lines, phases, startup_phases = [], [], []
    for line in raw.splitlines():
        match = re.search(r'\d{4}-\d\d-\d\dT\S+Z', line)
        if not match:
            continue
        timestamp = match.group()
        event_epoch = epoch(timestamp)
        if start <= event_epoch < end:
            stable_lines.append(line)
        if 'pool operation phase timings' in line:
            record = dict(fields(line), timestamp=timestamp)
            if start <= event_epoch < end:
                phases.append(record)
            elif event_epoch < start:
                startup_phases.append(record)
    http = [json.loads(line) for line in (directory / 'http.jsonl').read_text().splitlines()]
    http = [r for r in http if r['stable']]
    resources = [json.loads(line) for line in (directory / 'resources.jsonl').read_text().splitlines()]
    resources = [r for r in resources if r['stable']]
    cpu = None
    if len(resources) > 1:
        first, last = resources[0], resources[-1]
        seconds = last['epoch'] - first['epoch']

        def usage(r):
            return int(dict(line.split() for line in r['cpu.stat'].splitlines())['usage_usec'])

        cpu = {'observed_seconds': seconds, 'cpu_seconds': (usage(last) - usage(first)) / 1e6,
               'average_cores': (usage(last) - usage(first)) / 1e6 / seconds}
    sessions = [r['database']['sessions'] for r in resources]
    new_backends = {s['pid'] for ss in sessions for s in ss if
                    epoch(s['backend_start']) >= start}
    quality = json.loads((directory / 'quality.json').read_text())
    return {'stage': directory.name, 'pool': int(directory.name.rsplit('-', 1)[1]), 'window': window,
            'quality': quality, 'all_work_complete': all(r['total'] == r['complete'] for r in quality['work']),
            'stable_acquire': phase_summary(phases), 'startup_acquire': phase_summary(startup_phases),
            'readyz_seconds': distribution([r['seconds'] for r in http]),
            'readyz_status': dict(collections.Counter(str(r['status']) for r in http)),
            'api_cpu': cpu, 'api_memory_bytes': distribution([int(r['memory.current']) for r in resources]),
            'database_same_user_sessions': distribution([len(ss) for ss in sessions]),
            'database_new_backends_observed': len(new_backends),
            'observer_roundtrip_seconds': distribution([r['query_seconds'] for r in resources]),
            'stable_logs': {'errors': sum(' ERROR ' in s for s in stable_lines),
                'warnings': sum(' WARN ' in s for s in stable_lines),
                'slow_sql': sum('slow statement' in s or 'slow query' in s for s in stable_lines),
                'original_slow_acquire': sum('acquired connection, but time to acquire exceeded slow threshold' in s for s in stable_lines),
                'panics': sum('panicked at' in s for s in stable_lines)}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence', type=Path)
    args = parser.parse_args()
    stages = [summarize(p) for p in sorted(args.evidence.glob('[0-9][0-9]-pool-*'))
              if all((p / f).is_file() for f in ['quality.json', 'api.log', 'window.json', 'http.jsonl', 'resources.jsonl'])]
    result = {'stages': stages,
              'limits': 'Three stable minutes per stage. Natural sequential production load; no randomized control or capacity acceptance. '
                        'Acquire phase logs are threshold-biased (>2s), not a full acquire distribution. '
                        'PG sessions include other same-user connections, not SQLx idle/size. CPU is measured cgroup time.'}
    (args.evidence / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps([{'stage': s['stage'], 'work_complete': s['all_work_complete'],
                      'slow_acquire': s['stable_acquire']['slow_acquire_count'],
                      'slow_acquire_max': s['stable_acquire']['slow_acquire_max_seconds'],
                      'permit_seconds': s['stable_acquire']['slow_acquire_phase_seconds']['permit_wait'],
                      'readyz': s['readyz_seconds'], 'cpu': s['api_cpu'],
                      'memory_max': s['api_memory_bytes'].get('max')} for s in stages], indent=2))


if __name__ == '__main__':
    main()
