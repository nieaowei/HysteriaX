// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.
import Foundation

struct SubscriptionTemplate: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let format: String
    let revision: Int
    let archived: Bool
    let isDefault: Bool
    let publishedVersion: Int?
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case format
        case revision
        case archived
        case isDefault = "is_default"
        case publishedVersion = "published_version"
        case updatedAt = "updated_at"
    }
}

struct SubscriptionTemplatesPage: Codable, Sendable {
    let items: [SubscriptionTemplate]
    let page: Int
    let pageSize: Int
    let total: Int

    enum CodingKeys: String, CodingKey {
        case items
        case page
        case pageSize = "page_size"
        case total
    }
}

struct SubscriptionTemplateDraft: Codable, Sendable {
    let template: SubscriptionTemplate
    let document: [String: JSONValue]

    enum CodingKeys: String, CodingKey {
        case template
        case document
    }
}

struct ManagedUserIdentity: Codable, Sendable {
    let identityId: String
    let caCredentialId: String
    let caName: String
    let revision: Int
    let authorizationRevision: Int
    let credentialId: String?
    let archived: Bool
    let maintenanceError: String?
    let nextActionAt: String?
    let publishedCaVersion: Int
    let caExpiresAt: String
    let policyRevision: Int
    let versions: [ManagedIdentityVersion]
    let nodeIds: [String]

    enum CodingKeys: String, CodingKey {
        case identityId = "identity_id"
        case caCredentialId = "ca_credential_id"
        case caName = "ca_name"
        case revision
        case authorizationRevision = "authorization_revision"
        case credentialId = "credential_id"
        case archived
        case maintenanceError = "maintenance_error"
        case nextActionAt = "next_action_at"
        case publishedCaVersion = "published_ca_version"
        case caExpiresAt = "ca_expires_at"
        case policyRevision = "policy_revision"
        case versions
        case nodeIds = "node_ids"
    }
}

struct ManagedIdentityVersion: Codable, Sendable {
    let version: Int
    let caVersion: Int
    let notBefore: String
    let expiresAt: String
    let released: Bool
    let discarded: Bool

    enum CodingKeys: String, CodingKey {
        case version
        case caVersion = "ca_version"
        case notBefore = "not_before"
        case expiresAt = "expires_at"
        case released
        case discarded
    }
}

struct ManagedCAPolicy: Codable, Sendable {
    let clientValidityDays: Int
    let renewBeforeHours: Int

    enum CodingKeys: String, CodingKey {
        case clientValidityDays = "client_validity_days"
        case renewBeforeHours = "renew_before_hours"
    }
}

struct CAIdentityReceipt: Codable, Sendable {
    let id: String
    let revision: Int
    let version: Int
    let policyRevision: Int?
    let batchId: String?
    let rotationId: String?
    let published: Bool?

    enum CodingKeys: String, CodingKey {
        case id
        case revision
        case version
        case policyRevision = "policy_revision"
        case batchId = "batch_id"
        case rotationId = "rotation_id"
        case published
    }
}

struct CAPolicyReceipt: Codable, Sendable {
    let id: String
    let policyRevision: Int
    let policy: ManagedCAPolicy

    enum CodingKeys: String, CodingKey {
        case id
        case policyRevision = "policy_revision"
        case policy
    }
}

struct CAIdentityState: Codable, Sendable {
    let source: String
    let policyRevision: Int
    let publishedVersion: Int
    let clientValidityDays: Int
    let renewBeforeHours: Int
    let cleanup: [CATrustCleanup]?
    let maintenanceError: String?

    enum CodingKeys: String, CodingKey {
        case source
        case policyRevision = "policy_revision"
        case publishedVersion = "published_version"
        case clientValidityDays = "client_validity_days"
        case renewBeforeHours = "renew_before_hours"
        case cleanup
        case maintenanceError = "maintenance_error"
    }
}

struct ManagedUserReissueReceipt: Codable, Sendable {
    let userId: String
    let identities: [ManagedIdentityReceipt]
    let refreshSubscription: Bool

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case identities
        case refreshSubscription = "refresh_subscription"
    }
}

