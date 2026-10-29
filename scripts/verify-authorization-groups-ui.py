#!/usr/bin/env python3
"""Run authorization group UI checks using isolated fixture transport."""
from pathlib import Path
import argparse
import json
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument("--only-test", action="append", help="Run one AuthorizationFixtureUITests method")
args = parser.parse_args()

ROOT = Path(__file__).resolve().parent.parent
if not shutil.which("xcodegen"):
    raise SystemExit("xcodegen is required")
with tempfile.TemporaryDirectory(prefix="hx-group-ui-") as folder:
    temp = Path(folder)
    shutil.copytree(ROOT / "apps/macos/Sources/HysteriaX", temp / "AppSources")
    fixtures = temp / "fixtures"
    fixtures.mkdir()
    stamp = "2026-10-07T00:00:00Z"
    def fixture_user_id(index):
        return f"user-{index}" if index in (1, 101, 202, 203) else f"{index:08x}-1234-5678-9abc-000000000000"
    group = dict(id="group-1", name="常用线路", revision=1, user_ids=[fixture_user_id(index) for index in range(1, 202)], node_ids=["node-1"], user_count=201, node_count=1, created_at=stamp, updated_at=stamp)
    user = dict(id="user-1", name="Alice", enabled=True, usage_bytes=0, revision=1, created_at=stamp, updated_at=stamp,
                authorization_groups=[dict(id="group-1", name="常用线路", revision=1)],
                assignments=[dict(node_id="node-1", created_at=stamp, source_groups=[dict(id="group-1", name="常用线路")])])
    users = [user] + [dict(user, id=fixture_user_id(index), name=f"Member {index:04}", enabled=index % 7 != 0,
                           authorization_groups=user["authorization_groups"] if index <= 201 else [],
                           assignments=user["assignments"] if index <= 201 else []) for index in range(2, 302)]
    nodes = [dict(id="node-1", name="香港", revision=1, state="new", mtls_required=True),
             dict(id="node-2", name="日本 mTLS", revision=1, state="new", mtls_required=True)]
    user["managed_certificates"] = [dict(identity_id="identity-1", ca_credential_id="ca-1", ca_name="Shared CA", revision=1, authorization_revision=1, credential_id="client-1", archived=False, published_ca_version=1, ca_expires_at="2030-10-07T00:00:00Z", policy_revision=1, versions=[dict(version=1, ca_version=1, not_before=stamp, expires_at="2027-04-07T00:00:00Z", released=True, discarded=False)], node_ids=["node-1"])]
    responses = dict(version=dict(api_version="2.0.0", management_protocol=2, directory_version="fixture-directory-v2", features=["authorization_groups", "managed_mtls"], current_admin_token_id="fixture", service_version="fixture", hysteria_version="app/v2.12.3", mihomo_version="v1.19.31"),
                     groups=[group], group=group, users=users, user=user, nodes=nodes, empty=[])
    ca = dict(id="ca-1", name="Shared CA", kind="ca_identity", revision=3, latest_version=3, archived=False, status="active", metadata={"expires_at":"2036-10-07T00:00:00Z"}, expires_at="2036-10-07T00:00:00Z", created_at=stamp, updated_at=stamp)
    probe = dict(ca,id="probe-1",name="Internal probe",kind="tls_identity",metadata={"managed":True,"internal_probe":True})
    tls = dict(ca,id="tls-1",name="Server TLS",kind="tls_identity",metadata={})
    rotation = dict(id="rotation-3",version=3,phase="trust",next_action_at=stamp,error_message="Waiting for offline node",nodes=[dict(node_id="node-1",name="香港",trust_prepared=False,probe_switched=False,job_id="trust-job",error_message="offline")],status=dict(previous_version=2,old_ca_expires_at="2030-10-07T00:00:00Z",deadline="2030-10-07T00:00:00Z",days_remaining=100,blocked_node_ids=["node-1"],eligible_user_count=2,prepared_user_count=1))
    cleanup = dict(version=1,next_action_at="2027-04-07T00:05:00Z",nodes=[dict(node_id="node-1",name="香港",job_id="cleanup-job",completed=False,status="failed",stage="ssh",error_message="offline")])
    ca_detail = dict(ca, ca=dict(source="generated", policy_revision=1, published_version=2, client_validity_days=180, renew_before_hours=720,cleanup=[cleanup]), versions=[dict(version=i, metadata=ca["metadata"], created_at=stamp) for i in (1,2,3)], references=[], batches=[dict(id="batch-3",credential_id="ca-1",version=3,created_at=stamp,items=[],ca_rotation=rotation)])
    mtls_settings = dict(mode="managed",ca_credential_id="ca-1",ca_credential_version=2)
    node_detail = dict(id="node-1",name="香港",revision=1,deployed_revision=1,state="deployed",mtls_state="recovery_required",mtls_error="Remote activation outcome is unconfirmed",mtls_generation=2,mtls=dict(target=mtls_settings,deployed=mtls_settings,trust=[dict(ca_credential_id="ca-1",ca_credential_version=2,desired=True,deployed=True)]),config=dict(tls=dict(cert="credential://tls-1/1/certificate",key="credential://tls-1/1/private_key",sniGuard="disable"),mtls=mtls_settings),yaml_preview="listen: :443",ssh=dict(host="127.0.0.1",port=22,username="root",auth_type="password",credential_id="ssh-1",credential_version=1),connection=dict(host="127.0.0.1",port=443,listen_addr=":443",skip_cert_verify=True),created_at=stamp,updated_at=stamp)
    node_detail["public"] = node_detail.pop("connection")
    nodes[0].update(state="deployed",deployed_revision=1,mtls_state="recovery_required",mtls_error=node_detail["mtls_error"],mtls_generation=2)
    responses.update(credentials=[ca,probe,tls], ca=ca_detail,node_detail=node_detail,ca_receipt=dict(id="ca-1",revision=1,version=1,published=True), policy_receipt=dict(id="ca-1",policy_revision=2,policy=dict(client_validity_days=90,renew_before_hours=168)))
    responses.update(ready={"status":"ready","database":"ok","management_protocol":2},
                     event_head={"cursor":"v2:00000000-0000-0000-0000-000000000001:0",
                                 "epoch":"00000000-0000-0000-0000-000000000001","publication_seq":0})
    for name, value in responses.items():
        (fixtures / (name + ".json")).write_text(json.dumps(value))
    support = '''import Foundation
private final class AuthorizationFixtureState: @unchecked Sendable {
    private let lock = NSLock()
    private var committed = false
    func markCommitted() { lock.lock(); committed = true; lock.unlock() }
    var hasCommitted: Bool { lock.lock(); defer { lock.unlock() }; return committed }
}
final class AuthorizationFixtureProtocol: URLProtocol, @unchecked Sendable {
    private static let state = AuthorizationFixtureState()
    private let deliveryLock = NSLock()
    private var stopped = false
    private var canDeliver: Bool { deliveryLock.lock(); defer { deliveryLock.unlock() }; return !stopped }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let paths = ["/readyz":"ready", "/api/v2/events/head":"event_head", "/api/v2/events":"empty", "/api/v2/version":"version", "/api/v2/authorization-groups":"groups", "/api/v2/authorization-groups/group-1":"group", "/api/v2/users":"users", "/api/v2/users/user-1":"user", "/api/v2/nodes":"nodes", "/api/v2/nodes/node-1":"node_detail", "/api/v2/nodes/node-1/resources":"empty", "/api/v2/credentials":"credentials", "/api/v2/credentials/ca-1":"ca", "/api/v2/credentials/ca-identities":"ca_receipt", "/api/v2/credentials/ca-1/ca-policy":"policy_receipt", "/api/v2/credentials/ca-1/ca-versions":"ca_receipt", "/api/v2/jobs":"empty", "/api/v2/audit":"empty", "/api/v2/admin/tokens":"empty"]
        let path = request.url!.path
        if path.contains("authorization-groups"), request.httpMethod != "GET" { Self.state.markCommitted() }
        let folder = ProcessInfo.processInfo.environment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"]!
        var data = paths[path].map { try! Data(contentsOf: URL(fileURLWithPath: folder + "/" + $0 + ".json")) } ?? Data(#"{"code":"fixture","message":"not available"}"#.utf8)
        if ["/api/v2/jobs", "/api/v2/audit"].contains(path) {
            data = Data(#"{"items":[],"total":0,"page":1,"page_size":200}"#.utf8)
        }
        if path == "/api/v2/users" {
            let users = try! JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: folder + "/users.json"))) as! [[String: Any]]
            let queryItems = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems ?? []
            let parameters = Dictionary(uniqueKeysWithValues: queryItems.map { ($0.name, $0.value ?? "") })
            let query = (parameters["q"] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let size = Int(parameters["page_size"] ?? "50")!
            let filtered = users.filter { user in
                query.isEmpty || (user["name"] as! String).localizedCaseInsensitiveContains(query) || (user["id"] as! String).localizedCaseInsensitiveContains(query)
                    || (user["assignments"] as? [[String: Any]] ?? []).contains { ($0["node_id"] as? String ?? "").localizedCaseInsensitiveContains(query) }
            }.sorted { left, right in
                let lhs = left["name"] as! String, rhs = right["name"] as! String
                if parameters["order"] == "desc" {
                    return lhs == rhs ? (left["id"] as! String) > (right["id"] as! String) : lhs > rhs
                }
                return lhs == rhs ? (left["id"] as! String) < (right["id"] as! String) : lhs < rhs
            }
            let page = min(Int(parameters["page"] ?? "1")!, max(1, (filtered.count + size - 1) / size))
            let items = Array(filtered.dropFirst((page - 1) * size).prefix(size))
            data = try! JSONSerialization.data(withJSONObject: ["items": items, "total": filtered.count, "page": page, "page_size": size])
        }
        if path == "/api/v2/credentials" {
            var items = try! JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: folder + "/credentials.json"))) as! [[String: Any]]
            let extraCount = Int(ProcessInfo.processInfo.environment["HYSTERIAX_TEST_EXTRA_CA_COUNT"] ?? "0") ?? 0
            if extraCount > 0, let ca = items.first(where: { $0["id"] as? String == "ca-1" }) {
                for index in 1...extraCount {
                    var extra = ca; extra["id"] = "ca-extra-\\(index)"; extra["name"] = "A CA \\(index)"; items.append(extra)
                }
            }
            let queryItems = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems ?? []
            let parameters = Dictionary(uniqueKeysWithValues: queryItems.map { ($0.name, $0.value ?? "") })
            let size = Int(parameters["page_size"] ?? "50")!
            let query = (parameters["q"] ?? "").trimmingCharacters(in:.whitespacesAndNewlines)
            let filtered = items.filter { entry in
                let metadata = entry["metadata"] as? [String:Any] ?? [:]
                let userOwned = entry["owner_user_id"] as? String != nil
                let categoryMatches = parameters["category"] == nil || parameters["category"] == "all" || userOwned == (parameters["category"] == "user")
                return categoryMatches && (query.isEmpty || (entry["name"] as! String).localizedCaseInsensitiveContains(query)) && (parameters["include_internal"] == "true" || metadata["internal_probe"] as? Bool != true) && (parameters["kind"] == nil || parameters["kind"] == "" || entry["kind"] as? String == parameters["kind"])
            }.sorted { left,right in
                for key in [parameters["sort"] ?? "name", "name", "id"] {
                    let lhs = (left[key] as? String ?? "").lowercased(), rhs = (right[key] as? String ?? "").lowercased()
                    if lhs != rhs { return parameters["order"] == "desc" ? lhs > rhs : lhs < rhs }
                }
                return false
            }
            let page = min(Int(parameters["page"] ?? "1")!,max(1,(filtered.count+size-1)/size))
            data = try! JSONSerialization.data(withJSONObject: ["items":Array(filtered.dropFirst((page-1)*size).prefix(size)),"total":filtered.count,"page":page,"page_size":size])
        }
        if request.url!.path == "/api/v2/nodes" {
            let nodes = (try! JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: folder + "/nodes.json"))) as! [[String: Any]]).map { value in
                var node = value
                if node["id"] as? String == "node-1", let state = ProcessInfo.processInfo.environment["HYSTERIAX_TEST_NODE_MTLS_STATE"] {
                    node["mtls_state"] = state; node["mtls_recovery_configured"] = false
                }
                return node
            }
            let queryItems = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems ?? []
            let parameters = Dictionary(uniqueKeysWithValues: queryItems.map { ($0.name, $0.value ?? "") })
            let query = (parameters["q"] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let size = Int(parameters["page_size"] ?? "50")!
            let matches = Set((parameters["state_matches"] ?? "").split(separator: ",").map(String.init))
            func text(_ node: [String: Any], _ key: String) -> String {
                if key == "ssh_host" { return (node["ssh"] as? [String: Any])?["host"] as? String ?? "" }
                return node[key] as? String ?? ""
            }
            let filtered = nodes.filter { node in
                query.isEmpty || ["name", "ssh_host", "id", "state"].contains { text(node, $0).localizedCaseInsensitiveContains(query) }
                    || matches.contains(text(node, "state"))
            }.sorted { left, right in
                for key in [parameters["sort"] ?? "name", "name", "id"] {
                    let lhs = text(left, key).lowercased(), rhs = text(right, key).lowercased()
                    if lhs != rhs { return parameters["order"] == "desc" ? lhs > rhs : lhs < rhs }
                }
                return false
            }
            let page = min(Int(parameters["page"] ?? "1")!, max(1, (filtered.count + size - 1) / size))
            data = try! JSONSerialization.data(withJSONObject: ["items": Array(filtered.dropFirst((page - 1) * size).prefix(size)), "total": filtered.count, "page": page, "page_size": size])
        }
        if path == "/api/v2/nodes/node-1", request.httpMethod == "GET", let state = ProcessInfo.processInfo.environment["HYSTERIAX_TEST_NODE_MTLS_STATE"] {
            var node = try! JSONSerialization.jsonObject(with:data) as! [String:Any]
            node["mtls_state"] = state; node["mtls_recovery_configured"] = false
            data = try! JSONSerialization.data(withJSONObject:node)
        }
        var status = paths[path] == nil && !["/api/v2/users", "/api/v2/credentials", "/api/v2/nodes"].contains(path) ? 404 : 200
        if path == "/api/v2/nodes/node-1" && request.httpMethod == "PATCH" {
            let stream = request.httpBodyStream
            var body = request.httpBody ?? Data()
            if let stream { stream.open(); defer { stream.close() }; var buffer = [UInt8](repeating:0,count:4096); while stream.hasBytesAvailable { let n=stream.read(&buffer,maxLength:buffer.count); if n<=0 { break }; body.append(contentsOf:buffer.prefix(n)) } }
            try! body.write(to:URL(fileURLWithPath:folder+"/node-patch.json"))
            data = Data(#"{"id":"node-1","name":"香港","revision":2,"state":"syncing","sync_job_queued":true}"#.utf8)
        }
        if path == "/api/v2/authorization-groups" && request.httpMethod == "POST" {
            data = Data(#"{"revocation_job_ids":[]}"#.utf8)
            status = 201
        }
        if path == "/api/v2/authorization-groups/group-1" && request.httpMethod == "PATCH" {
            let stream = request.httpBodyStream
            var body = request.httpBody ?? Data()
            if let stream { stream.open(); defer { stream.close() }; var buffer = [UInt8](repeating:0,count:4096); while stream.hasBytesAvailable { let n=stream.read(&buffer,maxLength:buffer.count); if n<=0 { break }; body.append(contentsOf:buffer.prefix(n)) } }
            let input = try! JSONSerialization.jsonObject(with:body) as! [String:Any]
            precondition(input["preview_token"] == nil && input["user_ids"] == nil && input["node_ids"] == nil)
            precondition(input["expected_revision"] as? Int == 1)
            data = Data(#"{"revocation_job_ids":[]}"#.utf8)
        }
        if path.contains("authorization-groups"), path.hasSuffix("/preview") {
            preconditionFailure("authorization previews are retired")
        }
        if path == "/api/v2/authorization-groups/group-1" && request.httpMethod == "DELETE" {
            data = Data(#"{"revocation_job_ids":[]}"#.utf8)
        }
        if path == "/api/v2/users/user-1/authorization-groups" && request.httpMethod == "PUT" {
            data = Data(#"{"user_id":"user-1","revision":2,"group_ids":[],"assignments":[],"additions_count":0,"removals_count":1,"created_credentials":[],"revocation_job_ids":[]}"#.utf8)
            status = 200
        }
        if request.url!.path == "/api/v2/events" { data = Data() }
        let resultData = data
        let resultStatus = status
        let deliver: @Sendable () -> Void = { [self] in
            guard canDeliver else { return }
            let response = HTTPURLResponse(url: request.url!, statusCode: resultStatus, httpVersion: "HTTP/1.1", headerFields: ["Content-Type":path == "/api/v2/events" ? "text/event-stream" : "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: resultData)
            client?.urlProtocolDidFinishLoading(self)
        }
        let delay = Double(ProcessInfo.processInfo.environment["HYSTERIAX_TEST_REFRESH_DELAY_SECONDS"] ?? "0") ?? 0
        let parameters = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let slowDirectory = ProcessInfo.processInfo.environment["HYSTERIAX_TEST_SLOW_DIRECTORY"] == "1" && path == "/api/v2/users" && parameters.contains { $0.name == "page_size" && $0.value == "200" }
        if slowDirectory, Self.state.hasCommitted {
            try! Data().write(to: URL(fileURLWithPath: folder + "/directory-refresh-started"))
            DispatchQueue.global().asyncAfter(deadline: .now() + 8, execute: deliver)
        } else if path == "/api/v2/server/monitoring", Self.state.hasCommitted, delay > 0 {
            DispatchQueue.global().asyncAfter(deadline: .now() + delay, execute: deliver)
        } else { deliver() }
    }
    override func stopLoading() { deliveryLock.lock(); stopped = true; deliveryLock.unlock() }
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
    only = ["-only-testing:AuthorizationUITests/AuthorizationFixtureUITests/" + name for name in (args.only_test or [])]
    with log.open("w") as output:
        result = subprocess.run(["xcodebuild", "test", "-project", str(temp / "AuthorizationUITestHarness.xcodeproj"), "-scheme", "HysteriaX", "-destination", "platform=macOS,arch=arm64", "-derivedDataPath", str(temp / "DerivedData"), "-resultBundlePath", str(results), "CODE_SIGNING_ALLOWED=YES", "CODE_SIGNING_IDENTITY=-"] + only, stdout=output, stderr=subprocess.STDOUT)
    print("Authorization UI results:", results)
    print("Authorization UI log:", log)
    raise SystemExit(result.returncode)
