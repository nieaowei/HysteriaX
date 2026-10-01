#!/usr/bin/env python3
"""Run the macOS UI workflow against the management service in the local .env."""

import json
import pathlib
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
APP_SOURCES = ROOT / "apps/macos/Sources/HysteriaX"
UI_TEST_SOURCE = ROOT / "apps/macos/UITests/HysteriaXLiveUITests.swift"


def read_env():
    values = {}
    for line in (ROOT / ".env").read_text().splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def request(base, token, path, method="GET"):
    req = urllib.request.Request(
        base + path,
        headers={"Authorization": f"Bearer {token}"},
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as response:
            body = response.read()
            return response.status, json.loads(body) if body else None
    except urllib.error.HTTPError as error:
        body = error.read()
        try:
            return error.code, json.loads(body) if body else None
        except Exception:
            return error.code, None


def cleanup_test_user(base, token, name):
    status, payload = request(base, token, "/api/v1/users")
    if status != 200:
        print(f"UI_TEST_CLEANUP=unable to list users; HTTP {status}")
        return False
    users = payload if isinstance(payload, list) else payload.get("users", [])
    matches = [user for user in users if user.get("name") == name]
    for user in matches:
        status, _ = request(
            base,
            token,
            f"/api/v1/users/{user['id']}?expected_revision={user['revision']}",
            method="DELETE",
        )
        if status != 204:
            print(f"UI_TEST_CLEANUP=failed; HTTP {status}")
            return False
    print("UI_TEST_CLEANUP=" + ("deleted" if matches else "no leftover user"))
    return True


def swift_escape(value):
    return value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def main():
    if not shutil.which("xcodegen"):
        raise SystemExit("xcodegen is required for the temporary UI test project")
    values = read_env()
    base = "https://" + values["HYSTERIAX_DOMAIN"]
    token = values["HYSTERIAX_ADMIN_TOKEN"]
    status, _ = request(base, token, "/readyz")
    if status != 200:
        raise SystemExit(f"Management service readiness failed: HTTP {status}")

    status, nodes = request(base, token, "/api/v1/nodes")
    if status != 200:
        raise SystemExit(f"Could not read nodes: HTTP {status}")
    if isinstance(nodes, dict):
        nodes = nodes.get("nodes", nodes.get("items", []))
    deployed = sorted(
        (node for node in nodes if node.get("state") == "deployed"),
        key=lambda node: node.get("name", ""),
    )
    if len(deployed) < 2:
        raise SystemExit("At least two deployed nodes are required for the live UI workflow")
    node_one, node_two = deployed[:2]

    user_name = f"HX UI Workflow {int(time.time())}"
    bundle = f"com.hysteriax.ui.harness.run{int(time.time())}"
    with tempfile.TemporaryDirectory(prefix="hysteriax-live-ui-") as temporary:
        temp = pathlib.Path(temporary)
        shutil.copytree(APP_SOURCES, temp / "AppSources")
        tests = temp / "UITests"
        tests.mkdir()
        credentials = temp / "credentials.json"
        credentials.write_text(
            json.dumps(
                {
                    "serviceAddress": base,
                    "adminToken": token,
                    "userName": user_name,
                    "nodeOneID": node_one["id"],
                    "nodeOneName": node_one["name"],
                    "nodeTwoID": node_two["id"],
                    "nodeTwoName": node_two["name"],
                }
            )
        )
        credentials.chmod(0o600)
        source = UI_TEST_SOURCE.read_text()
        source = source.replace(
            "__HYSTERIAX_UI_CREDENTIALS_PATH__", swift_escape(str(credentials))
        ).replace("__HYSTERIAX_UI_TEST_USER_NAME__", swift_escape(user_name))
        (tests / UI_TEST_SOURCE.name).write_text(source)

        spec = """name: HysteriaXUITestHarness
options:
  deploymentTarget:
    macOS: "26.0"
  createIntermediateGroups: true
settings:
  base:
    SWIFT_VERSION: "6.0"
    MACOSX_DEPLOYMENT_TARGET: "26.0"
targets:
  HysteriaX:
    type: application
    platform: macOS
    sources:
      - path: AppSources
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: __BUNDLE__
        GENERATE_INFOPLIST_FILE: YES
        INFOPLIST_KEY_CFBundleDisplayName: HysteriaX
        INFOPLIST_KEY_LSMinimumSystemVersion: "26.0"
        SWIFT_STRICT_CONCURRENCY: complete
        SWIFT_ACTIVE_COMPILATION_CONDITIONS: "$(inherited) HYSTERIAX_UI_TESTING"
  HysteriaXUITests:
    type: bundle.ui-testing
    platform: macOS
    sources:
      - path: UITests
    dependencies:
      - target: HysteriaX
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: __BUNDLE__.uitests
        GENERATE_INFOPLIST_FILE: YES
schemes:
  HysteriaX:
    build:
      targets:
        HysteriaX: all
        HysteriaXUITests: [test]
    test:
      targets:
        - HysteriaXUITests
"""
        (temp / "project.yml").write_text(spec.replace("__BUNDLE__", bundle))
        generated = subprocess.run(
            ["xcodegen", "generate", "--spec", "project.yml"],
            cwd=temp,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        if generated.returncode:
            print(generated.stdout)
            raise SystemExit(generated.returncode)

        log_path = temp / "xcodebuild.log"
        command = [
            "xcodebuild",
            "test",
            "-project",
            str(temp / "HysteriaXUITestHarness.xcodeproj"),
            "-scheme",
            "HysteriaX",
            "-destination",
            "platform=macOS,arch=arm64",
            "-derivedDataPath",
            str(temp / "DerivedData"),
            "-resultBundlePath",
            str(temp / "TestResults.xcresult"),
            "CODE_SIGNING_ALLOWED=YES",
            "CODE_SIGNING_IDENTITY=-",
        ]
        with log_path.open("w") as log:
            result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        lines = log_path.read_text(errors="replace").splitlines()
        markers = (
            "LIVE_UI_WORKFLOW=",
            "SYSTEM_SETTINGS_",
            "FRONTMOST_",
            "Test Case",
            "Executed ",
            "TEST SUCCEEDED",
            "TEST FAILED",
            "error:",
            "failed -",
            "XCTAssert",
        )
        for line in lines:
            if any(marker in line for marker in markers):
                print(line.replace(token, "[redacted]"))

        cleanup_ok = cleanup_test_user(base, token, user_name)
        if result.returncode:
            tail = "\n".join(lines[-55:]).replace(token, "[redacted]")
            print("XCODEBUILD_TAIL_BEGIN\n" + tail + "\nXCODEBUILD_TAIL_END")
            raise SystemExit(result.returncode)
        if not cleanup_ok:
            raise SystemExit(1)


if __name__ == "__main__":
    main()
