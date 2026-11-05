"""Build and archive an exact Rust server test executable and its inputs."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def sources():
    paths = [ROOT / 'Cargo.toml', ROOT / 'Cargo.lock']
    paths.extend(p for p in (ROOT / 'crates').rglob('*') if p.is_file()
                 and p.suffix in ('.rs', '.toml', '.sql', '.lua'))
    for root in ['tests/fixtures','openapi','deploy']:
        paths.extend(p for p in (ROOT/root).rglob('*') if p.is_file())
    paths.extend(ROOT/relative for relative in ['tests/requirements.txt',
        'tests/support/rust_test_binary.py','tests/integration/verify-execution-finalization.py',
        'tests/performance/verify-execution-finalization.py'])
    return {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(paths)}


def compile_binary(output):
    manifest = sources()
    (output / 'source-files.sha256.json').write_text(json.dumps(manifest, indent=2))
    artifact = output / 'build.jsonl'
    with artifact.open('w') as log:
        subprocess.run(['cargo', 'test', '--locked', '-p', 'hysteriax-server', '--no-run',
                        '--message-format=json'], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                       check=True)
    binary = None
    for line in artifact.read_text().splitlines():
        try:
            item = json.loads(line)
        except ValueError:
            continue
        if (item.get('reason') == 'compiler-artifact' and item.get('executable')
                and item.get('profile', {}).get('test')
                and item.get('target', {}).get('name') == 'hysteriax-server'):
            binary = Path(item['executable'])
    if binary is None or sources() != manifest:
        raise RuntimeError('Test executable unavailable or source changed during build')
    executable = output / 'server-tests'
    shutil.copy2(binary, executable)
    digest = hashlib.sha256(executable.read_bytes()).hexdigest()
    return executable, digest, manifest