struct ManagedIdentityReceipt: Codable, Sendable {
    let caCredentialId: String
    let identityId: String
    let credentialVersion: Int?
    let issued: Bool
    let affectedNodeIds: [String]

    enum CodingKeys: String, CodingKey {
        case caCredentialId = "ca_credential_id"
        case identityId = "identity_id"
        case credentialVersion = "credential_version"
        case issued
        case affectedNodeIds = "affected_node_ids"
    }
}

struct CARotationState: Codable, Sendable {
    let id: String
    let version: Int
    let phase: String
    let errorMessage: String?
    let nextActionAt: String
    let outcome: String?
    let publishedAt: String?
    let status: CARotationStatus
    let nodes: [CARotationNode]

    enum CodingKeys: String, CodingKey {
        case id
        case version
        case phase
        case errorMessage = "error_message"
        case nextActionAt = "next_action_at"
        case outcome
        case publishedAt = "published_at"
        case status
        case nodes
    }
}

struct CARotationStatus: Codable, Sendable {
    let previousVersion: Int
    let oldCaExpiresAt: String
    let clientCertificateExpiresAt: String?
    let probeCertificateExpiresAt: String?
    let deadline: String
    let daysRemaining: Int
    let blockedNodeIds: [String]
    let eligibleUserCount: Int
    let preparedUserCount: Int

    enum CodingKeys: String, CodingKey {
        case previousVersion = "previous_version"
        case oldCaExpiresAt = "old_ca_expires_at"
        case clientCertificateExpiresAt = "client_certificate_expires_at"
        case probeCertificateExpiresAt = "probe_certificate_expires_at"
        case deadline
        case daysRemaining = "days_remaining"
        case blockedNodeIds = "blocked_node_ids"
        case eligibleUserCount = "eligible_user_count"
        case preparedUserCount = "prepared_user_count"
    }
}

struct CARotationNode: Codable, Sendable {
    let nodeID: String
    let name: String?
    let trustPrepared: Bool
    let probeSwitched: Bool
    let jobId: String?
    let status: String?
    let stage: String?
    let errorMessage: String?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case name
        case trustPrepared = "trust_prepared"
        case probeSwitched = "probe_switched"
        case jobId = "job_id"
        case status
        case stage
        case errorMessage = "error_message"
    }
}

struct CATrustCleanup: Codable, Sendable {
    let version: Int
    let nextActionAt: String
    let completedAt: String?
    let nodes: [CATrustCleanupNode]

    enum CodingKeys: String, CodingKey {
        case version
        case nextActionAt = "next_action_at"
        case completedAt = "completed_at"
        case nodes
    }
}

struct CATrustCleanupNode: Codable, Sendable {
    let nodeID: String
    let name: String?
    let completed: Bool
    let jobId: String?
    let status: String?
    let stage: String?
    let errorMessage: String?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case name
        case completed
        case jobId = "job_id"
        case status
        case stage
        case errorMessage = "error_message"
    }
}

struct AuthorizationGroupReference: Codable, Sendable {
    let id: String
    let name: String
    let revision: Int

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
    }
}

struct AuthorizationGroupSource: Codable, Sendable {
    let id: String
    let name: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
    }
}

struct AuthorizationGroupSummary: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let revision: Int
    let userIds: [String]
    let nodeIds: [String]
    let userCount: Int
    let nodeCount: Int
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case userIds = "user_ids"
        case nodeIds = "node_ids"
        case userCount = "user_count"
        case nodeCount = "node_count"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct AuthorizationPair: Codable, Sendable {
    let userId: String
    let nodeID: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case nodeID = "node_id"
    }
}

struct AuthorizationManagedIdentity: Codable, Sendable {
    let policyRevision: Int?
    let userId: String
    let caCredentialId: String
    let caVersions: [Int]
    let issueNow: Bool
    let pending: Bool

