#!/usr/bin/env python3
"""Freeze the four plan-0026 release/idle combinations and their test binaries."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile


ROOT = Path(__file__).resolve().parents[2]
RETENTION = "crates/hysteriax-server/src/credentials/retention.rs"
RETENTION_TESTS = "crates/hysteriax-server/src/credentials/retention_tests.rs"
BENCHMARK = "crates/hysteriax-server/src/credentials/retention_benchmarks.rs"
RELEASE = ["crates/hysteriax-server/src/" + name for name in
           ["db/mod.rs", "execution/mod.rs", "execution/finalization.rs"]]


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-revision", required=True)
    parser.add_argument("--candidate-revision", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    before, after = [git("rev-parse", "--verify", "--end-of-options", ref + "^{commit}")
                     .decode().strip() for ref in [args.baseline_revision, args.candidate_revision]]
    allowed = {RETENTION, RETENTION_TESTS, BENCHMARK,
               "crates/hysteriax-server/src/execution/tests.rs", *RELEASE}
    changed = git("diff", "--name-only", before, after).decode().splitlines()
    unexpected = [name for name in changed if name not in allowed and
                  (name in ["Cargo.toml", "Cargo.lock"] or
                   name.startswith("crates/") and Path(name).suffix in [".rs", ".sql", ".toml"])]
    if unexpected:
        parser.error("candidate has changes outside the release/idle input: " + ", ".join(unexpected))
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    fixture = (ROOT / BENCHMARK).read_bytes()
    variants = {}
    for name, release, idle in [("baseline", False, False), ("release_only", True, False),
                                ("idle_only", False, True), ("combined", True, True)]:
        source = output / (name + "-source")
        source.mkdir()
        with tarfile.open(fileobj=io.BytesIO(git("archive", "--format=tar", before))) as archive:
            for member in archive.getmembers():
                relative = Path(member.name)
                assert not relative.is_absolute() and ".." not in relative.parts
                assert not member.issym() and not member.islnk()
            archive.extractall(source)
        for file in RELEASE if release else []:
            (source / file).write_bytes(git("show", after + ":" + file))
        if idle:
            for file in [RETENTION, RETENTION_TESTS]:
                (source / file).write_bytes(git("show", after + ":" + file))
        else:
            file = source / RETENTION_TESTS
            text = file.read_text()
            if 'mod benchmarks;' not in text:
                file.write_text(text + '\n#[path = "retention_benchmarks.rs"]\nmod benchmarks;\n')
        (source / BENCHMARK).write_bytes(fixture)
        variants[name] = {
            "release_optimization": release, "idle_optimization": idle,
            "product_hashes": {file: hashlib.sha256((source / file).read_bytes()).hexdigest()
                               for file in [RETENTION, *RELEASE]},
            "benchmark_sha256": hashlib.sha256(fixture).hexdigest(),
        }
    (output / "variant-sources.json").write_text(json.dumps(
        {"base": before, "candidate": after, "variants": variants}, indent=2) + "\n")
    binaries = {}
    for name in variants:
        env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target"))
        build = output / (name + "-build.jsonl")
        with build.open("w") as stdout, (output / (name + "-build.log")).open("w") as stderr:
            subprocess.run(["cargo", "test", "--manifest-path", str(output / (name + "-source/Cargo.toml")),
                            "-p", "hysteriax-server", "--no-run", "--message-format=json"],
                           cwd=ROOT, env=env, stdout=stdout, stderr=stderr, check=True)
        artifacts = []
        for line in build.read_text().splitlines():
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if value.get("reason") == "compiler-artifact" and value.get("executable") and value.get("profile", {}).get("test"):
                artifacts.append(value["executable"])
        assert len(artifacts) == 1
        binary = output / (name + "-tests")
        # Shared Cargo output can be overwritten by the next variant's build.
        shutil.copy2(artifacts[0], binary)
        listing = subprocess.check_output([str(binary), "--list"], text=True)
        assert "credentials::retention::tests::benchmarks::background_contention_comparison: test" in listing
        binaries[name] = {"binary": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                          "test_summary": listing.splitlines()[-1]}
        (output / "variant-binaries.json").write_text(json.dumps(binaries, indent=2) + "\n")
        print(name + " frozen", flush=True)


if __name__ == "__main__":
    main()
