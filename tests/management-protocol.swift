import Foundation

private struct SecretReceipt: Codable, Sendable { let secret: String }
private struct WriteBody: Encodable, Sendable { let fields: [String: String] }

private final class ProtocolTransport: URLProtocol, @unchecked Sendable {
    enum Result { case failure; case response(Int, Data, String) }
    final class Backend: @unchecked Sendable {
        let lock = NSLock()
        var lost = 0
        var statuses: [Int] = []
        var corrupt = false
        var writes = 0
        var requests: [URLRequest] = []
        var receipts: [String: Data] = [:]
        var events = ""
        var storeMode = false
        var failState = false
        var failedStateReads = 0
        func reset(lost: Int = 0, statuses: [Int] = [], corrupt: Bool = false, events: String = "", store: Bool = false) {
            lock.lock(); defer { lock.unlock() }
            self.lost=lost; self.statuses=statuses; self.corrupt=corrupt; self.events=events
            writes=0; requests=[]; receipts=[:]; storeMode=store; failState=false; failedStateReads=0
        }
        func recorded() -> [URLRequest] { lock.lock(); defer { lock.unlock() }; return requests }
        func commits() -> Int { lock.lock(); defer { lock.unlock() }; return writes }
        func failedReads() -> Int { lock.lock(); defer { lock.unlock() }; return failedStateReads }
        func recover() { lock.lock(); defer { lock.unlock() }; failState=false }
        func result(_ request: URLRequest) -> Result {
            lock.lock(); defer { lock.unlock() }
            requests.append(request)
            let path=request.url!.path
            if path.hasSuffix("/events") {
                if storeMode { failState = failedStateReads == 0 }
                if statuses.first == 410 { statuses.removeFirst(); return .response(410,Data(),"application/json") }
                return .response(200,Data(events.utf8),"text/event-stream")
            }
            if storeMode {
                if path.hasSuffix("/version") { return .response(200,Data("{\"api_version\":\"1.0.0\",\"directory_generation\":1,\"service_version\":\"test\",\"hysteria_version\":\"test\",\"mihomo_version\":\"test\",\"features\":[]}".utf8),"application/json") }
                if path == "/readyz" { return .response(200,Data("{\"status\":\"ok\"}".utf8),"application/json") }
                if path.hasSuffix("/users"), failState { failedStateReads += 1; return .response(503,Data(),"application/json") }
                if path.hasSuffix("/tokens") { return .response(200,Data("[]".utf8),"application/json") }
                if path.hasSuffix("/monitoring") { return .response(503,Data(),"application/json") }
                return .response(200,Data("{\"items\":[],\"total\":0,\"page\":1,\"page_size\":50}".utf8),"application/json")
            }
            let status=statuses.isEmpty ? 200 : statuses.removeFirst()
            if !(200..<300).contains(status) { return .response(status,Data("{\"error\":{\"code\":\"test\",\"message\":\"test\"}}".utf8),"application/json") }
            if request.httpMethod == "GET" { return .response(200,Data("{\"secret\":\"read\"}".utf8),"application/json") }
            let key=(request.value(forHTTPHeaderField:"Authorization") ?? "") + (request.value(forHTTPHeaderField:"Idempotency-Key") ?? UUID().uuidString)
            if receipts[key] == nil { writes += 1; receipts[key]=try! JSONEncoder().encode(SecretReceipt(secret:"receipt-\(writes)")) }
            if lost > 0 {lost -= 1; return .failure}
            if corrupt {corrupt=false; return .response(200,Data("invalid-json".utf8),"application/json")}
            if status == 204 {return .response(204,Data(),"application/json")}
            return .response(status,receipts[key]!,"application/json")
        }
    }
    static let backend = Backend()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        switch Self.backend.result(request) {
        case .failure: client?.urlProtocol(self,didFailWithError:URLError(.networkConnectionLost))
        case let .response(status,data,type):
            let response=HTTPURLResponse(url:request.url!,statusCode:status,httpVersion:"HTTP/1.1",headerFields:["Content-Type":type])!
            client?.urlProtocol(self,didReceive:response,cacheStoragePolicy:.notAllowed)
            client?.urlProtocol(self,didLoad:data)
            client?.urlProtocolDidFinishLoading(self)
        }
    }
    override func stopLoading() {}
}

