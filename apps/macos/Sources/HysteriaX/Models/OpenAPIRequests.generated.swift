// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.
import Foundation

struct DNSConnectionCreateRequest: Encodable, Sendable {
    let name: String
    let credentialId: String
    let credentialVersion: Int

    init(
        name: String,
        credentialId: String,
        credentialVersion: Int
    ) {
        self.name = name
        self.credentialId = credentialId
        self.credentialVersion = credentialVersion
    }

    enum CodingKeys: String, CodingKey {
        case name
        case credentialId = "credential_id"
        case credentialVersion = "credential_version"
    }
}

struct DNSConnectionPatchRequest: Encodable, Sendable {
    let expectedRevision: Int
    let name: String

    init(
        expectedRevision: Int,
        name: String
    ) {
        self.expectedRevision = expectedRevision
        self.name = name
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case name
    }
}

struct DNSActionRequest: Encodable, Sendable {
    let expectedRevision: Int
    let idempotencyKey: String

    init(
        expectedRevision: Int,
        idempotencyKey: String
    ) {
        self.expectedRevision = expectedRevision
        self.idempotencyKey = idempotencyKey
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case idempotencyKey = "idempotency_key"
    }
}

struct DNSZonePatchRequest: Encodable, Sendable {
    let expectedRevision: Int
    let enabled: Bool

    init(
        expectedRevision: Int,
        enabled: Bool
    ) {
        self.expectedRevision = expectedRevision
        self.enabled = enabled
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case enabled
    }
}

struct DNSRecordCreateRequest: Encodable, Sendable {
    let zoneId: String
    let idempotencyKey: String
    let record: DNSRecordInput

    init(
        zoneId: String,
        idempotencyKey: String,
        record: DNSRecordInput
    ) {
        self.zoneId = zoneId
        self.idempotencyKey = idempotencyKey
        self.record = record
    }

    enum CodingKeys: String, CodingKey {
        case zoneId = "zone_id"
        case idempotencyKey = "idempotency_key"
        case record
    }
}

struct DNSRecordUpdateRequest: Encodable, Sendable {
    let expectedRevision: Int
    let idempotencyKey: String
    let record: DNSRecordInput

    init(
        expectedRevision: Int,
        idempotencyKey: String,
        record: DNSRecordInput
    ) {
        self.expectedRevision = expectedRevision
        self.idempotencyKey = idempotencyKey
        self.record = record
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case idempotencyKey = "idempotency_key"
        case record
    }
}

struct DNSBindingSetRequest: Encodable, Sendable {
    let expectedRevision: Int
    let allocation: DNSAllocation

    init(
        expectedRevision: Int,
        allocation: DNSAllocation
    ) {
        self.expectedRevision = expectedRevision
        self.allocation = allocation
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case allocation
    }
}

struct DNSBindingRemoveRequest: Encodable, Sendable {
    let expectedRevision: Int
    let idempotencyKey: String
    let publicHost: String

    init(
        expectedRevision: Int,
        idempotencyKey: String,
        publicHost: String
    ) {
        self.expectedRevision = expectedRevision
        self.idempotencyKey = idempotencyKey
        self.publicHost = publicHost
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case idempotencyKey = "idempotency_key"
        case publicHost = "public_host"
    }
}

struct DNSRecordInput: Encodable, Sendable {
    let name: String
    let recordType: String
    let content: String
    let ttl: Int
    let proxied: Bool

    init(
        name: String,
        recordType: String,
        content: String,
        ttl: Int = 1,
        proxied: Bool = false
    ) {
        self.name = name
        self.recordType = recordType
        self.content = content
        self.ttl = ttl
        self.proxied = proxied
    }

    enum CodingKeys: String, CodingKey {
        case name
        case recordType = "record_type"
        case content
        case ttl
        case proxied
    }
}

struct DNSAllocation: Encodable, Sendable {
    let idempotencyKey: String
    let zoneId: String
    let mode: String
    let prefix: String?
    let hostname: String?
    let ipv4: String?
    let ipv6: String?
    let recordIds: [String]?

