import Foundation

extension ManagementStore {
    func credentialsPage(page: Int, pageSize: Int, category: String, kind: String, query: String, sort: String, order: String, cacheKey: String? = nil, revalidate: Bool = false, includeInternal: Bool = false) async throws -> CredentialsPage {
        try await loadListPage(APIEndpoints.listCredentials(page: page, pageSize: pageSize, category: category, kind: kind.isEmpty ? nil : kind, includeInternal: includeInternal, q: query, sort: sort, order: order), cacheKey: cacheKey, ttl: 60, revalidate: revalidate)
    }

    func credentialDetail(_ id: String) async throws -> CredentialDetail {
        try await requireConnectedAPI().get(APIEndpoints.getCredential(id: id))
    }

    func createCredential(name: String, kind: String,
                          reminderAt: String? = nil, payload: [String: JSONValue]) async throws -> CredentialReceipt {
        let result = try await requireConnectedAPI().post(APIEndpoints.createCredential,
            body: CredentialCreateRequest(name: name, kind: kind, reminderAt: reminderAt, payload: payload))
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


extension ManagementStore {
    func reissueManagedCertificates(userID: String, request: ManagedUserReissueRequest) async throws -> ManagedUserReissueReceipt {
        let result = try await requireConnectedAPI().post(APIEndpoints.reissueManagedUserCertificates(id: userID), body: request)
        await refresh()
        return result
    }
}

extension ManagementStore {
    func createCAIdentity(_ request: CAIdentityCreateRequest) async throws -> CAIdentityReceipt {
        let result = try await requireConnectedAPI().post(APIEndpoints.createCAIdentity, body: request)
        await refresh()
        return result
    }

    func publishCAIdentity(_ id: String, request: CAIdentityPublishRequest) async throws -> CAIdentityReceipt {
        let result = try await requireConnectedAPI().post(APIEndpoints.publishCAIdentityVersion(id: id), body: request)
        await refresh()
        return result
    }

    func updateCAIdentityPolicy(_ id: String, request: CAPolicyUpdateRequest) async throws {
        let _: CAPolicyReceipt = try await requireConnectedAPI().patch(APIEndpoints.updateCAPolicy(id: id), body: request)
        await refresh()
    }
}
