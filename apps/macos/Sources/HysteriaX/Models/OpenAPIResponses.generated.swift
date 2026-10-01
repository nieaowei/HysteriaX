// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.
import Foundation

struct NodeSummary: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let revision: Int
    let deployedRevision: Int?
    let state: String
    let lastSampleAt: String?
    let dataFreshness: String?
    let openGaps: Int?
    let pendingRevocations: Int?
    let proxyProbeUrl: String?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case deployedRevision = "deployed_revision"
        case state
        case lastSampleAt = "last_sample_at"
        case dataFreshness = "data_freshness"
        case openGaps = "open_gaps"
        case pendingRevocations = "pending_revocations"
        case proxyProbeUrl = "proxy_probe_url"
    }
}

struct NodeSSHDetail: Codable, Sendable {
    let host: String
    let port: Int
    let username: String
    let authType: String
    let hostFingerprint: String?

    enum CodingKeys: String, CodingKey {
        case host
        case port
        case username
        case authType = "auth_type"
        case hostFingerprint = "host_fingerprint"
    }
}

struct NodeConnectionDetail: Codable, Sendable {
    let host: String
    let port: Int
    let listenAddress: String
    let tlsSNI: String?
    let skipCertVerify: Bool

    enum CodingKeys: String, CodingKey {
        case host
        case port
        case listenAddress = "listen_addr"
        case tlsSNI = "tls_sni"
        case skipCertVerify = "skip_cert_verify"
    }
}

struct NodeDetail: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let revision: Int
    let deployedRevision: Int?
    let config: [String: JSONValue]
    let yamlPreview: String
    let ssh: NodeSSHDetail
    let connection: NodeConnectionDetail
    let state: String
    let lastSeenAt: String?
    let lastSampleAt: String?
    let dataFreshness: String?
    let openGaps: Int?
    let pendingRevocations: Int?
    let proxyProbeUrl: String?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case deployedRevision = "deployed_revision"
        case config
        case yamlPreview = "yaml_preview"
        case ssh
        case connection = "public"
        case state
        case lastSeenAt = "last_seen_at"
        case lastSampleAt = "last_sample_at"
        case dataFreshness = "data_freshness"
        case openGaps = "open_gaps"
        case pendingRevocations = "pending_revocations"
        case proxyProbeUrl = "proxy_probe_url"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct AssignmentInfo: Codable, Sendable {
    let nodeID: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case createdAt = "created_at"
    }
}

struct UserSummary: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let enabled: Bool
    let expiresAt: String?
    let quotaBytes: Int64?
    let usageBytes: Int64
    let revision: Int
    let quotaResetAt: String?
    let assignments: [AssignmentInfo]
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case enabled
        case expiresAt = "expires_at"
        case quotaBytes = "quota_bytes"
        case usageBytes = "usage_bytes"
        case revision
        case quotaResetAt = "quota_reset_at"
        case assignments
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct JobOutcome: Codable, Sendable {
    let status: String?
    let fingerprint: String?
    let distribution: String?
    let version: String?
    let architecture: String?
    let privilege: String?
    let proxyProbe: [String: JSONValue]?

    enum CodingKeys: String, CodingKey {
        case status
        case fingerprint
        case distribution
        case version
        case architecture
        case privilege
        case proxyProbe = "proxy_probe"
    }
}

struct JobResult: Codable, Sendable {
    let stage: String?
    let result: JobOutcome?

    enum CodingKeys: String, CodingKey {
        case stage
        case result
    }
}

struct JobSummary: Codable, Sendable, Identifiable {
    let id: String
    let kind: String
    let nodeID: String?
    let targetRevision: Int?
    let status: String
    let stage: String
    let errorMessage: String?
    let result: JobResult?
    let attempts: Int
    let createdAt: String
    let updatedAt: String
    let finishedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case nodeID = "node_id"
        case targetRevision = "target_revision"
        case status
        case stage
        case errorMessage = "error_message"
        case result
        case attempts
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case finishedAt = "finished_at"
    }
}

struct AuditSummary: Codable, Sendable, Identifiable {
    let id: String
    let actor: String
    let action: String
    let entityType: String
    let entityID: String
    let createdAt: String
    let detail: [String: JSONValue]?

    enum CodingKeys: String, CodingKey {
        case id
        case actor
        case action
        case entityType = "entity_type"
        case entityID = "entity_id"
        case createdAt = "created_at"
        case detail
    }
}

struct NodeResource: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let resourceKind: String
    let contentSHA256: String
    let sizeBytes: Int64
    let createdAt: String
    let reference: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case resourceKind = "resource_kind"
        case contentSHA256 = "content_sha256"
        case sizeBytes = "size_bytes"
        case createdAt = "created_at"
        case reference
    }
}

struct ResourceReceipt: Codable, Sendable {
    let id: String
    let resourceKind: String?
    let contentSHA256: String
    let sizeBytes: Int64?
    let reference: String

    enum CodingKeys: String, CodingKey {
        case id
        case resourceKind = "resource_kind"
        case contentSHA256 = "content_sha256"
        case sizeBytes = "size_bytes"
        case reference
    }
}

struct AssignmentReceipt: Codable, Sendable {
    let userId: String
    let nodeID: String
    let revision: Int
    let hy2Credential: String
    let note: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case nodeID = "node_id"
        case revision
        case hy2Credential = "hy2_credential"
        case note
    }
}

struct SubscriptionReceipt: Codable, Sendable {
    let userId: String
    let revision: Int
    let token: String
    let url: String
    let note: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case revision
        case token
        case url
        case note
    }
}