    init(
        idempotencyKey: String,
        zoneId: String,
        mode: String,
        prefix: String? = nil,
        hostname: String? = nil,
        ipv4: String? = nil,
        ipv6: String? = nil,
        recordIds: [String]? = nil
    ) {
        self.idempotencyKey = idempotencyKey
        self.zoneId = zoneId
        self.mode = mode
        self.prefix = prefix
        self.hostname = hostname
        self.ipv4 = ipv4
        self.ipv6 = ipv6
        self.recordIds = recordIds
    }

    enum CodingKeys: String, CodingKey {
        case idempotencyKey = "idempotency_key"
        case zoneId = "zone_id"
        case mode
        case prefix
        case hostname
        case ipv4
        case ipv6
        case recordIds = "record_ids"
    }
}

struct CredentialCreateRequest: Encodable, Sendable {
    let name: String
    let kind: String
    let ownerUserId: String?
    let reminderAt: String?
    let payload: [String: JSONValue]

    init(
        name: String,
        kind: String,
        ownerUserId: String? = nil,
        reminderAt: String? = nil,
        payload: [String: JSONValue]
    ) {
        self.name = name
        self.kind = kind
        self.ownerUserId = ownerUserId
        self.reminderAt = reminderAt
        self.payload = payload
    }

    enum CodingKeys: String, CodingKey {
        case name
        case kind
        case ownerUserId = "owner_user_id"
        case reminderAt = "reminder_at"
        case payload
    }
}

struct CredentialPatchRequest: Encodable, Sendable {
    let expectedRevision: Int
    let name: String
    let archived: Bool
    let reminderAt: String?

    init(
        expectedRevision: Int,
        name: String,
        archived: Bool,
        reminderAt: String? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.name = name
        self.archived = archived
        self.reminderAt = reminderAt
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case name
        case archived
        case reminderAt = "reminder_at"
    }
}

struct CredentialPublishRequest: Encodable, Sendable {
    let expectedRevision: Int
    let payload: [String: JSONValue]

    init(
        expectedRevision: Int,
        payload: [String: JSONValue]
    ) {
        self.expectedRevision = expectedRevision
        self.payload = payload
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case payload
    }
}

struct NodeUsageUpdateRequest: Encodable, Sendable {
    let expectedRevision: Int
    let usageBytes: Int64
    let reset: Bool?

    init(
        expectedRevision: Int,
        usageBytes: Int64,
        reset: Bool? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.usageBytes = usageBytes
        self.reset = reset
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case usageBytes = "usage_bytes"
        case reset
    }
}

struct NodeCreateRequest: Encodable, Sendable {
    let dnsAllocation: DNSAllocation?
    let package: NodePackage?
    let initialUsageBytes: Int64?
    let name: String
    let sshHost: String
    let sshPort: Int
    let sshUsername: String
    let sshHostFingerprint: String?
    let publicHost: String?
    let publicPort: Int
    let listenAddr: String
    let trafficStatsPort: Int
    let proxyProbeUrl: String?
    let tlsSNI: String?
    let tlsSkipVerify: Bool
    let config: [String: JSONValue]
    let sshCredentialId: String
    let sshCredentialVersion: Int

    init(
        dnsAllocation: DNSAllocation? = nil,
        package: NodePackage? = nil,
        initialUsageBytes: Int64? = nil,
        name: String,
        sshHost: String,
        sshPort: Int,
        sshUsername: String,
        sshHostFingerprint: String? = nil,
        publicHost: String? = nil,
        publicPort: Int,
        listenAddr: String,
        trafficStatsPort: Int = 9780,
        proxyProbeUrl: String? = nil,
        tlsSNI: String? = nil,
        tlsSkipVerify: Bool = false,
        config: [String: JSONValue] = [:],
        sshCredentialId: String,
        sshCredentialVersion: Int
    ) {
        self.dnsAllocation = dnsAllocation
        self.package = package
        self.initialUsageBytes = initialUsageBytes
        self.name = name
        self.sshHost = sshHost
        self.sshPort = sshPort
        self.sshUsername = sshUsername
        self.sshHostFingerprint = sshHostFingerprint
        self.publicHost = publicHost
        self.publicPort = publicPort
        self.listenAddr = listenAddr
        self.trafficStatsPort = trafficStatsPort
        self.proxyProbeUrl = proxyProbeUrl
        self.tlsSNI = tlsSNI
        self.tlsSkipVerify = tlsSkipVerify
        self.config = config
        self.sshCredentialId = sshCredentialId
        self.sshCredentialVersion = sshCredentialVersion
    }