    enum CodingKeys: String, CodingKey {
        case policyRevision = "policy_revision"
        case userId = "user_id"
        case caCredentialId = "ca_credential_id"
        case caVersions = "ca_versions"
        case issueNow = "issue_now"
        case pending
    }
}

struct AuthorizationIdentityResult: Codable, Sendable {
    let policyRevision: Int?
    let caVersions: [Int]?
    let affectedNodeIds: [String]?
    let userId: String
    let caCredentialId: String
    let identityId: String?
    let issued: Bool
    let pending: Bool

    enum CodingKeys: String, CodingKey {
        case policyRevision = "policy_revision"
        case caVersions = "ca_versions"
        case affectedNodeIds = "affected_node_ids"
        case userId = "user_id"
        case caCredentialId = "ca_credential_id"
        case identityId = "identity_id"
        case issued
        case pending
    }
}

struct AuthorizationChangePreview: Codable, Sendable {
    let automaticIssueCount: Int
    let pendingIssueCount: Int
    let managedIdentities: [AuthorizationManagedIdentity]
    let action: String
    let previewToken: String
    let additionsCount: Int
    let removalsCount: Int
    let additions: [AuthorizationPair]
    let removals: [AuthorizationPair]

    enum CodingKeys: String, CodingKey {
        case automaticIssueCount = "automatic_issue_count"
        case pendingIssueCount = "pending_issue_count"
        case managedIdentities = "managed_identities"
        case action
        case previewToken = "preview_token"
        case additionsCount = "additions_count"
        case removalsCount = "removals_count"
        case additions
        case removals
    }
}

struct AuthorizationCreatedCredential: Codable, Sendable {
    let userId: String
    let nodeID: String
    let hy2Credential: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case nodeID = "node_id"
        case hy2Credential = "hy2_credential"
    }
}

struct AuthorizationGroupMutationResponse: Codable, Sendable {
    let managedIdentities: [AuthorizationIdentityResult]?
    let group: AuthorizationGroupSummary?
    let additionsCount: Int
    let removalsCount: Int
    let createdCredentials: [AuthorizationCreatedCredential]
    let revocationJobIds: [String]

    enum CodingKeys: String, CodingKey {
        case managedIdentities = "managed_identities"
        case group
        case additionsCount = "additions_count"
        case removalsCount = "removals_count"
        case createdCredentials = "created_credentials"
        case revocationJobIds = "revocation_job_ids"
    }
}

struct UserAuthorizationGroupsMutationResponse: Codable, Sendable {
    let managedIdentities: [AuthorizationIdentityResult]?
    let userId: String
    let revision: Int
    let groupIds: [String]
    let assignments: [AssignmentInfo]
    let createdCredentials: [AuthorizationCreatedCredential]
    let revocationJobIds: [String]

    enum CodingKeys: String, CodingKey {
        case managedIdentities = "managed_identities"
        case userId = "user_id"
        case revision
        case groupIds = "group_ids"
        case assignments
        case createdCredentials = "created_credentials"
        case revocationJobIds = "revocation_job_ids"
    }
}

struct DNSConnection: Codable, Sendable, Identifiable {
    let id: String
    let name: String
    let provider: String
    let credentialId: String
    let credentialVersion: Int
    let revision: Int
    let status: String
    let verifiedAt: String?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case provider
        case credentialId = "credential_id"
        case credentialVersion = "credential_version"
        case revision
        case status
        case verifiedAt = "verified_at"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct DNSZone: Codable, Sendable, Identifiable {
    let id: String
    let connectionId: String
    let providerZoneId: String
    let name: String
    let enabled: Bool
    let revision: Int
    let syncedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case connectionId = "connection_id"
        case providerZoneId = "provider_zone_id"
        case name
        case enabled
        case revision
        case syncedAt = "synced_at"
    }
}

