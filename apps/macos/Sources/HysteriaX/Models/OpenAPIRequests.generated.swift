// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.
import Foundation

struct NodeCreateRequest: Encodable, Sendable {
    let name: String
    let sshHost: String
    let sshPort: Int
    let sshUsername: String
    let sshAuthType: String
    let sshSecret: String
    let sshPassphrase: String?
    let sshHostFingerprint: String?
    let publicHost: String
    let publicPort: Int
    let listenAddr: String
    let trafficStatsPort: Int
    let proxyProbeUrl: String?
    let tlsSNI: String?
    let tlsSkipVerify: Bool
    let config: [String: JSONValue]

    init(
        name: String,
        sshHost: String,
        sshPort: Int,
        sshUsername: String,
        sshAuthType: String,
        sshSecret: String,
        sshPassphrase: String? = nil,
        sshHostFingerprint: String? = nil,
        publicHost: String,
        publicPort: Int,
        listenAddr: String,
        trafficStatsPort: Int = 9780,
        proxyProbeUrl: String? = nil,
        tlsSNI: String? = nil,
        tlsSkipVerify: Bool = false,
        config: [String: JSONValue] = [:]
    ) {
        self.name = name
        self.sshHost = sshHost
        self.sshPort = sshPort
        self.sshUsername = sshUsername
        self.sshAuthType = sshAuthType
        self.sshSecret = sshSecret
        self.sshPassphrase = sshPassphrase
        self.sshHostFingerprint = sshHostFingerprint
        self.publicHost = publicHost
        self.publicPort = publicPort
        self.listenAddr = listenAddr
        self.trafficStatsPort = trafficStatsPort
        self.proxyProbeUrl = proxyProbeUrl
        self.tlsSNI = tlsSNI
        self.tlsSkipVerify = tlsSkipVerify
        self.config = config
    }

    enum CodingKeys: String, CodingKey {
        case name
        case sshHost = "ssh_host"
        case sshPort = "ssh_port"
        case sshUsername = "ssh_username"
        case sshAuthType = "ssh_auth_type"
        case sshSecret = "ssh_secret"
        case sshPassphrase = "ssh_passphrase"
        case sshHostFingerprint = "ssh_host_fingerprint"
        case publicHost = "public_host"
        case publicPort = "public_port"
        case listenAddr = "listen_addr"
        case trafficStatsPort = "traffic_stats_port"
        case proxyProbeUrl = "proxy_probe_url"
        case tlsSNI = "tls_sni"
        case tlsSkipVerify = "tls_skip_verify"
        case config
    }
}

struct NodePatchRequest: Encodable, Sendable {
    let expectedRevision: Int
    let name: String?
    let sshHost: String?
    let sshPort: Int?
    let sshUsername: String?
    let sshAuthType: String?
    let sshSecret: String?
    let sshPassphrase: String?
    let sshHostFingerprint: String?
    let publicHost: String?
    let publicPort: Int?
    let listenAddr: String?
    let trafficStatsPort: Int?
    let proxyProbeUrl: String?
    let tlsSNI: String?
    let tlsSkipVerify: Bool?
    let config: [String: JSONValue]?

    init(
        expectedRevision: Int,
        name: String? = nil,
        sshHost: String? = nil,
        sshPort: Int? = nil,
        sshUsername: String? = nil,
        sshAuthType: String? = nil,
        sshSecret: String? = nil,
        sshPassphrase: String? = nil,
        sshHostFingerprint: String? = nil,
        publicHost: String? = nil,
        publicPort: Int? = nil,
        listenAddr: String? = nil,
        trafficStatsPort: Int? = nil,
        proxyProbeUrl: String? = nil,
        tlsSNI: String? = nil,
        tlsSkipVerify: Bool? = nil,
        config: [String: JSONValue]? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.name = name
        self.sshHost = sshHost
        self.sshPort = sshPort
        self.sshUsername = sshUsername
        self.sshAuthType = sshAuthType
        self.sshSecret = sshSecret
        self.sshPassphrase = sshPassphrase
        self.sshHostFingerprint = sshHostFingerprint
        self.publicHost = publicHost
        self.publicPort = publicPort
        self.listenAddr = listenAddr
        self.trafficStatsPort = trafficStatsPort
        self.proxyProbeUrl = proxyProbeUrl
        self.tlsSNI = tlsSNI
        self.tlsSkipVerify = tlsSkipVerify
        self.config = config
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case name
        case sshHost = "ssh_host"
        case sshPort = "ssh_port"
        case sshUsername = "ssh_username"
        case sshAuthType = "ssh_auth_type"
        case sshSecret = "ssh_secret"
        case sshPassphrase = "ssh_passphrase"
        case sshHostFingerprint = "ssh_host_fingerprint"
        case publicHost = "public_host"
        case publicPort = "public_port"
        case listenAddr = "listen_addr"
        case trafficStatsPort = "traffic_stats_port"
        case proxyProbeUrl = "proxy_probe_url"
        case tlsSNI = "tls_sni"
        case tlsSkipVerify = "tls_skip_verify"
        case config
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
    let clientCertificate: String?
    let clientPrivateKey: String?

    init(
        expectedRevision: Int,
        nodeID: String,
        clientCertificate: String? = nil,
        clientPrivateKey: String? = nil
    ) {
        self.expectedRevision = expectedRevision
        self.nodeID = nodeID
        self.clientCertificate = clientCertificate
        self.clientPrivateKey = clientPrivateKey
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case nodeID = "node_id"
        case clientCertificate = "client_certificate"
        case clientPrivateKey = "client_private_key"
    }
}

struct AssignmentCertificateUpdateRequest: Encodable, Sendable {
    let expectedRevision: Int
    let clientCertificate: String
    let clientPrivateKey: String

    init(
        expectedRevision: Int,
        clientCertificate: String,
        clientPrivateKey: String
    ) {
        self.expectedRevision = expectedRevision
        self.clientCertificate = clientCertificate
        self.clientPrivateKey = clientPrivateKey
    }

    enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case clientCertificate = "client_certificate"
        case clientPrivateKey = "client_private_key"
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
    static func downloadSubscription(token: String) -> APIOperation<NoRequest, NoResponse> {
        APIOperation(method: "GET", path: "sub/\(token)/clash.yaml", queryParameters: [:])
    }
    static func authenticateHysteriaUser(nodeID: String, nodeToken: String) -> APIOperation<AuthenticateHysteriaUserRequest, HysteriaAuthResponse> {
        APIOperation(method: "POST", path: "hy2/auth/\(nodeID)/\(nodeToken)", queryParameters: [:])
    }
}
