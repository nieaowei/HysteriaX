#!/usr/bin/env python3
"""Run isolated overview UI tests without network or Keychain access."""
import argparse
from contextlib import nullcontext
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
if not shutil.which("xcodegen"):
    raise SystemExit("xcodegen is required")
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--manual", action="store_true", help="build and launch an isolated fixture app for native UI verification")
args = parser.parse_args()
if args.manual:
    manual_path = ROOT / f"apps/macos/.build/overview-manual-{int(time.time())}"
    manual_path.mkdir(parents=True)
    context = nullcontext(str(manual_path))
else:
    context = tempfile.TemporaryDirectory(prefix="hysteriax-overview-ui-")
with context as folder:
    temp = Path(folder)
    shutil.copytree(ROOT / "apps/macos/Sources/HysteriaX", temp / "AppSources")
    fixtures = temp / "fixtures"
    subprocess.run(["python3", str(ROOT / "scripts/overview-fixtures.py"), str(fixtures)], check=True)
    store_path = temp / "AppSources/Stores/ManagementStore.swift"
    source = store_path.read_text().replace('    init(client: APIClient? = nil, restoreSnapshot: Bool = true, nodeNotifications: NodeAlertNotifications? = nil) {', '''    init(client: APIClient? = nil, restoreSnapshot: Bool = true, nodeNotifications: NodeAlertNotifications? = nil) {
        if let folder = ProcessInfo.processInfo.environment["HYSTERIAX_OVERVIEW_FIXTURE_DIRECTORY"] {
            self.nodeNotifications = nodeNotifications
            serviceAddress = "https://overview-fixture.invalid"
            nodes = try! JSONDecoder().decode([NodeSummary].self, from: Data(contentsOf: URL(fileURLWithPath: folder + "/nodes.json")))
            overview = try! JSONDecoder().decode(OverviewResponse.self, from: Data(contentsOf: URL(fileURLWithPath: folder + "/overview.json")))
            serverMonitoring = try! JSONDecoder().decode(ServerMonitoring.self, from: Data(contentsOf: URL(fileURLWithPath: folder + "/server-monitoring.json")))
            users = try! JSONDecoder().decode([UserSummary].self, from: Data(contentsOf: URL(fileURLWithPath: folder + "/users.json")))
            jobs = try! JSONDecoder().decode([JobSummary].self, from: Data(contentsOf: URL(fileURLWithPath: folder + "/jobs.json")))
            for file in ["history-24h.json", "history.json", "history-30d.json"] {
                let history = try! JSONDecoder().decode(OverviewHistory.self, from: Data(contentsOf: URL(fileURLWithPath: folder + "/" + file)))
                cacheOverviewHistory(history, nodeID: nil)
                cacheOverviewHistory(history, nodeID: "node-1")
            }
            supportsOverviewMonitoring = true
            supportsJobRetryLinks = true
            lastUpdated = Date()
            return
        }''')
    store_path.write_text(source)
    tests = temp / "UITests"
    tests.mkdir()
    source = (ROOT / "apps/macos/UITests/OverviewFixtureUITests.swift").read_text().replace("__OVERVIEW_FIXTURES__", str(fixtures))
    (tests / "OverviewFixtureUITests.swift").write_text(source)
    bundle = f"com.hysteriax.overview.fixture{int(time.time())}"
    (temp / "project.yml").write_text(f'''name: OverviewUITestHarness
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
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: {bundle}
        GENERATE_INFOPLIST_FILE: YES
        SWIFT_ACTIVE_COMPILATION_CONDITIONS: "$(inherited) HYSTERIAX_UI_TESTING"
  OverviewUITests:
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
        OverviewUITests: [test]
    test:
      targets: [OverviewUITests]
''')
    subprocess.run(["xcodegen", "generate", "--spec", "project.yml"], cwd=temp, check=True, stdout=subprocess.DEVNULL)
    if args.manual:
        with (temp / "build.log").open("w") as stream:
            subprocess.run(["xcodebuild", "build", "-project", str(temp / "OverviewUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-derivedDataPath", str(temp / "DerivedData"), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGNING_IDENTITY=-"], stdout=stream, stderr=subprocess.STDOUT, check=True)
        app = temp / "DerivedData/Build/Products/Debug/HysteriaX.app"
        subprocess.run(["open", "-n", str(app), "--env", f"HYSTERIAX_OVERVIEW_FIXTURE_DIRECTORY={fixtures}"], check=True)
        print(f"Fixture app: {app}")
        raise SystemExit(0)
    result_path = ROOT / "apps/macos/.build/overview-ui.xcresult"
    if result_path.exists():
        result_path = result_path.with_name(f"overview-ui-{int(time.time())}.xcresult")
    log = ROOT / "apps/macos/.build/overview-ui.log"
    with log.open("w") as stream:
        result = subprocess.run(["xcodebuild", "test", "-project", str(temp / "OverviewUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-derivedDataPath", str(temp / "DerivedData"), "-resultBundlePath", str(result_path), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGNING_IDENTITY=-"], stdout=stream, stderr=subprocess.STDOUT)
    for line in log.read_text(errors="replace").splitlines():
        if any(marker in line for marker in ["Test Case", "TEST SUCCEEDED", "TEST FAILED", "error:", "failed -", "XCTAssert"]):
            print(line)
    print(f"UI result bundle: {result_path}")
    raise SystemExit(result.returncode)