    enum CodingKeys: String, CodingKey {
        case dnsAllocation = "dns_allocation"
        case package
        case initialUsageBytes = "initial_usage_bytes"
        case name
        case sshHost = "ssh_host"
        case sshPort = "ssh_port"
        case sshUsername = "ssh_username"
        case sshHostFingerprint = "ssh_host_fingerprint"
        case publicHost = "public_host"
        case publicPort = "public_port"
        case listenAddr = "listen_addr"
        case trafficStatsPort = "traffic_stats_port"
        case proxyProbeUrl = "proxy_probe_url"
        case tlsSNI = "tls_sni"
        case tlsSkipVerify = "tls_skip_verify"
        case config
        case sshCredentialId = "ssh_credential_id"
        case sshCredentialVersion = "ssh_credential_version"
    }
}

struct NodePatchRequest: Encodable, Sendable {
    let expectedRevision: Int
    let package: NodePackage?
    let name: String?
    let sshHost: String?
    let sshPort: Int?
    let sshUsername: String?
    let sshHostFingerprint: String?
    let publicHost: String?
    let publicPort: Int?
    let listenAddr: String?
    let trafficStatsPort: Int?
    let proxyProbeUrl: String?
    let tlsSNI: String?
    let tlsSkipVerify: Bool?
    let config: [String: JSONValue]?
    let sshCredentialId: String?
    let sshCredentialVersion: Int?

    init(
        expectedRevision: Int,
        package: NodePackage? = nil,
        name: String? = nil,
        sshHost: String? = nil,
        sshPort: Int? = nil,
        sshUsername: String? = nil,
        sshHostFingerprint: String? = nil,
        publicHost: String? = nil,
        publicPort: Int? = nil,
        listenAddr: String? = nil,
        trafficStatsPort: Int? = nil,
        proxyProbeUrl: String? = nil,
        tlsSNI: String? = nil,
        tlsSkipVerify: Bool? = nil,
        config: [String: JSONValue]? = nil,
        sshCredentialId: String? = nil,
        sshCredentialVersion: Int? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.package = package
        self.name = name
        self.sshHost = sshHost
        self.sshPort = sshPort
        self.sshUsername = sshUsername
        self.sshHostFingerprint = sshHostFingerprint
        self.publicHost = publicHost
        self.publicPort = publicPort
        self.listenAddr = listenAddr
        self.trafficStatsPort = trafficStatsPort
        self.proxyProbeUrl = proxyProbeUrl
        self.tlsSNI = tlsSNI
        self.tlsSkipVerify = tlsSkipVerify
        self.config = config
        self.sshCredentialId = sshCredentialId
        self.sshCredentialVersion = sshCredentialVersion
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case package
        case name
        case sshHost = "ssh_host"
        case sshPort = "ssh_port"
        case sshUsername = "ssh_username"
        case sshHostFingerprint = "ssh_host_fingerprint"
        case publicHost = "public_host"
        case publicPort = "public_port"
        case listenAddr = "listen_addr"
        case trafficStatsPort = "traffic_stats_port"
        case proxyProbeUrl = "proxy_probe_url"
        case tlsSNI = "tls_sni"
        case tlsSkipVerify = "tls_skip_verify"
        case config
        case sshCredentialId = "ssh_credential_id"
        case sshCredentialVersion = "ssh_credential_version"
    }
}

struct ResourceUploadRequest: Encodable, Sendable {
    let name: String
    let resourceKind: String
    let contentBase64: String