struct DNSRecord: Codable, Sendable, Identifiable {
    let id: String
    let zoneId: String
    let providerRecordId: String?
    let name: String
    let recordType: String
    let content: String
    let ttl: Int
    let proxied: Bool
    let origin: String
    let revision: Int
    let remoteSnapshot: [String: JSONValue]?
    let desired: [String: JSONValue]?
    let state: String
    let resolutionStatus: String
    let resolutionDetail: [String: JSONValue]?
    let boundNodeId: String?
    let checkedAt: String?
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case zoneId = "zone_id"
        case providerRecordId = "provider_record_id"
        case name
        case recordType = "record_type"
        case content
        case ttl
        case proxied
        case origin
        case revision
        case remoteSnapshot = "remote_snapshot"
        case desired
        case state
        case resolutionStatus = "resolution_status"
        case resolutionDetail = "resolution_detail"
        case boundNodeId = "bound_node_id"
        case checkedAt = "checked_at"
        case updatedAt = "updated_at"
    }
}

struct DNSRecordsPage: Codable, Sendable {
    let items: [DNSRecord]
    let total: Int
    let page: Int
    let pageSize: Int

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
    }
}

struct PublishedConnection: Codable, Sendable {
    let publicHost: String
    let publicPort: Int
    let listenAddress: String
    let tlsSNI: String?
    let tlsSkipVerify: Bool

    enum CodingKeys: String, CodingKey {
        case publicHost = "public_host"
        case publicPort = "public_port"
        case listenAddress = "listen_addr"
        case tlsSNI = "tls_sni"
        case tlsSkipVerify = "tls_skip_verify"
    }
}

struct DNSBinding: Codable, Sendable {
    let nodeID: String
    let zoneId: String
    let hostname: String
    let recordIds: [String]
    let records: [DNSRecord]
    let revision: Int
    let publishedConnection: PublishedConnection?

    enum CodingKeys: String, CodingKey {
        case nodeID = "node_id"
        case zoneId = "zone_id"
        case hostname
        case recordIds = "record_ids"
        case records
        case revision
        case publishedConnection = "published_connection"
    }
}

struct DNSBindingResponse: Codable, Sendable {
    let binding: DNSBinding?

    enum CodingKeys: String, CodingKey {
        case binding
    }
}

struct DNSActionReceipt: Codable, Sendable {
    let operationId: String?
    let jobId: String?
    let resourceId: String?
    let nodeID: String?
    let hostname: String?
    let recordIds: [String]?
    let jobIds: [String]?
    let recordsRetained: Bool?

    enum CodingKeys: String, CodingKey {
        case operationId = "operation_id"
        case jobId = "job_id"
        case resourceId = "resource_id"
        case nodeID = "node_id"
        case hostname
        case recordIds = "record_ids"
        case jobIds = "job_ids"
        case recordsRetained = "records_retained"
    }
}

struct DNSCredentialBatchItem: Codable, Sendable {
    let connectionId: String
    let jobId: String
    let name: String
    let status: String
    let stage: String
    let errorMessage: String?

    enum CodingKeys: String, CodingKey {
        case connectionId = "connection_id"
        case jobId = "job_id"
        case name
        case status
        case stage
        case errorMessage = "error_message"
    }
}

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

struct CredentialsPage: Codable, Sendable {
    let items: [CredentialSummary]
    let total: Int
    let page: Int
    let pageSize: Int

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
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
    let ca: CAIdentityState?
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
        case ca
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
    let identityId: String?
    let caId: String?
    let caVersion: Int?
    let desired: Bool?
    let deployed: Bool?
    let phase: String?
    let entityType: String
    let entityID: String
    let source: String
    let name: String?
    let version: Int?
    let field: String?
    let nodeID: String?
    let configRevision: Int?

    enum CodingKeys: String, CodingKey {
        case identityId = "identity_id"
        case caId = "ca_id"
        case caVersion = "ca_version"
        case desired
        case deployed
        case phase
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
    let caRotation: CARotationState?
    let dnsItems: [DNSCredentialBatchItem]?
    let id: String
    let credentialId: String
    let version: Int
    let createdAt: String
    let items: [CredentialBatchItem]

