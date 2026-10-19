#!/usr/bin/env python3
"""Verify fixed-schema build gates, application-only cache reuse, and schema invalidation."""
from pathlib import Path
import hashlib
import json
import shutil
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/hysteriax-server"
RELATIVE = Path("src/api/subscriptions/templates")


def main():
    with tempfile.TemporaryDirectory(prefix="hx-schema-build-gate-") as folder:
        work = Path(folder)
        schema_dir = work / "crates/hysteriax-server" / RELATIVE
        schema_dir.mkdir(parents=True)
        shutil.copyfile(ROOT / "Cargo.lock", work / "Cargo.lock")
        for name in ["mihomo.schema.json", "singbox.schema.json", "schema_transform.rs"]:
            shutil.copyfile(SOURCE / RELATIVE / name, schema_dir / name)
        validator = work / "crates/hysteriax-template-validator"
        shutil.copytree(ROOT / "crates/hysteriax-template-validator", validator)
        # Each deliberately corrupted fixture gets an independent build-script identity.
        package_name = "hx-schema-gate-" + uuid.uuid4().hex[:12]
        manifest = validator / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('name = "hysteriax-template-validator"', f'name = "{package_name}"', 1))
        (work / "Cargo.toml").write_text('''[workspace]
members = ["crates/hysteriax-template-validator", "cache-client"]
resolver = "2"
[workspace.package]
edition = "2024"
license = "MIT"
rust-version = "1.89"
[workspace.dependencies]
serde_json = "1"
''')
        client = work / "cache-client"
        (client / "src").mkdir(parents=True)
        (client / "Cargo.toml").write_text(f'''[package]
name = "hx-schema-cache-client"
version = "0.1.0"
edition = "2024"
[dependencies]
hysteriax-template-validator = {{ package = "{package_name}", path = "../crates/hysteriax-template-validator" }}
serde_json = "1"
''')
        client_source = '''fn main() {
 let valid = hysteriax_template_validator::error_paths(hysteriax_template_validator::Format::Mihomo, &serde_json::json!({"mixed-port":7890})).is_empty();
 println!("{}", valid);
}
'''
        (client / "src/main.rs").write_text(client_source)

        def check(expected=None):
            result = subprocess.run(
                ["cargo", "build", "--offline", "--message-format=json", "--manifest-path", str(work / "Cargo.toml"),
                 "--target-dir", str(ROOT / "target")],
                capture_output=True, text=True, timeout=300,
            )
            combined = result.stdout + result.stderr
            if expected is not None:
                assert result.returncode != 0 and expected in combined, combined[-6000:]
                return None
            assert result.returncode == 0, combined[-6000:]
            artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
            library = next(a for a in artifacts if a.get("reason") == "compiler-artifact" and a.get("target", {}).get("kind") == ["lib"] and package_name in a["package_id"])
            binary = next(a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact" and a.get("target", {}).get("name") == "hx-schema-cache-client")
            rlib = next(Path(p) for p in library["filenames"] if p.endswith(".rlib"))
            digest = hashlib.sha256(rlib.read_bytes()).hexdigest()
            stamp = rlib.stat().st_mtime_ns
            output = subprocess.check_output([binary], text=True).strip()
            return library["fresh"], digest, stamp, output

        initial = check()
        assert initial[3] == "true", initial
        (client / "src/main.rs").write_text(client_source.replace('println!("{}", valid)', 'println!("application-only-edit:{}", valid)'))
        cached = check()
        assert cached[0] and cached[1:3] == initial[1:3], (initial, cached)
        assert cached[3] == "application-only-edit:true", cached

        schema = schema_dir / "mihomo.schema.json"
        original = schema.read_bytes()
        changed = json.loads(original)
        changed["properties"]["mixed-port"]["maximum"] = 1024
        schema.write_text(json.dumps(changed))
        rebuilt = check()
        assert not rebuilt[0] and rebuilt[3] == "application-only-edit:false", rebuilt

        schema.write_text('{"type":42}')
        check("invalid raw mihomo schema")
        schema.write_bytes(original)
        transform = schema_dir / "schema_transform.rs"
        source = transform.read_text()
        signature = "pub fn prepare(mut schema: Value) -> Value {"
        assert signature in source
        transform.write_text(source.replace(signature, signature + '\n    schema["type"] = json!(42);', 1))
        check("invalid prepared mihomo schema")
        print("Schema gates/cache passed: application-only edits reuse the same rlib; schema edits rebuild and change verdicts; invalid raw/prepared schemas rejected")


if __name__ == "__main__":
    main()
