// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.
import Foundation

struct CredentialSummary: Codable, Sendable, Identifiable {
    let referenceCount: Int?
    let id: String
    let name: String
    let kind: String
    let ownerUserId: String?
    let revision: Int
    let latestVersion: Int
    let archived: Bool
    let reminderAt: String?
    let expiresAt: String?
    let daysRemaining: Int?
    let status: String
    let metadata: [String: JSONValue]
    let createdAt: String
    let updatedAt: String?

    enum CodingKeys: String, CodingKey {
        case referenceCount = "reference_count"
        case id
        case name
        case kind
        case ownerUserId = "owner_user_id"
        case revision
        case latestVersion = "latest_version"
        case archived
        case reminderAt = "reminder_at"
        case expiresAt = "expires_at"
        case daysRemaining = "days_remaining"
        case status
        case metadata
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct CredentialDetail: Codable, Sendable, Identifiable {
    let referenceCount: Int?
    let id: String
    let name: String
    let kind: String
    let ownerUserId: String?
    let revision: Int
    let latestVersion: Int
    let archived: Bool
    let reminderAt: String?
    let expiresAt: String?
    let daysRemaining: Int?
    let status: String
    let metadata: [String: JSONValue]
    let createdAt: String
    let updatedAt: String?
    let versions: [CredentialVersion]
    let references: [CredentialReference]
    let batches: [CredentialBatch]

    enum CodingKeys: String, CodingKey {
        case referenceCount = "reference_count"
        case id
        case name
        case kind
        case ownerUserId = "owner_user_id"
        case revision
        case latestVersion = "latest_version"
        case archived
        case reminderAt = "reminder_at"
        case expiresAt = "expires_at"
        case daysRemaining = "days_remaining"
        case status
        case metadata
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case versions
        case references
        case batches
    }
}

struct CredentialVersion: Codable, Sendable {
    let version: Int
    let metadata: [String: JSONValue]
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case version
        case metadata
        case createdAt = "created_at"
    }
}

struct CredentialReference: Codable, Sendable {
    let entityType: String
    let entityID: String
    let source: String
    let name: String?
    let version: Int?
    let field: String?
    let nodeID: String?
    let configRevision: Int?

    enum CodingKeys: String, CodingKey {
        case entityType = "entity_type"
        case entityID = "entity_id"
        case source
        case name
        case version
        case field
        case nodeID = "node_id"
        case configRevision = "config_revision"
    }
}

struct CredentialReceipt: Codable, Sendable {
    let id: String
    let revision: Int
    let version: Int?
    let batchId: String?
    let affectedCount: Int?

    enum CodingKeys: String, CodingKey {
        case id
        case revision
        case version
        case batchId = "batch_id"
        case affectedCount = "affected_count"
    }
}

struct CredentialBatch: Codable, Sendable, Identifiable {
    let id: String
    let credentialId: String
    let version: Int
    let createdAt: String
    let items: [CredentialBatchItem]

    enum CodingKeys: String, CodingKey {
        case id
        case credentialId = "credential_id"
        case version
        case createdAt = "created_at"
        case items
    }
}

struct CredentialBatchItem: Codable, Sendable {
    let nodeID: String
    let userId: String
    let jobId: String
    let status: String
    let stage: String
    let errorMessage: String?
    let name: String?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case userId = "user_id"
        case jobId = "job_id"
        case status
        case stage
        case errorMessage = "error_message"
        case name
    }
}

struct CredentialBatchReceipt: Codable, Sendable {
    let batchId: String
    let affectedCount: Int

    enum CodingKeys: String, CodingKey {
        case batchId = "batch_id"
        case affectedCount = "affected_count"
    }
}

struct NodePackage: Codable, Sendable {
    let expiresAt: String?
    let quotaBytes: Int64?
    let cycle: String
    let resetDay: Int
    let timezone: String
    let interface: String?
    let direction: String
    let expiryWarningDays: Int
    let trafficWarningPercent: Int

    enum CodingKeys: String, CodingKey {
        case expiresAt = "expires_at"
        case quotaBytes = "quota_bytes"
        case cycle
        case resetDay = "reset_day"
        case timezone
        case interface
        case direction
        case expiryWarningDays = "expiry_warning_days"
        case trafficWarningPercent = "traffic_warning_percent"
    }
}