    init(
        name: String,
        resourceKind: String,
        contentBase64: String
    ) {
        self.name = name
        self.resourceKind = resourceKind
        self.contentBase64 = contentBase64
    }

    enum CodingKeys: String, CodingKey {
        case name
        case resourceKind = "resource_kind"
        case contentBase64 = "content_base64"
    }
}

struct UserCreateRequest: Encodable, Sendable {
    let name: String
    let enabled: Bool
    let expiresAt: String?
    let quotaBytes: Int64?

    init(
        name: String,
        enabled: Bool = true,
        expiresAt: String? = nil,
        quotaBytes: Int64? = nil
    ) {
        self.name = name
        self.enabled = enabled
        self.expiresAt = expiresAt
        self.quotaBytes = quotaBytes
    }

    enum CodingKeys: String, CodingKey {
        case name
        case enabled
        case expiresAt = "expires_at"
        case quotaBytes = "quota_bytes"
    }
}

struct UserPatchRequest: Encodable, Sendable {
    let expectedRevision: Int
    let name: String?
    let enabled: Bool?
    let expiresAt: PatchValue<String>?
    let quotaBytes: PatchValue<Int64>?

    init(
        expectedRevision: Int,
        name: String? = nil,
        enabled: Bool? = nil,
        expiresAt: PatchValue<String>? = nil,
        quotaBytes: PatchValue<Int64>? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.name = name
        self.enabled = enabled
        self.expiresAt = expiresAt
        self.quotaBytes = quotaBytes
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case name
        case enabled
        case expiresAt = "expires_at"
        case quotaBytes = "quota_bytes"
    }
}

struct RevisionRequest: Encodable, Sendable {
    let expectedRevision: Int

    init(
        expectedRevision: Int
    ) {
        self.expectedRevision = expectedRevision
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
    }
}

struct AssignmentRequest: Encodable, Sendable {
    let expectedRevision: Int
    let nodeID: String
    let mtlsCredentialId: String?
    let mtlsCredentialVersion: Int?

    init(
        expectedRevision: Int,
        nodeID: String,
        mtlsCredentialId: String? = nil,
        mtlsCredentialVersion: Int? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.nodeID = nodeID
        self.mtlsCredentialId = mtlsCredentialId
        self.mtlsCredentialVersion = mtlsCredentialVersion
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case nodeID = "node_id"
        case mtlsCredentialId = "mtls_credential_id"
        case mtlsCredentialVersion = "mtls_credential_version"
    }
}

struct AssignmentCertificateUpdateRequest: Encodable, Sendable {
    let expectedRevision: Int
    let mtlsCredentialId: String
    let mtlsCredentialVersion: Int

    init(
        expectedRevision: Int,
        mtlsCredentialId: String,
        mtlsCredentialVersion: Int
    ) {
        self.expectedRevision = expectedRevision
        self.mtlsCredentialId = mtlsCredentialId
        self.mtlsCredentialVersion = mtlsCredentialVersion
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case mtlsCredentialId = "mtls_credential_id"
        case mtlsCredentialVersion = "mtls_credential_version"
    }
}

struct CreateAdminTokenRequest: Encodable, Sendable {
    let label: String

    init(
        label: String
    ) {
        self.label = label
    }

    enum CodingKeys: String, CodingKey {
        case label
    }
}

struct AuthenticateHysteriaUserRequest: Encodable, Sendable {
    let addr: String
    let auth: String
    let tx: Int64

    init(
        addr: String,
        auth: String,
        tx: Int64
    ) {
        self.addr = addr
        self.auth = auth
        self.tx = tx
    }

    enum CodingKeys: String, CodingKey {
        case addr
        case auth
        case tx
    }
}

struct NoRequest: Sendable {}
struct NoResponse: Decodable, Sendable {}

struct APIOperation<Request: Sendable, Response: Sendable>: Sendable {
    let method: String
    let path: String
    let queryParameters: [String: String]
}

