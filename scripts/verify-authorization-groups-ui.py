#!/usr/bin/env python3
"""Run authorization group UI checks using isolated fixture transport."""
from pathlib import Path
import json
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
if not shutil.which("xcodegen"):
    raise SystemExit("xcodegen is required")
with tempfile.TemporaryDirectory(prefix="hx-group-ui-") as folder:
    temp = Path(folder)
    shutil.copytree(ROOT / "apps/macos/Sources/HysteriaX", temp / "AppSources")
    fixtures = temp / "fixtures"
    fixtures.mkdir()
    stamp = "2026-10-07T00:00:00Z"
    group = dict(id="group-1", name="常用线路", revision=1, user_ids=["user-1"], node_ids=["node-1"], user_count=1, node_count=1, created_at=stamp, updated_at=stamp)
    user = dict(id="user-1", name="Alice", enabled=True, usage_bytes=0, revision=1, created_at=stamp, updated_at=stamp,
                authorization_groups=[dict(id="group-1", name="常用线路", revision=1)],
                assignments=[dict(node_id="node-1", created_at=stamp, source_groups=[dict(id="group-1", name="常用线路")])])
    nodes = [dict(id="node-1", name="香港", revision=1, state="new", mtls_required=False),
             dict(id="node-2", name="日本 mTLS", revision=1, state="new", mtls_required=True)]
    responses = dict(version=dict(api_version="1.0.0", features=["authorization_groups"], current_admin_token_id="fixture", service_version="fixture", hysteria_version="app/v2.12.3", mihomo_version="v1.19.31"),
                     groups=[group], group=group, users=[user], user=user, nodes=nodes, empty=[])
    for name, value in responses.items():
        (fixtures / (name + ".json")).write_text(json.dumps(value))
    support = '''import Foundation
final class AuthorizationFixtureProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let paths = ["/api/v1/version":"version", "/api/v1/authorization-groups":"groups", "/api/v1/authorization-groups/group-1":"group", "/api/v1/users":"users", "/api/v1/users/user-1":"user", "/api/v1/nodes":"nodes", "/api/v1/credentials":"empty", "/api/v1/jobs":"empty", "/api/v1/audit":"empty", "/api/v1/admin/tokens":"empty"]
        let path = request.url!.path
        let folder = ProcessInfo.processInfo.environment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"]!
        var data = paths[path].map { try! Data(contentsOf: URL(fileURLWithPath: folder + "/" + $0 + ".json")) } ?? Data(#"{"code":"fixture","message":"not available"}"#.utf8)
        var status = paths[path] == nil ? 404 : 200
        if path == "/api/v1/authorization-groups" && request.httpMethod == "POST" {
            let groupData = try! Data(contentsOf: URL(fileURLWithPath: folder + "/group.json"))
            var group = try! JSONSerialization.jsonObject(with: groupData) as! [String:Any]
            group["id"] = "group-created"
            data = try! JSONSerialization.data(withJSONObject: ["group":group, "additions_count":1, "removals_count":0, "created_credentials":[], "revocation_job_ids":[]])
            status = 201
        }
        if path.hasSuffix("/preview") {
            let stream = request.httpBodyStream
            var body = request.httpBody ?? Data()
            if let stream { stream.open(); defer { stream.close() }; var buffer = [UInt8](repeating: 0, count: 4096); while stream.hasBytesAvailable { let n = stream.read(&buffer, maxLength: buffer.count); if n <= 0 { break }; body.append(contentsOf: buffer.prefix(n)) } }
            let input = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] ?? [:]
            let users = input["user_ids"] as? [String] ?? ["user-1"]
            let nodes = input["node_ids"] as? [String] ?? ["node-1"]
            let pairs = users.flatMap { user in nodes.map { ["user_id":user,"node_id":$0] } }
            let missing = pairs.filter { $0["node_id"] == "node-2" }
            data = try! JSONSerialization.data(withJSONObject: ["action":input["action"] ?? "update_memberships", "preview_token":"fixture-preview", "additions_count":pairs.count, "removals_count":0, "additions":pairs, "removals":[], "missing_mtls":missing])
            status = 200
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type":"application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
'''
    (temp / "AppSources/Support/AuthorizationFixtureProtocol.swift").write_text(support)
    store = temp / "AppSources/Stores/ManagementStore.swift"
    marker = "    init(client: APIClient? = nil, restoreSnapshot: Bool = true, nodeNotifications: NodeAlertNotifications? = nil) {"
    source = store.read_text()
    assert marker in source
    replacement = marker + '''
        if ProcessInfo.processInfo.environment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] != nil {
            self.nodeNotifications = nodeNotifications
            serviceAddress = "https://authorization-fixture.invalid"
            let config = URLSessionConfiguration.ephemeral
            config.protocolClasses = [AuthorizationFixtureProtocol.self]
            api = APIClient(baseURL: URL(string: serviceAddress)!, token: "fixture", session: URLSession(configuration: config))
            supportsAuthorizationGroups = true
            isConnected = true
            return
        }
'''
    store.write_text(source.replace(marker, replacement, 1))
    tests = temp / "UITests"
    tests.mkdir()
    (tests / "AuthorizationFixtureUITests.swift").write_text((ROOT / "apps/macos/UITests/AuthorizationFixtureUITests.swift").read_text().replace("__AUTHORIZATION_FIXTURES__", str(fixtures)))
    bundle = f"com.hysteriax.authorization.fixture{int(time.time())}"
    (temp / "project.yml").write_text(f'''name: AuthorizationUITestHarness
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
  AuthorizationUITests:
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
        AuthorizationUITests: [test]
    test:
      targets: [AuthorizationUITests]
''')
    subprocess.run(["xcodegen", "generate", "--spec", "project.yml"], cwd=temp, check=True, stdout=subprocess.DEVNULL)
    results = ROOT / "apps/macos/.build" / f"authorization-ui-{int(time.time())}.xcresult"
    results.parent.mkdir(parents=True, exist_ok=True)
    log = results.with_suffix(".log")
    with log.open("w") as output:
        result = subprocess.run(["xcodebuild", "test", "-project", str(temp / "AuthorizationUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-derivedDataPath", str(temp / "DerivedData"), "-resultBundlePath", str(results), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGNING_IDENTITY=-"], stdout=output, stderr=subprocess.STDOUT)
    print("Authorization UI results:", results)
    print("Authorization UI log:", log)
    raise SystemExit(result.returncode)
