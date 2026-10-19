#!/usr/bin/env python3
"""Compare the actual internal validator crate with the frozen 1,046-case evaluation baseline."""
from pathlib import Path
import json
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures/template-validator"


def main():
    with tempfile.TemporaryDirectory(prefix="hx-template-compatibility-") as folder:
        work = Path(folder)
        subprocess.run([sys.executable, str(FIXTURES / "generate_corpus.py"), str(FIXTURES / "inputs"), str(work / "corpus")], check=True)
        subprocess.run([sys.executable, str(FIXTURES / "generate_edges.py"), str(work)], check=True)
        (work / "src").mkdir()
        dependency = (ROOT / "crates/hysteriax-template-validator").as_posix()
        (work / "Cargo.toml").write_text(f'''[package]
name = "hx-template-compatibility"
version = "0.1.0"
edition = "2024"
[workspace]
[dependencies]
hysteriax-template-validator = {{ path = "{dependency}" }}
serde_json = "1"
''')
        (work / "src/main.rs").write_text('''use hysteriax_template_validator::{error_paths, Format};
use serde_json::{Value, json};
fn main() {
 let args:Vec<_>=std::env::args().collect();
 let format=if args[1]=="mihomo" {Format::Mihomo}else{Format::Singbox};
 let cases:Vec<Value>=serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
 let results:Vec<_>=cases.iter().map(|case| {
  let mut paths=error_paths(format,&case["config"]);paths.sort();paths.dedup();
  json!({"name":case["name"],"valid":paths.is_empty(),"paths":paths})
 }).collect();
 println!("{}",serde_json::to_string(&results).unwrap());
}
''')
        (work / "Cargo.lock").write_bytes((ROOT / "Cargo.lock").read_bytes())
        result = subprocess.run(
            ["cargo", "build", "--offline", "--quiet", "--message-format=json", "--manifest-path", str(work / "Cargo.toml"), "--target-dir", str(ROOT / "target")],
            capture_output=True, text=True, timeout=300, check=True,
        )
        artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        binary = next(a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact" and a.get("target", {}).get("name") == "hx-template-compatibility")
        count = 0
        for suite, baseline_file in [("corpus", "verdicts.json"), ("edges", "advanced-verdicts.json")]:
            baseline = json.loads((FIXTURES / baseline_file).read_text())
            for format_name in ["mihomo", "singbox"]:
                actual = json.loads(subprocess.check_output([binary, format_name, str(work / suite / (format_name + ".json"))], text=True))
                expected = baseline[format_name]
                assert actual == expected, f"Verdicts or field paths changed: {suite}/{format_name}"
                count += len(actual)
        assert count == 1046, count
        print("Actual validator crate: all 1,046 verdicts and normalized field-path sets match the frozen baseline")


if __name__ == "__main__":
    main()