struct NodePackageUsage: Codable, Sendable {
    let usageBytes: Int64
    let restricted: Bool
    let reasons: [String]
    let nextResetAt: String?
    let interface: String?
    let sampledAt: String?
    let gapReason: String?
    let freshness: String
    let pendingDisconnects: Int?
    let failedDisconnects: Int?
    let alerts: [NodeAlert]

    enum CodingKeys: String, CodingKey {
        case usageBytes = "usage_bytes"
        case restricted
        case reasons
        case nextResetAt = "next_reset_at"
        case interface
        case sampledAt = "sampled_at"
        case gapReason = "gap_reason"
        case freshness
        case pendingDisconnects = "pending_disconnects"
        case failedDisconnects = "failed_disconnects"
        case alerts
    }
}

struct NodeAlert: Codable, Sendable {
    let id: String
    let kind: String
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case createdAt = "created_at"
    }
}

struct NodeUsageUpdateResponse: Codable, Sendable {
    let id: String
    let revision: Int

    enum CodingKeys: String, CodingKey {
        case id
        case revision
    }
}

struct NodeSummary: Codable, Sendable, Identifiable {
    let package: NodePackage?
    let packageUsage: NodePackageUsage?
    let id: String
    let name: String
    let ssh: NodeSSHDetail?
    let revision: Int
    let deployedRevision: Int?
    let state: String
    let lastSampleAt: String?
    let dataFreshness: String?
    let openGaps: Int?
    let pendingRevocations: Int?
    let proxyProbeUrl: String?

    enum CodingKeys: String, CodingKey {
        case package
        case packageUsage = "package_usage"
        case id
        case name
        case ssh
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
    let credentialId: String
    let credentialVersion: Int

    enum CodingKeys: String, CodingKey {
        case host
        case port
        case username
        case authType = "auth_type"
        case hostFingerprint = "host_fingerprint"
        case credentialId = "credential_id"
        case credentialVersion = "credential_version"
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
    let package: NodePackage?
    let packageUsage: NodePackageUsage?
    let id: String
    let name: String
    let revision: Int
    let deployedRevision: Int?
    let config: [String: JSONValue]
    let yamlPreview: String
    let ssh: NodeSSHDetail
    let connection: NodeConnectionDetail
    let trafficStatsPort: Int?
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
        case package
        case packageUsage = "package_usage"
        case id
        case name
        case revision
        case deployedRevision = "deployed_revision"
        case config
        case yamlPreview = "yaml_preview"
        case ssh
        case connection = "public"
        case trafficStatsPort = "traffic_stats_port"
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
    let mtlsCredentialId: String?
    let mtlsCredentialVersion: Int?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case createdAt = "created_at"
        case mtlsCredentialId = "mtls_credential_id"
        case mtlsCredentialVersion = "mtls_credential_version"
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
    let nodeName: String?
    let retryOfJobId: String?
    let retryJobId: String?
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
        case nodeName = "node_name"
        case retryOfJobId = "retry_of_job_id"
        case retryJobId = "retry_job_id"
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
    let autoUrl: String?
    let note: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case revision
        case token
        case url
        case autoUrl = "auto_url"
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
    let trafficStatsPort: Int?
    let syncJobQueued: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case state
        case trafficStatsPort = "traffic_stats_port"
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
    let autoUrl: String?
    let createdAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case token
        case url
        case autoUrl = "auto_url"
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
    let trafficStatsPort: Int?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case state
        case trafficStatsPort = "traffic_stats_port"
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
    let currentAdminTokenId: String?
    let features: [String]?
    let apiVersion: String
    let serviceVersion: String
    let hysteriaVersion: String
    let mihomoVersion: String

    enum CodingKeys: String, CodingKey {
        case currentAdminTokenId = "current_admin_token_id"
        case features
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

struct ServerMonitoring: Codable, Sendable {
    let serviceVersion: String
    let serviceUptimeSeconds: Int
    let database: String
    let sampledAt: String
    let hostname: String?
    let os: String?
    let hostUptimeSeconds: Int
    let cpuCount: Int
    let cpuUsagePercent: Double
    let memoryUsedBytes: Int
    let memoryTotalBytes: Int
    let rootDiskUsedBytes: Int?
    let rootDiskTotalBytes: Int?