@MainActor
private struct SilentNotifications: NodeNotificationTransport {
    func requestPermission() async throws -> Bool { false }
    func isAuthorized() async -> Bool { false }
    func send(id: String,title: String,body: String) async throws {}
}

@main
struct ManagementProtocolChecks {
    static func session() -> URLSession {
        let configuration=URLSessionConfiguration.ephemeral
        configuration.protocolClasses=[ProtocolTransport.self]
        return URLSession(configuration:configuration)
    }
    static func key(_ request: URLRequest) -> String { request.value(forHTTPHeaderField:"Idempotency-Key")! }
    static func main() async throws {
        let storage="hysteriax.protocol-check.\(UUID())"
        defer {UserDefaults.standard.removeObject(forKey:storage)}
        let transport=session()
        defer {transport.invalidateAndCancel()}
        let url=URL(string:"https://protocol-fixture.invalid")!
        let journal=ManagementWriteJournal(storageKey:storage)
        let client=APIClient(baseURL:url,token:"private-test-token",session:transport,writeJournal:journal)
        let post=APIOperation<WriteBody,SecretReceipt>(method:"POST",path:"api/v2/example",queryParameters:[:])
        let body=WriteBody(fields:["password":"private-test-password","name":"same"])

        ProtocolTransport.backend.reset(lost:1)
        let first=try await client.post(post,body:body)
        let requests=ProtocolTransport.backend.recorded()
        precondition(requests.count==2 && key(requests[0])==key(requests[1]))
        precondition(first.secret=="receipt-1" && ProtocolTransport.backend.commits()==1)
        precondition((UserDefaults.standard.dictionary(forKey:storage) ?? [:]).isEmpty)
        _ = try await client.post(post,body:body)
        precondition(key(ProtocolTransport.backend.recorded().last!) != key(requests[0]))
        precondition(ProtocolTransport.backend.commits()==2)

        ProtocolTransport.backend.reset(lost:3)
        do {_ = try await client.post(post,body:body); preconditionFailure("lost response must remain uncertain")}
        catch is URLError {}
        let uncertain=ProtocolTransport.backend.recorded()
        precondition(uncertain.count==3 && Set(uncertain.map(key)).count==1)
        let persisted=UserDefaults.standard.dictionary(forKey:storage) as! [String:String]
        precondition(persisted.count==1 && !String(describing:persisted).contains("private-test"))
        let restored=APIClient(baseURL:url,token:"private-test-token",session:transport,writeJournal:ManagementWriteJournal(storageKey:storage))
        let recovered=try await restored.post(post,body:WriteBody(fields:["name":"same","password":"private-test-password"]))
        precondition(recovered.secret=="receipt-1" && ProtocolTransport.backend.commits()==1)
        precondition(key(ProtocolTransport.backend.recorded().last!)==key(uncertain[0]))

        ProtocolTransport.backend.reset(corrupt:true)
        do {_ = try await client.post(post,body:body);preconditionFailure("invalid success response must not acknowledge the key")}
        catch is APIClientError {}
        let corruptKey=key(ProtocolTransport.backend.recorded()[0])
        _ = try await client.post(post,body:body)
        precondition(key(ProtocolTransport.backend.recorded().last!)==corruptKey && ProtocolTransport.backend.commits()==1)

        for method in ["POST","PUT","PATCH","DELETE"] {
            ProtocolTransport.backend.reset(statuses:[503,200])
            let operation=APIOperation<WriteBody,SecretReceipt>(method:method,path:"/api/v2/example",queryParameters:[:])
            switch method {
            case "POST": _ = try await client.post(operation,body:body)
            case "PUT": _ = try await client.put(operation,body:body)
            case "PATCH": _ = try await client.patch(operation,body:body)
            default: _ = try await client.delete(operation,body:body)
            }
            let recorded=ProtocolTransport.backend.recorded()
            precondition(recorded.count==2 && key(recorded[0])==key(recorded[1]))
        }
        ProtocolTransport.backend.reset(statuses:[409])
        do {_ = try await client.post(post,body:body);preconditionFailure("business conflict must be returned")}
        catch is APIClientError {}
        precondition(ProtocolTransport.backend.recorded().count==1 && (UserDefaults.standard.dictionary(forKey:storage) ?? [:]).isEmpty)
        ProtocolTransport.backend.reset(statuses:[204])
        try await client.deleteNoContent(APIOperation<NoRequest,NoResponse>(method:"DELETE",path:"api/v2/example",queryParameters:[:]))
        precondition((UserDefaults.standard.dictionary(forKey:storage) ?? [:]).isEmpty)
        ProtocolTransport.backend.reset(statuses:[204])
        let _: NoResponse = try await client.delete(APIOperation<WriteBody,NoResponse>(method:"DELETE",path:"api/v2/example",queryParameters:[:]),body:body)
        precondition((UserDefaults.standard.dictionary(forKey:storage) ?? [:]).isEmpty)

        ProtocolTransport.backend.reset(statuses:[503])
        do {_ = try await client.get(APIOperation<NoRequest,SecretReceipt>(method:"GET",path:"api/v2/example",queryParameters:[:]));preconditionFailure("GET should preserve its retry policy")}
        catch is APIClientError {}
        precondition(ProtocolTransport.backend.recorded().count==1 && ProtocolTransport.backend.recorded()[0].value(forHTTPHeaderField:"Idempotency-Key")==nil)
        ProtocolTransport.backend.reset(lost:1)
        do {_ = try await client.post(APIOperation<WriteBody,SecretReceipt>(method:"POST",path:"api/v1/example",queryParameters:[:]),body:body);preconditionFailure("legacy writes have no receipt authority")}
        catch is URLError {}
        precondition(ProtocolTransport.backend.recorded().count==1 && ProtocolTransport.backend.recorded()[0].value(forHTTPHeaderField:"Idempotency-Key")==nil)

        let boundedStorage=storage+".bounded"
        defer {UserDefaults.standard.removeObject(forKey:boundedStorage)}
        let bounded=ManagementWriteJournal(storageKey:boundedStorage,limit:2)
        let identity=try await bounded.reserve(method:"POST",url:url,token:"actor-a",body:Data("secret".utf8))
        let second=try await bounded.reserve(method:"POST",url:url,token:"actor-b",body:Data("secret".utf8))
        precondition(identity.key != second.key)
        do {_ = try await bounded.reserve(method:"PATCH",url:url,token:"actor-a",body:nil);preconditionFailure("pending requests must not be evicted")}
        catch is APIClientError {}
        let sameIdentity = try await bounded.reserve(method:"POST",url:url,token:"actor-a",body:Data("secret".utf8))
        precondition(sameIdentity.key==identity.key)
        await bounded.acknowledge(identity)
        let replacement=try await bounded.reserve(method:"POST",url:url,token:"actor-a",body:Data("secret".utf8))
        await bounded.acknowledge(identity)
        let afterStaleAcknowledgement = try await bounded.reserve(method:"POST",url:url,token:"actor-a",body:Data("secret".utf8))
        precondition(afterStaleAcknowledgement.key==replacement.key)

        try parserChecks()
        let epoch="77777777-7777-4777-8777-777777777777"
        let wire="id: v2:\(epoch):1\nevent: job.started\ndata: {\"event_id\":9}\n\nid: v2:\(epoch):2\nevent: stream.cursor\ndata: {\"cursor\":\"v2:\(epoch):2\"}\n\n"
        ProtocolTransport.backend.reset(events:wire)
        let stream=try await client.events(APIOperation<NoRequest,NoResponse>(method:"GET",path:"api/v2/events",queryParameters:[:]),lastEventID:"v2:\(epoch):0")
        var delivered:[ManagementEvent]=[]
        for try await event in stream {delivered.append(event)}
        precondition(delivered.count==2 && delivered[0].sourceID==9 && delivered[1].kind=="stream.cursor", "received \(delivered.count) complete frames")
        precondition(ProtocolTransport.backend.recorded()[0].value(forHTTPHeaderField:"Last-Event-ID")=="v2:\(epoch):0")
        ProtocolTransport.backend.reset(statuses:[410])
        do {_ = try await client.events(APIOperation<NoRequest,NoResponse>(method:"GET",path:"api/v2/events",queryParameters:[:]),lastEventID:nil);preconditionFailure("410 must request a full resync")}
        catch APIClientError.eventCursorExpired {}
        try await acknowledgementCheck(session:transport)
        print("management protocol: stable write recovery, bounded journal, SSE parsing and acknowledgement passed")
    }

