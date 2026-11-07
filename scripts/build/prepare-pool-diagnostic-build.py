#!/usr/bin/env python3
"""Freeze a temporary diagnostic build, applying a checksum-verified SQLx timing patch.

Normal application sources, Cargo.lock and Dockerfile remain unchanged. Build and test
from the returned context, never from a live production checkout. Requires git, patch
and the exact sqlx-core .crate in the local Cargo cache (cargo fetch populates it).
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import tarfile
import re


def sha(data):
    return hashlib.sha256(data).hexdigest()


def unpack(archive, destination, strip_prefix=None):
    for entry in archive.getmembers():
        name = PurePosixPath(entry.name)
        if strip_prefix:
            if name.parts[0] != strip_prefix:
                raise ValueError('unexpected crate archive prefix')
            name = PurePosixPath(*name.parts[1:])
        if name.is_absolute() or '..' in name.parts or not (entry.isfile() or entry.isdir()):
            raise ValueError('unsupported archive member')
        path = destination / str(name)
        if entry.isdir():
            path.mkdir(parents=True, exist_ok=True)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.extractfile(entry).read())
            path.chmod(entry.mode & 0o777)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    if subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=normal'], cwd=root).strip():
        raise SystemExit('Commit diagnostic inputs before freezing a build context')
    output = args.output.resolve()
    if output.exists():
        raise SystemExit('Output must not exist')
    output.mkdir(parents=True)
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    raw = subprocess.check_output(['git', 'archive', revision], cwd=root)
    (output / 'source.tar').write_bytes(raw)
    context = output / 'source'
    context.mkdir()
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        unpack(archive, context)
    lock = (context / 'Cargo.lock').read_text()
    sqlx_blocks = [b for b in lock.split('[[package]]') if '\nname = \"sqlx-core\"\n' in b]
    if len(sqlx_blocks) != 1:
        raise SystemExit('Unexpected SQLx lock entries')
    package = dict(re.findall(r'^(name|version|source|checksum) = \"([^\"]+)\"$', sqlx_blocks[0], re.MULTILINE))
    if package['version'] != '0.9.0' or not package.get('source', '').startswith('registry+'):
        raise SystemExit('SQLx version/source changed: review and regenerate the patch')
    cargo_cache = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))) / 'registry' / 'cache'
    candidates = list(cargo_cache.glob('*/sqlx-core-0.9.0.crate'))
    crate = next((p for p in candidates if sha(p.read_bytes()) == package['checksum']), None)
    if crate is None:
        raise SystemExit('Exact locked sqlx-core crate not cached; run cargo fetch')
    dependency = context / '.diagnostic-deps' / 'sqlx-core'
    dependency.mkdir(parents=True)
    with tarfile.open(crate) as archive:
        unpack(archive, dependency, 'sqlx-core-0.9.0')
    patch = context / 'scripts' / 'build' / 'sqlx-pool-timing.patch'
    subprocess.run(['patch', '--batch', '--fuzz=0', '-p1', '-i', str(patch)], cwd=dependency, check=True)
    manifest = context / 'Cargo.toml'
    manifest.write_text(manifest.read_text() + '\n[patch.crates-io]\nsqlx-core = { path = ".diagnostic-deps/sqlx-core" }\n')
    blocks = lock.split('[[package]]')
    matched = 0
    for i, block in enumerate(blocks):
        if '\nname = "sqlx-core"\n' in block:
            blocks[i] = '\n'.join(line for line in block.split('\n') if not line.startswith(('source = ', 'checksum = ')))
            matched += 1
    if matched != 1:
        raise SystemExit('Unexpected SQLx lock entries')
    (context / 'Cargo.lock').write_text('[[package]]'.join(blocks))
    # These tests deliberately require the patch; they are only registered in this frozen context.
    module = context / 'crates' / 'hysteriax-server' / 'src' / 'db' / 'mod.rs'
    module.write_text(module.read_text() + '\n#[cfg(test)]\nmod pool_phase_diagnostics_tests;\n')
    dockerfile = context / 'Dockerfile'
    text = dockerfile.read_text()
    marker = 'COPY deploy deploy\n'
    if text.count(marker) != 1:
        raise SystemExit('Dockerfile changed: review diagnostic COPY placement')
    text = text.replace(marker, marker + 'COPY .diagnostic-deps .diagnostic-deps\n')
    build = 'cargo build --locked --release -p hysteriax-server'
    if text.count(build) != 1:
        raise SystemExit('Docker build command changed: review diagnostic cache invalidation')
    dockerfile.write_text(text.replace(build, 'cargo clean --locked -p sqlx-core && ' + build))
    metadata = {'source_revision': revision, 'source_archive_sha256': sha(raw),
                'sqlx_core_version': package['version'], 'upstream_crate_sha256': package['checksum'],
                'diagnostic_patch_sha256': sha(patch.read_bytes()),
                'cargo_manifest_sha256': sha(manifest.read_bytes()),
                'cargo_lock_sha256': sha((context / 'Cargo.lock').read_bytes()),
                'dockerfile_sha256': sha(dockerfile.read_bytes()),
                'diagnostic_only': True, 'schema_changes': False,
                'context': str(context)}
    (output / 'build-inputs.json').write_text(json.dumps(metadata, indent=2) + '\n')
    print(json.dumps(metadata))


if __name__ == '__main__':
    main()