    enum CodingKeys: String, CodingKey {
        case serviceVersion = "service_version"
        case serviceUptimeSeconds = "service_uptime_seconds"
        case database
        case sampledAt = "sampled_at"
        case hostname
        case os
        case hostUptimeSeconds = "host_uptime_seconds"
        case cpuCount = "cpu_count"
        case cpuUsagePercent = "cpu_usage_percent"
        case memoryUsedBytes = "memory_used_bytes"
        case memoryTotalBytes = "memory_total_bytes"
        case rootDiskUsedBytes = "root_disk_used_bytes"
        case rootDiskTotalBytes = "root_disk_total_bytes"
    }
}

struct OverviewIssue: Codable, Sendable {
    let id: String
    let entityType: String
    let entityID: String
    let name: String
    let kind: String
    let severity: Int64
    let reason: String
    let occurredAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case entityType = "entity_type"
        case entityID = "entity_id"
        case name
        case kind
        case severity
        case reason
        case occurredAt = "occurred_at"
    }
}

struct OverviewNode: Codable, Sendable {
    let nodeID: String
    let name: String
    let deploymentState: String
    let onlineStatus: String
    let onlineUsers: Int64?
    let connections: Int64?
    let onlineSampledAt: String?
    let probeStatus: String
    let inletStatus: String
    let probeSampledAt: String?
    let latencyMs: Double?
    let connectionMs: Double?
    let externalStatus: String?
    let reason: String?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case name
        case deploymentState = "deployment_state"
        case onlineStatus = "online_status"
        case onlineUsers = "online_users"
        case connections
        case onlineSampledAt = "online_sampled_at"
        case probeStatus = "probe_status"
        case inletStatus = "inlet_status"
        case probeSampledAt = "probe_sampled_at"
        case latencyMs = "latency_ms"
        case connectionMs = "connection_ms"
        case externalStatus = "external_status"
        case reason
    }
}

struct OverviewResponse: Codable, Sendable {
    let generatedAt: String
    let nodeCount: Int64
    let nodeStates: [String: JSONValue]
    let attentionNodes: Int64
    let riskNodes: Int64
    let queuedJobs: Int64
    let runningJobs: Int64
    let failedJobs24h: Int64
    let onlineUsers: Int64?
    let connections: Int64?
    let eligibleNodes: Int64
    let coveredNodes: Int64
    let issues: [OverviewIssue]
    let nodes: [OverviewNode]
    let quotaRank: [NodeSummary]

    enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at"
        case nodeCount = "node_count"
        case nodeStates = "node_states"
        case attentionNodes = "attention_nodes"
        case riskNodes = "risk_nodes"
        case queuedJobs = "queued_jobs"
        case runningJobs = "running_jobs"
        case failedJobs24h = "failed_jobs_24h"
        case onlineUsers = "online_users"
        case connections
        case eligibleNodes = "eligible_nodes"
        case coveredNodes = "covered_nodes"
        case issues
        case nodes
        case quotaRank = "quota_rank"
    }
}

struct OverviewBucket: Codable, Sendable {
    let start: String
    let end: String
    let txBytes: Int64?
    let rxBytes: Int64?
    let incomplete: Bool
    let trafficCoveredNodes: Int64
    let trafficExpectedNodes: Int64
    let onlineIncomplete: Bool
    let missingReason: String?
    let onlineUsersAvg: Double?
    let onlineUsersPeak: Double?
    let connectionsAvg: Double?
    let connectionsPeak: Double?
    let coveredNodes: Int64
    let probeAttempts: Int64
    let probeSuccesses: Int64
    let latencyP50Ms: Double?
    let latencyP95Ms: Double?

    enum CodingKeys: String, CodingKey {
        case start
        case end
        case txBytes = "tx_bytes"
        case rxBytes = "rx_bytes"
        case incomplete
        case trafficCoveredNodes = "traffic_covered_nodes"
        case trafficExpectedNodes = "traffic_expected_nodes"
        case onlineIncomplete = "online_incomplete"
        case missingReason = "missing_reason"
        case onlineUsersAvg = "online_users_avg"
        case onlineUsersPeak = "online_users_peak"
        case connectionsAvg = "connections_avg"
        case connectionsPeak = "connections_peak"
        case coveredNodes = "covered_nodes"
        case probeAttempts = "probe_attempts"
        case probeSuccesses = "probe_successes"
        case latencyP50Ms = "latency_p50_ms"
        case latencyP95Ms = "latency_p95_ms"
    }
}

struct OverviewHistory: Codable, Sendable {
    let generatedAt: String
    let range: String
    let timezone: String
    let source: String
    let buckets: [OverviewBucket]

    enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at"
        case range
        case timezone
        case source
        case buckets
    }
}
