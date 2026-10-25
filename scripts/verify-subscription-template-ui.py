#!/usr/bin/env python3
"""Run template workbench regressions against an isolated service and PostgreSQL schema.

Requires TEST_DATABASE_URL, Xcode, and scripts/requirements-test.txt.
xcodegen is required for UI and manual modes; --client runs without screen capture.
App sources and preferences are isolated in a temporary, uniquely identified test application.
"""
from management_test_http import management_request, start_test_server
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


def request(base, token, path, payload=None, method=None):
    data = None if payload is None else json.dumps(payload).encode()
    call = management_request(base + path, data=data, headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"}, method=method)
    with urllib.request.urlopen(call, timeout=20) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument("--test", action="append", help="Run a named SubscriptionTemplateUITests method; repeat to select multiple")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--manual", action="store_true", help="Build and open the same isolated app for manual verification; press Enter to clean up")
    mode.add_argument("--client", action="store_true", help="Run the production editor model against the isolated real service without screen capture")
    args = parser.parse_args()
    if not args.client and not shutil.which("xcodegen"):
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
            server = start_test_server([str(ROOT / "target/debug/hysteriax-server")], env=environment, stdout=server_log, stderr=subprocess.STDOUT)
            try:
                for _ in range(150):
                    if server.poll() is not None: raise RuntimeError("isolated UI service exited before readiness")
                    try:
                        with urllib.request.urlopen(base + "/readyz", timeout=1) as response:
                            if response.status == 200: break
                    except (OSError, urllib.error.URLError): time.sleep(0.2)
                else: raise TimeoutError("isolated UI service did not become ready")
                layout_draft = request(base, token, "/api/v2/subscription-templates/default-singbox/draft")
                request(base, token, "/api/v2/subscription-templates/default-singbox", {"expected_revision": layout_draft["template"]["revision"], "name": "0008 Layout - " + "Long template name " * 5}, method="PATCH")
                user = request(base, token, "/api/v2/users", {"name": "0005 UI User"})
                subscription = request(base, token, f"/api/v2/users/{user['id']}/subscription", {"expected_revision": 1})
                document = {"schema_version": 2, "common": {"tproxy-port": 7893, "mixed-port": 7888, "log-level": "silent", "proxies": [], "proxy-groups": [{"name": "Select", "type": "select", "proxies": []}], "rules": ["MATCH,Select"]}, "platforms": {}, "injections": [{"group": "Select"}]}
                template = request(base, token, "/api/v2/subscription-templates", {"expected_revision": 0, "name": "0005 UI Template", "format": "mihomo", "document": document})
                quit_first = request(base, token, "/api/v2/subscription-templates", {"expected_revision": 0, "name": "0008 Quit A", "format": "mihomo", "document": document})
                quit_second = request(base, token, "/api/v2/subscription-templates", {"expected_revision": 0, "name": "0008 Quit B", "format": "mihomo", "document": document})
                exports = work / "Exports"; exports.mkdir()
                export_document = json.loads(json.dumps(document)); export_document["common"]["future-field"] = {"counter": 18446744073709551615}
                export_template = request(base, token, "/api/v2/subscription-templates", {"expected_revision": 0, "name": "0005 UI Export Template", "format": "mihomo", "document": export_document})
                export_template = request(base, token, f"/api/v2/subscription-templates/{export_template['id']}/publish", {"expected_revision": export_template["revision"]})
                if args.client:
                    fixture = work / "ClientFixture.json"
                    fixture.write_text(json.dumps({"base": base, "token": token, "template_id": template["id"], "user_id": user["id"], "subscription_token": subscription["token"]}))
                    source = ROOT / "apps/macos/Sources/HysteriaX"
                    files = ["Support/Localization.swift", "Models/APIModels.swift", "Models/OpenAPIRequests.generated.swift", "Models/OpenAPIResponses.generated.swift", "Models/SubscriptionTemplateDocument.swift", "Models/MihomoTemplateRule.swift", "Models/TemplateReferenceIndex.swift", "Stores/SubscriptionTemplateStore.swift", "Services/APIClient.swift"]
                    binary = work / "workbench-client-tests"
                    subprocess.run(["swiftc", "-swift-version", "6", "-parse-as-library", *[str(source / file) for file in files], str(ROOT / "tests/subscription-template-workbench-live.swift"), "-o", str(binary)], check=True)
                    return subprocess.run([str(binary), str(fixture)]).returncode
                sources = work / "AppSources"; shutil.copytree(ROOT / "apps/macos/Sources/HysteriaX", sources)
                replacements = {"__QUIT_TEMPLATE_ID__": quit_first["id"], "__QUIT_SECOND_TEMPLATE_ID__": quit_second["id"], "__EXPORT_DIR__": str(exports), "__EXPORT_TEMPLATE_ID__": export_template["id"], "__BASE_URL__": base, "__ADMIN_TOKEN__": token, "__TEMPLATE_ID__": template["id"], "__USER_ID__": user["id"], "__SUBSCRIPTION_TOKEN__": subscription["token"]}
                main_source = '''import SwiftUI
@main struct TemplateUITestApp: App {
    @NSApplicationDelegateAdaptor(TemplateDraftTerminationGuard.self) private var terminationGuard
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
        .onAppear { terminationGuard.store = store; DispatchQueue.main.async { NSApp.windows.first { $0.canBecomeMain }?.center() } } }.defaultPosition(.center).commands { TemplateEditorCommands() } }
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
                selection = [] if not args.test else ["-only-testing:TemplateUITests/SubscriptionTemplateUITests/" + name for name in args.test]
                if args.manual:
                    with log.open("w") as output:
                        subprocess.run(["xcodebuild", "build", "-project", str(work / "TemplateUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-derivedDataPath", str(work / "DerivedData"), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_IDENTITY=-"], stdout=output, stderr=subprocess.STDOUT, check=True)
                    app = work / "DerivedData/Build/Products/Debug/HysteriaX.app"
                    fixture = work / "ManualFixture.json"
                    fixture.write_text(json.dumps({"base": base, "token": token, "template_id": template["id"], "export_template_id": export_template["id"], "user_id": user["id"], "subscription_token": subscription["token"]}))
                    print("Manual app:", app, flush=True)
                    print("Manual fixture:", fixture, flush=True)
                    subprocess.run(["open", "-n", str(app)], check=True)
                    try:
                        input("Press Enter after verification to close the isolated app and clean up: ")
                    finally:
                        subprocess.run(["pkill", "-f", str(app / "Contents/MacOS/HysteriaX")], check=False)
                    return 0
                with log.open("w") as output:
                    run = subprocess.run(["xcodebuild", "test", "-project", str(work / "TemplateUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-parallel-testing-enabled", "NO", "-derivedDataPath", str(work / "DerivedData"), "-resultBundlePath", str(results), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_IDENTITY=-", "-test-timeouts-enabled", "YES", "-default-test-execution-time-allowance", "600", "-maximum-test-execution-time-allowance", "600", *selection], stdout=output, stderr=subprocess.STDOUT, timeout=1000)
                print("Template UI results:", results); print("Template UI log:", log)
                return run.returncode
            finally:
                server.terminate()
                try: server.wait(timeout=10)
                except subprocess.TimeoutExpired: server.kill(); server.wait()


if __name__ == "__main__":
    raise SystemExit(main())