    static func parserChecks() throws {
        let epoch="77777777-7777-4777-8777-777777777777"
        let valid="v2:\(epoch):42"
        let parsed = try ManagementEventCursor(valid)
        precondition(parsed.sequence==42)
        for ending in ["\n", "\r\n", "\r"] {
            var decoder = ManagementEventDecoder(v2: true)
            var events: [ManagementEvent] = []
            let wire = "\u{feff}id: \(valid)\(ending)event: job.progress\(ending)data: {\"event_id\":7}\(ending)\(ending)"
            for byte in wire.utf8 { if let event = try decoder.append(byte) { events.append(event) } }
            precondition(events.count == 1 && events[0].sourceID == 7)
        }
        for value in ["42","v1:\(epoch):1","v2:\(epoch):-1","v2:\(epoch):01","v2:\(epoch):9223372036854775808"] {
            do {_ = try ManagementEventCursor(value);preconditionFailure("bad cursor accepted")}
            catch is APIClientError {}
        }
        var parser=ManagementEventParser(v2:true)
        var result:ManagementEvent?
        for line in [": comment","id: \(valid)","event: job.progress","data: {","data: \"event_id\":7}",""] {if let event=try parser.append(line){result=event}}
        precondition(result?.cursor==valid && result?.sourceID==7)
        for line in ["event: stream.reset","data: {\"code\":\"event_cursor_expired\"}"] {_ = try parser.append(line)}
        do {_ = try parser.append("");preconditionFailure("reset must stop the stream")}
        catch APIClientError.eventCursorExpired {}
        var invalid=ManagementEventParser(v2:true)
        for line in ["id: \(valid)","event: stream.cursor","data: {\"cursor\":\"wrong\"}"] {_ = try invalid.append(line)}
        do {_ = try invalid.append("");preconditionFailure("checkpoint mismatch accepted")}
        catch is APIClientError {}
    }