    enum CodingKeys: String, CodingKey {
        case caRotation = "ca_rotation"
        case dnsItems = "dns_items"
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

struct NodeACMEStatus: Codable, Sendable {
    let schemaVersion: Int
    let componentVersion: String
    let policyId: String
    let state: String
    let serviceState: String?
    let domains: [String]
    let fingerprint: String?
    let notBefore: String?
    let notAfter: String?
    let lastSuccess: String?
    let nextAttempt: String?
    let error: String?
    let failures: Int
    let collectedAt: String

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case componentVersion = "component_version"
        case policyId = "policy_id"
        case state
        case serviceState = "service_state"
        case domains
        case fingerprint
        case notBefore = "not_before"
        case notAfter = "not_after"
        case lastSuccess = "last_success"
        case nextAttempt = "next_attempt"
        case error
        case failures
        case collectedAt = "collected_at"
    }
}

struct NodeMTLSSettings: Codable, Sendable {
    let mode: String
    let caCredentialId: String?
    let caCredentialVersion: Int?

    enum CodingKeys: String, CodingKey {
        case mode
        case caCredentialId = "ca_credential_id"
        case caCredentialVersion = "ca_credential_version"
    }
}

struct NodeMTLSTrust: Codable, Sendable {
    let caCredentialId: String
    let caCredentialVersion: Int
    let desired: Bool
    let deployed: Bool

    enum CodingKeys: String, CodingKey {
        case caCredentialId = "ca_credential_id"
        case caCredentialVersion = "ca_credential_version"
        case desired
        case deployed
    }
}

struct NodeMTLSStatus: Codable, Sendable {
    let target: NodeMTLSSettings
    let deployed: NodeMTLSSettings?
    let trust: [NodeMTLSTrust]

    enum CodingKeys: String, CodingKey {
        case target
        case deployed
        case trust
    }
}

struct NodeSummary: Codable, Sendable, Identifiable {
    let acmeStatus: NodeACMEStatus?
    let mtlsState: String?
    let mtlsRecoveryConfigured: Bool?
    let mtlsError: String?
    let mtlsGeneration: Int?
    let publishedConnection: PublishedConnection?
    let dnsBinding: DNSBinding?
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
    let mtlsRequired: Bool?
    let openGaps: Int?
    let pendingRevocations: Int?
    let proxyProbeUrl: String?

    enum CodingKeys: String, CodingKey {
        case acmeStatus = "acme_status"
        case mtlsState = "mtls_state"
        case mtlsRecoveryConfigured = "mtls_recovery_configured"
        case mtlsError = "mtls_error"
        case mtlsGeneration = "mtls_generation"
        case publishedConnection = "published_connection"
        case dnsBinding = "dns_binding"
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
        case mtlsRequired = "mtls_required"
        case openGaps = "open_gaps"
        case pendingRevocations = "pending_revocations"
        case proxyProbeUrl = "proxy_probe_url"
    }
}

struct NodesPage: Codable, Sendable {
    let items: [NodeSummary]
    let total: Int
    let page: Int
    let pageSize: Int

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
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
    let acmeStatus: NodeACMEStatus?
    let mtlsState: String?
    let mtlsRecoveryConfigured: Bool?
    let mtlsError: String?
    let mtlsGeneration: Int?
    let mtls: NodeMTLSStatus?
    let publishedConnection: PublishedConnection?
    let dnsBinding: DNSBinding?
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
        case acmeStatus = "acme_status"
        case mtlsState = "mtls_state"
        case mtlsRecoveryConfigured = "mtls_recovery_configured"
        case mtlsError = "mtls_error"
        case mtlsGeneration = "mtls_generation"
        case mtls
        case publishedConnection = "published_connection"
        case dnsBinding = "dns_binding"
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
    let managedIdentityIds: [String]?
    let nodeID: String
    let createdAt: String
    let sourceGroups: [AuthorizationGroupSource]?