struct AssignmentMutationResponse: Codable, Sendable {
    let userId: String
    let nodeID: String
    let revision: Int

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case nodeID = "node_id"
        case revision
    }
}

struct RotatedCredential: Codable, Sendable {
    let nodeID: String
    let credential: String

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case credential
    }
}

struct RotatedCredentials: Codable, Sendable {
    let userId: String
    let revision: Int
    let credentials: [RotatedCredential]
    let note: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case revision
        case credentials
        case note
    }
}

struct JobReceipt: Codable, Sendable {
    let jobId: String
    let status: String

    enum CodingKeys: String, CodingKey {
        case jobId = "job_id"
        case status
    }
}

struct NodeUpdateResponse: Codable, Sendable {
    let id: String
    let name: String
    let revision: Int
    let state: String
    let syncJobQueued: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case state
        case syncJobQueued = "sync_job_queued"
    }
}

struct UserUpdateResponse: Codable, Sendable {
    let id: String
    let name: String
    let enabled: Bool
    let expiresAt: String?
    let quotaBytes: Int64?
    let revision: Int

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case enabled
        case expiresAt = "expires_at"
        case quotaBytes = "quota_bytes"
        case revision
    }
}

struct QuotaResetResponse: Codable, Sendable {
    let userId: String
    let usageBytes: Int64
    let quotaResetAt: String
    let revision: Int

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case usageBytes = "usage_bytes"
        case quotaResetAt = "quota_reset_at"
        case revision
    }
}

struct ActiveSubscription: Codable, Sendable {
    let id: String
    let token: String
    let url: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case token
        case url
        case createdAt = "created_at"
    }
}

struct SubscriptionStatus: Codable, Sendable {
    let userId: String
    let revision: Int
    let active: ActiveSubscription?

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case revision
        case active
    }
}

struct NodeUsageSummary: Codable, Sendable {
    let nodeID: String
    let txBytes: Int64
    let rxBytes: Int64
    let sampledAt: String?
    let assigned: Bool?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case txBytes = "tx_bytes"
        case rxBytes = "rx_bytes"
        case sampledAt = "sampled_at"
        case assigned
    }
}

struct DataFreshness: Codable, Sendable {
    let status: String
    let lastSampleAt: String?
    let openGaps: Int

    enum CodingKeys: String, CodingKey {
        case status
        case lastSampleAt = "last_sample_at"
        case openGaps = "open_gaps"
    }
}

struct PendingRevocation: Codable, Sendable {
    let jobId: String
    let nodeID: String
    let status: String
    let stage: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case jobId = "job_id"
        case nodeID = "node_id"
        case status
        case stage
        case updatedAt = "updated_at"
    }
}

struct UserUsageResponse: Codable, Sendable {
    let userId: String
    let name: String
    let usageBytes: Int64
    let quotaBytes: Int64?
    let quotaResetAt: String?
    let byNode: [NodeUsageSummary]
    let dataFreshness: DataFreshness
    let pendingRevocations: [PendingRevocation]?

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case name
        case usageBytes = "usage_bytes"
        case quotaBytes = "quota_bytes"
        case quotaResetAt = "quota_reset_at"
        case byNode = "by_node"
        case dataFreshness = "data_freshness"
        case pendingRevocations = "pending_revocations"
    }
}

struct JobDetailResponse: Codable, Sendable {
    let job: JobSummary
    let result: [String: JSONValue]?
    let logs: [[String: JSONValue]]
    let startedAt: String?

    enum CodingKeys: String, CodingKey {
        case job
        case result
        case logs
        case startedAt = "started_at"
    }
}

struct HysteriaAuthResponse: Codable, Sendable {
    let ok: Bool
    let id: String

    enum CodingKeys: String, CodingKey {
        case ok
        case id
    }
}

struct CreatedEntity: Codable, Sendable {
    let id: String?
    let revision: Int?
    let nodeAuthToken: String?
    let node: NodeReceipt?

    enum CodingKeys: String, CodingKey {
        case id
        case revision
        case nodeAuthToken = "node_auth_token"
        case node
    }
}

struct NodeReceipt: Codable, Sendable {
    let id: String
    let name: String
    let revision: Int
    let state: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case state
    }
}

struct APIHealth: Codable, Sendable {
    let status: String
    let database: String?

    enum CodingKeys: String, CodingKey {
        case status
        case database
    }
}

struct APIVersion: Codable, Sendable {
    let apiVersion: String
    let serviceVersion: String
    let hysteriaVersion: String
    let mihomoVersion: String

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case serviceVersion = "service_version"
        case hysteriaVersion = "hysteria_version"
        case mihomoVersion = "mihomo_version"
    }
}

struct APIErrorDetail: Codable, Sendable {
    let code: String
    let message: String

    enum CodingKeys: String, CodingKey {
        case code
        case message
    }
}

struct APIErrorResponse: Codable, Sendable {
    let error: APIErrorDetail

    enum CodingKeys: String, CodingKey {
        case error
    }
}

struct AdminTokenSummary: Codable, Sendable, Identifiable {
    let id: String
    let label: String
    let createdAt: String
    let lastUsedAt: String?
    let revokedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case label
        case createdAt = "created_at"
        case lastUsedAt = "last_used_at"
        case revokedAt = "revoked_at"
    }
}

struct AdminTokenReceipt: Codable, Sendable {
    let id: String
    let label: String
    let token: String

    enum CodingKeys: String, CodingKey {
        case id
        case label
        case token
    }
}
