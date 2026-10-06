#!/usr/bin/env python3
"""Run isolated DNS UI tests with a fixture transport and no Keychain/network access."""
from pathlib import Path
import json
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
if not shutil.which("xcodegen"):
    raise SystemExit("xcodegen is required")

with tempfile.TemporaryDirectory(prefix="hysteriax-dns-ui-") as folder:
    temp = Path(folder)
    shutil.copytree(ROOT / "apps/macos/Sources/HysteriaX", temp / "AppSources")
    fixtures = temp / "fixtures"
    fixtures.mkdir()
    stamp = "2026-10-06T00:00:00Z"
    connection = {"id":"connection-1","name":"Cloudflare test","provider":"cloudflare","credential_id":"credential-1","credential_version":1,"revision":1,"status":"verified","created_at":stamp,"updated_at":stamp}
    zone = {"id":"zone-1","connection_id":"connection-1","provider_zone_id":"remote-zone","name":"example.test","enabled":True,"revision":1}
    record = {"id":"record-1","zone_id":"zone-1","provider_record_id":"remote-record","name":"hk.example.test","record_type":"A","content":"8.8.8.8","ttl":300,"proxied":False,"origin":"external","revision":1,"state":"synced","resolution_status":"verified","bound_node_id":"node-1","updated_at":stamp}
    binding = {"node_id":"node-1","zone_id":"zone-1","hostname":"hk.example.test","record_ids":["record-1"],"records":[record],"revision":1}
    ssh = {"host":"8.8.8.8","port":22,"username":"root","auth_type":"private_key","credential_id":"ssh-1","credential_version":1}
    node = {"id":"node-1","name":"Hong Kong","revision":1,"state":"new","ssh":ssh,"dns_binding":binding}
    credential = {"id":"credential-1","name":"Cloudflare","kind":"dns","revision":1,"latest_version":1,"archived":False,"status":"active","metadata":{"provider":"cloudflare","fields":["cloudflare_api_token"]},"created_at":stamp,"updated_at":stamp}
    detail = node | {"config":{},"yaml_preview":"listen: :443","public":{"host":"hk.example.test","port":443,"listen_addr":":443","skip_cert_verify":False},"created_at":stamp,"updated_at":stamp}
    responses = {"version":{"api_version":"1.0.0","features":["dns_management"],"current_admin_token_id":"fixture","service_version":"fixture","hysteria_version":"app/v2.12.3","mihomo_version":"v1.19.31"},"connections":[connection],"zones":[zone],"records":[record],"record":record,"nodes":[node],"node":detail,"credentials":[credential],"empty":[]}
    for name, value in responses.items():
        (fixtures / (name + ".json")).write_text(json.dumps(value))
    support = '''import Foundation
final class DNSFixtureProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let paths = ["/api/v1/version":"version", "/api/v1/dns/connections":"connections", "/api/v1/dns/zones":"zones", "/api/v1/dns/records":"records", "/api/v1/dns/records/record-1":"record", "/api/v1/nodes":"nodes", "/api/v1/nodes/node-1":"node", "/api/v1/nodes/node-1/resources":"empty", "/api/v1/credentials":"credentials", "/api/v1/users":"empty", "/api/v1/jobs":"empty", "/api/v1/audit":"empty", "/api/v1/admin/tokens":"empty"]
        let name = paths[request.url!.path]
        let folder = ProcessInfo.processInfo.environment["HYSTERIAX_DNS_FIXTURE_DIRECTORY"]!
        let data = name.map { try! Data(contentsOf: URL(fileURLWithPath: folder + "/" + $0 + ".json")) } ?? Data(#"{"code":"fixture","message":"not available"}"#.utf8)
        let response = HTTPURLResponse(url: request.url!, statusCode: name == nil ? 404 : 200, httpVersion: "HTTP/1.1", headerFields: ["Content-Type":"application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
'''
    (temp / "AppSources/Support/DNSFixtureProtocol.swift").write_text(support)
    store = temp / "AppSources/Stores/ManagementStore.swift"
    marker = "    init(client: APIClient? = nil, restoreSnapshot: Bool = true, nodeNotifications: NodeAlertNotifications? = nil) {"
    replacement = marker + '''
        if ProcessInfo.processInfo.environment["HYSTERIAX_DNS_FIXTURE_DIRECTORY"] != nil {
            self.nodeNotifications = nodeNotifications
            serviceAddress = "https://dns-fixture.invalid"
            let config = URLSessionConfiguration.ephemeral
            config.protocolClasses = [DNSFixtureProtocol.self]
            api = APIClient(baseURL: URL(string: serviceAddress)!, token: "fixture", session: URLSession(configuration: config))
            supportsDNSManagement = true
            isConnected = true
            return
        }
'''
    source = store.read_text()
    assert marker in source
    store.write_text(source.replace(marker, replacement, 1))
    tests = temp / "UITests"
    tests.mkdir()
    (tests / "DNSFixtureUITests.swift").write_text((ROOT / "apps/macos/UITests/DNSFixtureUITests.swift").read_text().replace("__DNS_FIXTURES__", str(fixtures)))
    bundle = f"com.hysteriax.dns.fixture{int(time.time())}"
    (temp / "project.yml").write_text(f'''name: DNSUITestHarness
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
  DNSUITests:
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
        DNSUITests: [test]
    test:
      targets: [DNSUITests]
''')
    subprocess.run(["xcodegen","generate","--spec","project.yml"],cwd=temp,check=True,stdout=subprocess.DEVNULL)
    results = ROOT / "apps/macos/.build" / f"dns-ui-{int(time.time())}.xcresult"
    log = results.with_suffix(".log")
    results.parent.mkdir(parents=True,exist_ok=True)
    with log.open("w") as output:
        result = subprocess.run(["xcodebuild","test","-project",str(temp / "DNSUITestHarness.xcodeproj"),"-scheme","HysteriaX","-destination","platform=macOS,arch=arm64","-derivedDataPath",str(temp / "DerivedData"),"-resultBundlePath",str(results),"CODE_SIGNING_ALLOWED=YES","CODE_SIGNING_IDENTITY=-"],stdout=output,stderr=subprocess.STDOUT)
    print("DNS UI results:",results)
    print("DNS UI log:",log)
    raise SystemExit(result.returncode)