    @MainActor static func acknowledgementCheck(session:URLSession) async throws {
        let address="https://ack-\(UUID().uuidString.lowercased()).invalid"
        let key="lastEventID.\(address)"
        let oldAddress=UserDefaults.standard.object(forKey:"serviceAddress")
        defer {
            for prefix in ["lastEventID.","displaySnapshot.","overviewHistory.","dnsSnapshot.","currentAdminTokenID."] {UserDefaults.standard.removeObject(forKey:prefix+address)}
            if let oldAddress {UserDefaults.standard.set(oldAddress,forKey:"serviceAddress")} else {UserDefaults.standard.removeObject(forKey:"serviceAddress")}
        }
        ProtocolTransport.backend.reset(events:"id: 1\nevent: job.progress\ndata: {\"event_id\":1}\n\n",store:true)
        let notifications=NodeAlertNotifications(transport:SilentNotifications(),defaults:UserDefaults(suiteName:"hysteriax.protocol.notifications")!)
        let store=ManagementStore(restoreSnapshot:false,nodeNotifications:notifications)
        try await store.connect(serviceAddress:address,token:"test-store-token",session:session)
        for _ in 0..<100 where ProtocolTransport.backend.failedReads()==0 {try await Task.sleep(for:.milliseconds(10))}
        precondition(ProtocolTransport.backend.failedReads()>0)
        precondition(UserDefaults.standard.string(forKey:key)==nil,"failed state refresh must not acknowledge the event")
        ProtocolTransport.backend.recover()
        for _ in 0..<400 where UserDefaults.standard.string(forKey:key)==nil {try await Task.sleep(for:.milliseconds(10))}
        precondition(UserDefaults.standard.string(forKey:key)=="1","the original event must be recovered after state refresh succeeds")
    }
}
