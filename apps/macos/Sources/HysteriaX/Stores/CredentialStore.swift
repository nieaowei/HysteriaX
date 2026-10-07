import Foundation

extension ManagementStore {
    func resolveMTLSCredential(userID: String, nodeID: String, certificate: String?, privateKey: String?, selected: String?) async throws -> (id: String, version: Int)? {
        if let selected, !selected.isEmpty {
            guard let entry = credentials.first(where: { $0.id == selected && $0.ownerUserId == userID && $0.kind == "tls_identity" && !$0.archived }) else { throw APIClientError.server(L10n.text("请选择该用户的有效 mTLS 凭据。")) }
            return (entry.id, entry.latestVersion)
        }
        guard let certificate, let privateKey else { return nil }
        let receipt = try await createCredential(name: "\(userID) · \(nodeID) · mTLS", kind: "tls_identity", ownerUserID: userID,
            payload: ["certificate": .string(certificate), "private_key": .string(privateKey)])
        return (receipt.id, receipt.version ?? 1)
    }

    func credentialDetail(_ id: String) async throws -> CredentialDetail {
        try await requireConnectedAPI().get(APIEndpoints.getCredential(id: id))
    }

    func createCredential(name: String, kind: String, ownerUserID: String? = nil,
                          reminderAt: String? = nil, payload: [String: JSONValue]) async throws -> CredentialReceipt {
        let result = try await requireConnectedAPI().post(APIEndpoints.createCredential,
            body: CredentialCreateRequest(name: name, kind: kind, ownerUserId: ownerUserID, reminderAt: reminderAt, payload: payload))
        await refresh()
        return result
    }

    func publishCredential(_ detail: CredentialDetail, payload: [String: JSONValue]) async throws {
        let _: CredentialReceipt = try await requireConnectedAPI().post(APIEndpoints.publishCredentialVersion(id: detail.id),
            body: CredentialPublishRequest(expectedRevision: detail.revision, payload: payload))
        await refresh()
    }

    func updateCredential(_ detail: CredentialDetail, name: String, archived: Bool, reminderAt: String?) async throws {
        let _: CredentialReceipt = try await requireConnectedAPI().patch(APIEndpoints.updateCredential(id: detail.id),
            body: CredentialPatchRequest(expectedRevision: detail.revision, name: name, archived: archived, reminderAt: reminderAt))
        await refresh()
    }

    func deleteCredential(_ detail: CredentialDetail) async throws {
        try await requireConnectedAPI().deleteNoContent(APIEndpoints.deleteCredential(id: detail.id, expectedRevision: detail.revision))
        await refresh()
    }

    func retryCredentialBatch(_ id: String) async throws {
        let _: CredentialBatchReceipt = try await requireConnectedAPI().post(APIEndpoints.retryCredentialBatch(id: id))
        await refresh()
    }
}

