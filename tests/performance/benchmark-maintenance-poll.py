#!/usr/bin/env python3
"""Compare frozen maintenance poll binaries over bounded loopback latency."""
import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--database-url-file', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    parser.add_argument('--delay-ms', type=float, default=25)
    parser.add_argument('--iterations', type=int, default=10)
    parser.add_argument('--candidate-first', action='store_true')
    args = parser.parse_args()
    if not 10 <= args.iterations <= 100 or not 0 <= args.delay_ms <= 100:
        parser.error('iterations must be 10..100 and delay-ms 0..100')
    if args.baseline.resolve() == args.candidate.resolve():
        parser.error('distinct frozen binaries are required')
    backend = urlsplit(args.database_url_file.read_text().strip())
    if backend.scheme not in ('postgres', 'postgresql') or backend.hostname not in ('127.0.0.1', 'localhost', '::1'):
        parser.error('a disposable loopback PostgreSQL URL is required')
    args.output_dir.mkdir(parents=True, exist_ok=True)
    peers = set()

    async def relay(reader, writer, delay):
        while data := await reader.read(65536):
            if delay:
                await asyncio.sleep(delay)
            writer.write(data)
            await writer.drain()

    async def connect(reader, writer):
        task = asyncio.current_task()
        peers.add(task)
        remote = None
        pending = []
        try:
            upstream, remote = await asyncio.open_connection(backend.hostname, backend.port or 5432)
            pending = [asyncio.create_task(relay(reader, remote, 0)),
                       asyncio.create_task(relay(upstream, writer, args.delay_ms / 1000))]
            await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for item in pending:
                item.cancel()
            await asyncio.gather(*pending, return_exceptions=True)
            for stream in (writer, remote):
                if stream:
                    stream.close()
            peers.discard(task)

    server = await asyncio.start_server(connect, '127.0.0.1', 0)
    port = server.sockets[0].getsockname()[1]
    credentials = backend.netloc.rsplit('@', 1)[0] + '@' if '@' in backend.netloc else ''
    options = dict(parse_qsl(backend.query))
    options['sslmode'] = 'disable'
    environment = dict(os.environ)
    for key in list(environment):
        if key in ('DATABASE_URL', 'REDIS_URL', 'TEST_REDIS_URL') or key.startswith('HYSTERIAX_'):
            environment.pop(key, None)
    environment.update(TEST_DATABASE_URL=urlunsplit((backend.scheme,
        credentials + '127.0.0.1:' + str(port), backend.path, urlencode(options), '')),
        HX_MAINTENANCE_POLL_ITERATIONS=str(args.iterations), RUST_MIN_STACK='16777216')
    binaries = [('baseline', args.baseline), ('candidate', args.candidate)]
    if args.candidate_first:
        binaries.reverse()
    results = {}
    try:
        for name, binary in binaries:
            binary = binary.resolve()
            digest = hashlib.sha256(binary.read_bytes()).hexdigest()
            with (args.output_dir / (name + '.log')).open('wb') as output:
                process = await asyncio.create_subprocess_exec(str(binary), '--ignored', '--exact',
                    'background::maintenance::poll_tests::maintenance_poll_comparison',
                    '--nocapture', '--test-threads=1', env=environment,
                    stdout=output, stderr=asyncio.subprocess.STDOUT)
                try:
                    code = await asyncio.wait_for(process.wait(), timeout=600)
                except BaseException:
                    process.kill()
                    await process.wait()
                    raise
            if code:
                raise RuntimeError(name + ' benchmark failed; inspect its log')
            scenarios = {}
            for line in (args.output_dir / (name + '.log')).read_text().splitlines():
                if 'MAINTENANCE_POLL_BENCHMARK ' in line:
                    value = json.loads(line.split('MAINTENANCE_POLL_BENCHMARK ', 1)[1])
                    scenarios[value['scenario']] = value
            if set(scenarios) != {'idle', 'one_resource', 'three_resources', 'nine_resources'}:
                raise RuntimeError(name + ' did not report all scenarios')
            if hashlib.sha256(binary.read_bytes()).hexdigest() != digest:
                raise RuntimeError('benchmark binary changed')
            results[name] = {'binary_sha256': digest, 'scenarios': scenarios}
        result = {'delay_ms': args.delay_ms, 'iterations': args.iterations,
                  'order': [name for name, _ in binaries], 'results': results,
                  'scope': 'isolated microbenchmark, not production or capacity acceptance'}
        (args.output_dir / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
        for scenario in results['baseline']['scenarios']:
            print(json.dumps({'scenario': scenario, **{
                name: results[name]['scenarios'][scenario] for name in ['baseline', 'candidate']}}), flush=True)
    finally:
        server.close()
        await server.wait_closed()
        for task in list(peers):
            task.cancel()
        await asyncio.gather(*list(peers), return_exceptions=True)


if __name__ == '__main__':
    asyncio.run(main())