enum APIEndpoints {
    static let listCredentials: APIOperation<NoRequest, [CredentialSummary]> = APIOperation<NoRequest, [CredentialSummary]>(method: "GET", path: "api/v1/credentials", queryParameters: [:])
    static let createCredential: APIOperation<CredentialCreateRequest, CredentialReceipt> = APIOperation<CredentialCreateRequest, CredentialReceipt>(method: "POST", path: "api/v1/credentials", queryParameters: [:])
    static func getCredential(id: String) -> APIOperation<NoRequest, CredentialDetail> {
        APIOperation(method: "GET", path: "api/v1/credentials/\(id)", queryParameters: [:])
    }
    static func updateCredential(id: String) -> APIOperation<CredentialPatchRequest, CredentialReceipt> {
        APIOperation(method: "PATCH", path: "api/v1/credentials/\(id)", queryParameters: [:])
    }
    static func deleteCredential(id: String, expectedRevision: Int) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "DELETE", path: "api/v1/credentials/\(id)", queryParameters: ["expected_revision": String(expectedRevision)])
    }
    static func publishCredentialVersion(id: String) -> APIOperation<CredentialPublishRequest, CredentialReceipt> {
        APIOperation(method: "POST", path: "api/v1/credentials/\(id)/versions", queryParameters: [:])
    }
    static func getCredentialReferences(id: String) -> APIOperation<NoRequest, [CredentialReference]> {
        APIOperation(method: "GET", path: "api/v1/credentials/\(id)/references", queryParameters: [:])
    }
    static func getCredentialBatch(id: String) -> APIOperation<NoRequest, CredentialBatch> {
        APIOperation(method: "GET", path: "api/v1/credential-batches/\(id)", queryParameters: [:])
    }
    static func retryCredentialBatch(id: String) -> APIOperation<NoRequest, CredentialBatchReceipt> {
        APIOperation(method: "POST", path: "api/v1/credential-batches/\(id)/retry", queryParameters: [:])
    }
    static let getOverview: APIOperation<NoRequest, OverviewResponse> = APIOperation<NoRequest, OverviewResponse>(method: "GET", path: "api/v1/overview", queryParameters: [:])
    static func getOverviewHistory(range: String, timezone: String, nodeID: String? = nil, source: String? = nil) -> APIOperation<NoRequest, OverviewHistory> {
        APIOperation(method: "GET", path: "api/v1/overview/history", queryParameters: ["range": range, "timezone": timezone, "node_id": nodeID.map { String($0) }, "source": source.map { String($0) }].compactMapValues { $0 })
    }
    static let listNodes: APIOperation<NoRequest, [NodeSummary]> = APIOperation<NoRequest, [NodeSummary]>(method: "GET", path: "api/v1/nodes", queryParameters: [:])
    static let createNode: APIOperation<NodeCreateRequest, CreatedEntity> = APIOperation<NodeCreateRequest, CreatedEntity>(method: "POST", path: "api/v1/nodes", queryParameters: [:])
    static func getNode(id: String) -> APIOperation<NoRequest, NodeDetail> {
        APIOperation(method: "GET", path: "api/v1/nodes/\(id)", queryParameters: [:])
    }
    static func updateNode(id: String) -> APIOperation<NodePatchRequest, NodeUpdateResponse> {
        APIOperation(method: "PATCH", path: "api/v1/nodes/\(id)", queryParameters: [:])
    }
    static func deleteNode(id: String, expectedRevision: Int) -> APIOperation<NoRequest, JobReceipt> {
        APIOperation(method: "DELETE", path: "api/v1/nodes/\(id)", queryParameters: ["expected_revision": String(expectedRevision)])
    }
    static func removeNodeRecord(id: String, expectedRevision: Int) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "DELETE", path: "api/v1/nodes/\(id)/record", queryParameters: ["expected_revision": String(expectedRevision)])
    }
    static func updateNodeUsage(id: String) -> APIOperation<NodeUsageUpdateRequest, NodeUsageUpdateResponse> {
        APIOperation(method: "PUT", path: "api/v1/nodes/\(id)/usage", queryParameters: [:])
    }
    static func testNodeSSH(id: String) -> APIOperation<RevisionRequest, JobReceipt> {
        APIOperation(method: "POST", path: "api/v1/nodes/\(id)/ssh-test", queryParameters: [:])
    }
    static func deployNode(id: String) -> APIOperation<RevisionRequest, JobReceipt> {
        APIOperation(method: "POST", path: "api/v1/nodes/\(id)/deploy", queryParameters: [:])
    }
    static func syncNode(id: String) -> APIOperation<RevisionRequest, JobReceipt> {
        APIOperation(method: "POST", path: "api/v1/nodes/\(id)/sync", queryParameters: [:])
    }
    static func rollbackNode(id: String) -> APIOperation<RevisionRequest, JobReceipt> {
        APIOperation(method: "POST", path: "api/v1/nodes/\(id)/rollback", queryParameters: [:])
    }
    static func listNodeResources(id: String) -> APIOperation<NoRequest, [NodeResource]> {
        APIOperation(method: "GET", path: "api/v1/nodes/\(id)/resources", queryParameters: [:])
    }
    static func uploadNodeResource(id: String) -> APIOperation<ResourceUploadRequest, ResourceReceipt> {
        APIOperation(method: "POST", path: "api/v1/nodes/\(id)/resources", queryParameters: [:])
    }
    static func deleteNodeResource(id: String, resourceId: String) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "DELETE", path: "api/v1/nodes/\(id)/resources/\(resourceId)", queryParameters: [:])
    }
    static let listUsers: APIOperation<NoRequest, [UserSummary]> = APIOperation<NoRequest, [UserSummary]>(method: "GET", path: "api/v1/users", queryParameters: [:])
    static let createUser: APIOperation<UserCreateRequest, CreatedEntity> = APIOperation<UserCreateRequest, CreatedEntity>(method: "POST", path: "api/v1/users", queryParameters: [:])
    static func getUser(id: String) -> APIOperation<NoRequest, UserSummary> {
        APIOperation(method: "GET", path: "api/v1/users/\(id)", queryParameters: [:])
    }
    static func updateUser(id: String) -> APIOperation<UserPatchRequest, UserUpdateResponse> {
        APIOperation(method: "PATCH", path: "api/v1/users/\(id)", queryParameters: [:])
    }
    static func deleteUser(id: String, expectedRevision: Int) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "DELETE", path: "api/v1/users/\(id)", queryParameters: ["expected_revision": String(expectedRevision)])
    }
    static func assignUserToNode(id: String) -> APIOperation<AssignmentRequest, AssignmentReceipt> {
        APIOperation(method: "POST", path: "api/v1/users/\(id)/assignments", queryParameters: [:])
    }
    static func unassignUserFromNode(id: String, nodeID: String) -> APIOperation<RevisionRequest, AssignmentMutationResponse> {
        APIOperation(method: "DELETE", path: "api/v1/users/\(id)/assignments/\(nodeID)", queryParameters: [:])
    }
    static func updateAssignmentClientCertificate(id: String, nodeID: String) -> APIOperation<AssignmentCertificateUpdateRequest, AssignmentMutationResponse> {
        APIOperation(method: "PUT", path: "api/v1/users/\(id)/assignments/\(nodeID)", queryParameters: [:])
    }
    static func rotateUserCredentials(id: String) -> APIOperation<RevisionRequest, RotatedCredentials> {
        APIOperation(method: "POST", path: "api/v1/users/\(id)/credentials/rotate", queryParameters: [:])
    }
    static func getSubscription(id: String) -> APIOperation<NoRequest, SubscriptionStatus> {
        APIOperation(method: "GET", path: "api/v1/users/\(id)/subscription", queryParameters: [:])
    }
    static func rotateSubscription(id: String) -> APIOperation<RevisionRequest, SubscriptionReceipt> {
        APIOperation(method: "POST", path: "api/v1/users/\(id)/subscription", queryParameters: [:])
    }
    static func getUserUsage(id: String) -> APIOperation<NoRequest, UserUsageResponse> {
        APIOperation(method: "GET", path: "api/v1/users/\(id)/usage", queryParameters: [:])
    }
    static func resetUserQuota(id: String) -> APIOperation<RevisionRequest, QuotaResetResponse> {
        APIOperation(method: "POST", path: "api/v1/users/\(id)/quota/reset", queryParameters: [:])
    }
    static let listJobs: APIOperation<NoRequest, [JobSummary]> = APIOperation<NoRequest, [JobSummary]>(method: "GET", path: "api/v1/jobs", queryParameters: [:])
    static let getServerMonitoring: APIOperation<NoRequest, ServerMonitoring> = APIOperation<NoRequest, ServerMonitoring>(method: "GET", path: "api/v1/server/monitoring", queryParameters: [:])
    static let getAPIVersion: APIOperation<NoRequest, APIVersion> = APIOperation<NoRequest, APIVersion>(method: "GET", path: "api/v1/version", queryParameters: [:])
    static func getJob(id: String) -> APIOperation<NoRequest, JobDetailResponse> {
        APIOperation(method: "GET", path: "api/v1/jobs/\(id)", queryParameters: [:])
    }
    static func retryJob(id: String) -> APIOperation<RevisionRequest, JobReceipt> {
        APIOperation(method: "POST", path: "api/v1/jobs/\(id)/retry", queryParameters: [:])
    }
    static func streamJobEvents(id: String) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "GET", path: "api/v1/jobs/\(id)/events", queryParameters: [:])
    }
    static let streamEvents: APIOperation<NoRequest, NoResponse> = APIOperation<NoRequest, NoResponse>(method: "GET", path: "api/v1/events", queryParameters: [:])
    static let listAuditRecords: APIOperation<NoRequest, [AuditSummary]> = APIOperation<NoRequest, [AuditSummary]>(method: "GET", path: "api/v1/audit", queryParameters: [:])
    static let listAdminTokens: APIOperation<NoRequest, [AdminTokenSummary]> = APIOperation<NoRequest, [AdminTokenSummary]>(method: "GET", path: "api/v1/admin/tokens", queryParameters: [:])
    static let createAdminToken: APIOperation<CreateAdminTokenRequest, AdminTokenReceipt> = APIOperation<CreateAdminTokenRequest, AdminTokenReceipt>(method: "POST", path: "api/v1/admin/tokens", queryParameters: [:])
    static func revokeAdminToken(id: String) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "DELETE", path: "api/v1/admin/tokens/\(id)", queryParameters: [:])
    }
    static let healthCheck: APIOperation<NoRequest, APIHealth> = APIOperation<NoRequest, APIHealth>(method: "GET", path: "healthz", queryParameters: [:])
    static let readinessCheck: APIOperation<NoRequest, APIHealth> = APIOperation<NoRequest, APIHealth>(method: "GET", path: "readyz", queryParameters: [:])
    static let getOpenAPISpec: APIOperation<NoRequest, NoResponse> = APIOperation<NoRequest, NoResponse>(method: "GET", path: "openapi.yaml", queryParameters: [:])
    static func downloadAutomaticSubscription(token: String, format: String? = nil) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "GET", path: "sub/\(token)", queryParameters: ["format": format.map { String($0) }].compactMapValues { $0 })
    }
    static func downloadSubscription(token: String) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "GET", path: "sub/\(token)/clash.yaml", queryParameters: [:])
    }
    static func authenticateHysteriaUser(nodeID: String, nodeToken: String) -> APIOperation<AuthenticateHysteriaUserRequest, HysteriaAuthResponse> {
        APIOperation(method: "POST", path: "hy2/auth/\(nodeID)/\(nodeToken)", queryParameters: [:])
    }
    static let listDNSConnections: APIOperation<NoRequest, [DNSConnection]> = APIOperation<NoRequest, [DNSConnection]>(method: "GET", path: "api/v1/dns/connections", queryParameters: [:])
    static let createDNSConnection: APIOperation<DNSConnectionCreateRequest, DNSConnection> = APIOperation<DNSConnectionCreateRequest, DNSConnection>(method: "POST", path: "api/v1/dns/connections", queryParameters: [:])
    static func getDNSConnection(id: String) -> APIOperation<NoRequest, DNSConnection> {
        APIOperation(method: "GET", path: "api/v1/dns/connections/\(id)", queryParameters: [:])
    }
    static func updateDNSConnection(id: String) -> APIOperation<DNSConnectionPatchRequest, DNSConnection> {
        APIOperation(method: "PATCH", path: "api/v1/dns/connections/\(id)", queryParameters: [:])
    }
    static func deleteDNSConnection(id: String, expectedRevision: Int) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "DELETE", path: "api/v1/dns/connections/\(id)", queryParameters: ["expected_revision": String(expectedRevision)])
    }
    static func verifyDNSConnection(id: String) -> APIOperation<DNSActionRequest, DNSActionReceipt> {
        APIOperation(method: "POST", path: "api/v1/dns/connections/\(id)/verify", queryParameters: [:])
    }
    static func refreshDNSConnection(id: String) -> APIOperation<DNSActionRequest, DNSActionReceipt> {
        APIOperation(method: "POST", path: "api/v1/dns/connections/\(id)/refresh", queryParameters: [:])
    }
    static let listDNSZones: APIOperation<NoRequest, [DNSZone]> = APIOperation<NoRequest, [DNSZone]>(method: "GET", path: "api/v1/dns/zones", queryParameters: [:])
    static func updateDNSZone(id: String) -> APIOperation<DNSZonePatchRequest, DNSZone> {
        APIOperation(method: "PATCH", path: "api/v1/dns/zones/\(id)", queryParameters: [:])
    }
    static func refreshDNSZone(id: String) -> APIOperation<DNSActionRequest, DNSActionReceipt> {
        APIOperation(method: "POST", path: "api/v1/dns/zones/\(id)/refresh", queryParameters: [:])
    }
    static func listDNSRecords(zoneId: String? = nil) -> APIOperation<NoRequest, [DNSRecord]> {
        APIOperation(method: "GET", path: "api/v1/dns/records", queryParameters: ["zone_id": zoneId.map { String($0) }].compactMapValues { $0 })
    }
    static let createDNSRecord: APIOperation<DNSRecordCreateRequest, DNSActionReceipt> = APIOperation<DNSRecordCreateRequest, DNSActionReceipt>(method: "POST", path: "api/v1/dns/records", queryParameters: [:])
    static func getDNSRecord(id: String) -> APIOperation<NoRequest, DNSRecord> {
        APIOperation(method: "GET", path: "api/v1/dns/records/\(id)", queryParameters: [:])
    }
    static func updateDNSRecord(id: String) -> APIOperation<DNSRecordUpdateRequest, DNSActionReceipt> {
        APIOperation(method: "PATCH", path: "api/v1/dns/records/\(id)", queryParameters: [:])
    }
    static func deleteDNSRecord(id: String) -> APIOperation<DNSActionRequest, DNSActionReceipt> {
        APIOperation(method: "DELETE", path: "api/v1/dns/records/\(id)", queryParameters: [:])
    }
    static func checkDNSRecord(id: String) -> APIOperation<DNSActionRequest, DNSActionReceipt> {
        APIOperation(method: "POST", path: "api/v1/dns/records/\(id)/check", queryParameters: [:])
    }
    static func getDNSBinding(id: String) -> APIOperation<NoRequest, DNSBindingResponse> {
        APIOperation(method: "GET", path: "api/v1/nodes/\(id)/dns-binding", queryParameters: [:])
    }
    static func setDNSBinding(id: String) -> APIOperation<DNSBindingSetRequest, DNSActionReceipt> {
        APIOperation(method: "PUT", path: "api/v1/nodes/\(id)/dns-binding", queryParameters: [:])
    }
    static func removeDNSBinding(id: String) -> APIOperation<DNSBindingRemoveRequest, DNSActionReceipt> {
        APIOperation(method: "DELETE", path: "api/v1/nodes/\(id)/dns-binding", queryParameters: [:])
    }
}
