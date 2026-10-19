#!/usr/bin/env python3
"""Exercise the production template validators in fresh processes, with Linux RSS limits."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", help="Run a locally built Linux image instead of the local binary")
    parser.add_argument("--binary", type=Path, help="Use an already built server binary")
    args = parser.parse_args()
    if args.image and args.binary:
        parser.error("choose --image or --binary")
    if args.image:
        command = ["docker", "run", "--rm", "--network", "none", args.image, "--check-template-runtime"]
    else:
        if not args.binary:
            subprocess.run(["cargo", "build", "--locked", "-q"], cwd=ROOT, check=True)
        binary = args.binary or ROOT / "target/debug/hysteriax-server"
        command = [str(binary.resolve()), "--check-template-runtime"]
    for run in range(3):
        result = subprocess.run(command, capture_output=True, text=True, check=True, timeout=90)
        output = json.loads(result.stdout)
        assert output["status"] == "ok" and output["invalid_configurations_rejected"]
        assert output["formats"] == ["mihomo", "singbox"]
        assert output["repeat_passes"] == 16 and output["concurrent_workers"] == 4
        after, warmed = output["after"], output["warmed"]
        if after["peak_rss_kib"] is not None:
            assert after["peak_rss_kib"] <= 96 * 1024, output
            assert after["rss_kib"] - warmed["rss_kib"] <= 16 * 1024, output
            assert after["swap_kib"] == 0, output
        print(json.dumps({"run": run + 1, **output}, sort_keys=True))
    print("Subscription schema runtime regression passed")


if __name__ == "__main__":
    main()
