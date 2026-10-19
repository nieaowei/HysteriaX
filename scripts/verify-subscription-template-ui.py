#!/usr/bin/env python3
"""Run template UI regression against an isolated local service and PostgreSQL schema.

Requires TEST_DATABASE_URL, Xcode, xcodegen, and scripts/requirements-test.txt.
App sources and preferences are isolated in a temporary, uniquely identified test application.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parents[1]


def request(base, token, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    call = urllib.request.Request(base + path, data=data, headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
    with urllib.request.urlopen(call, timeout=20) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument("--test", help="Run one named SubscriptionTemplateUITests method")
    args = parser.parse_args()
    if not shutil.which("xcodegen"):
        raise SystemExit("xcodegen is required for the isolated UI test project")
    subprocess.run(["cargo", "build", "-q"], cwd=ROOT, check=True)
    with PostgresTestSchema() as database, tempfile.TemporaryDirectory(prefix="hysteriax-template-ui-") as folder:
        work = Path(folder)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0)); port = listener.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        token = "hx_" + secrets.token_urlsafe(32)
        environment = os.environ | {"DATABASE_URL": database.url, "HYSTERIAX_LISTEN_ADDR": f"127.0.0.1:{port}", "HYSTERIAX_PUBLIC_URL": base, "HYSTERIAX_ADMIN_TOKEN": token, "HYSTERIAX_MASTER_KEY": base64.b64encode(secrets.token_bytes(32)).decode().rstrip("="), "RUST_LOG": "warn"}
        environment.pop("HYSTERIAX_REDIS_URL", None); environment.pop("REDIS_URL", None)
        results = ROOT / "apps/macos/.build" / f"subscription-template-ui-{int(time.time())}.xcresult"
        results.parent.mkdir(parents=True, exist_ok=True)
        with results.with_suffix(".server.log").open("w") as server_log:
            server = subprocess.Popen([str(ROOT / "target/debug/hysteriax-server")], env=environment, stdout=server_log, stderr=subprocess.STDOUT)
            try:
                for _ in range(150):
                    if server.poll() is not None: raise RuntimeError("isolated UI service exited before readiness")
                    try:
                        with urllib.request.urlopen(base + "/readyz", timeout=1) as response:
                            if response.status == 200: break
                    except (OSError, urllib.error.URLError): time.sleep(0.2)
                else: raise TimeoutError("isolated UI service did not become ready")
                user = request(base, token, "/api/v1/users", {"name": "0005 UI User"})
                subscription = request(base, token, f"/api/v1/users/{user['id']}/subscription", {"expected_revision": 1})
                document = {"schema_version": 1, "common": {"mixed-port": 7888, "log-level": "silent", "proxies": [], "proxy-groups": [{"name": "Select", "type": "select", "proxies": []}], "rules": ["MATCH,Select"]}, "platforms": {"linux": {"set": {"tproxy-port": 7893}, "remove": []}}, "injections": [{"group": "Select"}]}
                template = request(base, token, "/api/v1/subscription-templates", {"expected_revision": 0, "name": "0005 UI Template", "format": "mihomo", "document": document})
                exports = work / "Exports"; exports.mkdir()
                export_document = json.loads(json.dumps(document)); export_document["common"]["future-field"] = {"counter": 18446744073709551615}
                export_template = request(base, token, "/api/v1/subscription-templates", {"expected_revision": 0, "name": "0005 UI Export Template", "format": "mihomo", "document": export_document})
                export_template = request(base, token, f"/api/v1/subscription-templates/{export_template['id']}/publish", {"expected_revision": export_template["revision"]})
                sources = work / "AppSources"; shutil.copytree(ROOT / "apps/macos/Sources/HysteriaX", sources)
                replacements = {"__EXPORT_DIR__": str(exports), "__EXPORT_TEMPLATE_ID__": export_template["id"], "__BASE_URL__": base, "__ADMIN_TOKEN__": token, "__TEMPLATE_ID__": template["id"], "__USER_ID__": user["id"], "__SUBSCRIPTION_TOKEN__": subscription["token"]}
                main_source = '''import SwiftUI
@main struct TemplateUITestApp: App {
    @State private var store: ManagementStore = {
        AppLanguage.shared.selection = "zh-Hans"
        let api = APIClient(baseURL: URL(string: "__BASE_URL__")!, token: "__ADMIN_TOKEN__")
        let store = ManagementStore(client: api, restoreSnapshot: false)
        store.serviceAddress = "__BASE_URL__"; store.isConnected = true
        return store
    }()
    private var layoutWidth: CGFloat {
        let args = ProcessInfo.processInfo.arguments
        if let index = args.firstIndex(of: "-layoutWidth"), args.indices.contains(index + 1), let width = Double(args[index + 1]) { return width }
        return 1350
    }
    var body: some Scene { WindowGroup("Subscription Template UI Test") { ContentView(store: store).frame(width: layoutWidth, height: 780).environment(\\.locale, L10n.locale)
        .onAppear { DispatchQueue.main.async { NSApp.windows.first { $0.canBecomeMain }?.center() } } }.defaultPosition(.center) }
}
'''
                test_source = (ROOT / "apps/macos/UITests/SubscriptionTemplateUITests.swift").read_text()
                for key, value in replacements.items(): main_source = main_source.replace(key, value); test_source = test_source.replace(key, value)
                (sources / "App/HysteriaXApp.swift").write_text(main_source)
                (work / "UITests").mkdir(); (work / "UITests/SubscriptionTemplateUITests.swift").write_text(test_source)
                bundle = "com.hysteriax.template.fixture" + uuid.uuid4().hex
                (work / "project.yml").write_text(f'''name: TemplateUITestHarness
options:
  deploymentTarget:
    macOS: "26.0"
settings:
  base:
    SWIFT_VERSION: "6.0"
targets:
  HysteriaX:
    type: application
    platform: macOS
    sources: [AppSources]
    info:
      path: Info.plist
      properties:
        NSAppTransportSecurity:
          NSAllowsArbitraryLoads: true
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: {bundle}
        SWIFT_ACTIVE_COMPILATION_CONDITIONS: "$(inherited) HYSTERIAX_UI_TESTING"
  TemplateUITests:
    type: bundle.ui-testing
    platform: macOS
    sources: [UITests]
    dependencies:
      - target: HysteriaX
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: {bundle}.tests
        GENERATE_INFOPLIST_FILE: YES
schemes:
  HysteriaX:
    build:
      targets:
        HysteriaX: all
        TemplateUITests: [test]
    test:
      targets: [TemplateUITests]
''')
                subprocess.run(["xcodegen", "generate", "--spec", "project.yml"], cwd=work, check=True, stdout=subprocess.DEVNULL)
                log = results.with_suffix(".log")
                selection = [] if not args.test else ["-only-testing:TemplateUITests/SubscriptionTemplateUITests/" + args.test]
                with log.open("w") as output:
                    run = subprocess.run(["xcodebuild", "test", "-project", str(work / "TemplateUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-parallel-testing-enabled", "NO", "-derivedDataPath", str(work / "DerivedData"), "-resultBundlePath", str(results), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_IDENTITY=-", "-test-timeouts-enabled", "YES", "-default-test-execution-time-allowance", "300", "-maximum-test-execution-time-allowance", "300", *selection], stdout=output, stderr=subprocess.STDOUT, timeout=420)
                print("Template UI results:", results); print("Template UI log:", log)
                return run.returncode
            finally:
                server.terminate()
                try: server.wait(timeout=10)
                except subprocess.TimeoutExpired: server.kill(); server.wait()


if __name__ == "__main__":
    raise SystemExit(main())