    enum CodingKeys: String, CodingKey {
        case managedIdentityIds = "managed_identity_ids"
        case nodeID = "node_id"
        case createdAt = "created_at"
        case sourceGroups = "source_groups"
    }
}

struct UserSummary: Codable, Sendable, Identifiable {
    let managedCertificates: [ManagedUserIdentity]?
    let id: String
    let name: String
    let enabled: Bool
    let expiresAt: String?
    let quotaBytes: Int64?
    let usageBytes: Int64
    let revision: Int
    let quotaResetAt: String?
    let assignments: [AssignmentInfo]
    let authorizationGroups: [AuthorizationGroupReference]?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case managedCertificates = "managed_certificates"
        case id
        case name
        case enabled
        case expiresAt = "expires_at"
        case quotaBytes = "quota_bytes"
        case usageBytes = "usage_bytes"
        case revision
        case quotaResetAt = "quota_reset_at"
        case assignments
        case authorizationGroups = "authorization_groups"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

struct UsersPage: Codable, Sendable {
    let items: [UserSummary]
    let total: Int
    let page: Int
    let pageSize: Int

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
    }
}

struct JobOutcome: Codable, Sendable {
    let status: String?
    let fingerprint: String?
    let expectedFingerprint: String?
    let distribution: String?
    let version: String?
    let architecture: String?
    let privilege: String?
    let proxyProbe: [String: JSONValue]?

    enum CodingKeys: String, CodingKey {
        case status
        case fingerprint
        case expectedFingerprint = "expected_fingerprint"
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
    let resourceType: String?
    let resourceId: String?
    let resourceName: String?
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
        case resourceType = "resource_type"
        case resourceId = "resource_id"
        case resourceName = "resource_name"
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

struct JobsPage: Codable, Sendable {
    let items: [JobSummary]
    let total: Int
    let page: Int
    let pageSize: Int

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
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

struct AuditPage: Codable, Sendable {
    let items: [AuditSummary]
    let total: Int
    let page: Int
    let pageSize: Int

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
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
    let dnsJobIds: [String]?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case revision
        case state
        case trafficStatsPort = "traffic_stats_port"
        case syncJobQueued = "sync_job_queued"
        case dnsJobIds = "dns_job_ids"
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
    let dnsAllocation: DNSActionReceipt?
    let id: String?
    let revision: Int?
    let nodeAuthToken: String?
    let node: NodeReceipt?

    enum CodingKeys: String, CodingKey {
        case dnsAllocation = "dns_allocation"
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
    let directoryGeneration: Int64
    let currentAdminTokenId: String?
    let features: [String]?
    let apiVersion: String
    let serviceVersion: String
    let hysteriaVersion: String
    let mihomoVersion: String

    enum CodingKeys: String, CodingKey {
        case directoryGeneration = "directory_generation"
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

struct ClientSummary: Codable, Sendable {
    let nodeCount: Int
    let userTotal: Int
    let userEnabled: Int
    let userExpired: Int
    let userExpiring: Int
    let userDepleted: Int
    let notificationNodes: [NodeSummary]
    let notificationCredentials: [CredentialSummary]

    enum CodingKeys: String, CodingKey {
        case nodeCount = "node_count"
        case userTotal = "user_total"
        case userEnabled = "user_enabled"
        case userExpired = "user_expired"
        case userExpiring = "user_expiring"
        case userDepleted = "user_depleted"
        case notificationNodes = "notification_nodes"
        case notificationCredentials = "notification_credentials"
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

struct OverviewNodesPage: Codable, Sendable {
    let items: [OverviewNode]
    let total: Int
    let page: Int
    let pageSize: Int
    let generatedAt: String

    enum CodingKeys: String, CodingKey {
        case items
        case total
        case page
        case pageSize = "page_size"
        case generatedAt = "generated_at"
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
    let healthyNodes: Int64
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
        case healthyNodes = "healthy_nodes"
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
    let metrics: String?
    let generatedAt: String
    let range: String
    let timezone: String
    let source: String
    let buckets: [OverviewBucket]

    enum CodingKeys: String, CodingKey {
        case metrics
        case generatedAt = "generated_at"
        case range
        case timezone
        case source
        case buckets
    }
}
