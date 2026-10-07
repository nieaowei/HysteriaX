import Foundation

extension ManagementStore {
    func refreshAuthorizationGroups() async {
        guard supportsAuthorizationGroups else {
            authorizationGroups = []
            authorizationGroupsError = nil
            isLoadingAuthorizationGroups = false
            authorizationGroupsRequestID = UUID()
            return
        }
        guard let client = try? requireConnectedAPI() else { return }

        let requestID = UUID()
        let service = serviceAddress
        authorizationGroupsRequestID = requestID
        isLoadingAuthorizationGroups = true
        defer {
            if authorizationGroupsRequestID == requestID {
                isLoadingAuthorizationGroups = false
            }
        }

        do {
            let groups: [AuthorizationGroupSummary] = try await client.get(APIEndpoints.listAuthorizationGroups)
            guard serviceAddress == service, authorizationGroupsRequestID == requestID, !Task.isCancelled else { return }
            authorizationGroups = groups
            authorizationGroupsError = nil
        } catch {
            guard serviceAddress == service, authorizationGroupsRequestID == requestID, !Task.isCancelled else { return }
            authorizationGroupsError = error.localizedDescription
        }
    }

    func authorizationUserDetail(_ userID: String) async throws -> UserSummary {
        try await requireConnectedAPI().get(APIEndpoints.getUser(id: userID))
    }

    func authorizationGroupDetail(_ groupID: String) async throws -> AuthorizationGroupSummary {
        try await requireConnectedAPI().get(APIEndpoints.getAuthorizationGroup(groupID: groupID))
    }

    func userSummary(_ userID: String) async throws -> UserSummary {
        try await requireConnectedAPI().get(APIEndpoints.getUser(id: userID))
    }

    func previewAuthorizationGroup(
        group: AuthorizationGroupSummary?,
        name: String,
        userIDs: [String],
        nodeIDs: [String],
        mtlsBindings: [AuthorizationMTLSBinding]
    ) async throws -> AuthorizationChangePreview {
        let client = try requireConnectedAPI()
        let request = AuthorizationGroupPreviewRequest(
            action: group == nil ? "create" : "update",
            expectedRevision: group?.revision,
            name: name,
            userIds: userIDs,
            nodeIds: nodeIDs,
            mtlsBindings: mtlsBindings
        )
        if let group {
            return try await client.post(APIEndpoints.previewAuthorizationGroupChange(groupID: group.id), body: request)
        }
        return try await client.post(APIEndpoints.previewAuthorizationGroupCreate, body: request)
    }

    func createAuthorizationGroup(
        name: String,
        userIDs: [String],
        nodeIDs: [String],
        previewToken: String,
        mtlsBindings: [AuthorizationMTLSBinding]
    ) async throws -> AuthorizationGroupMutationResponse {
        let client = try requireConnectedAPI()
        let result: AuthorizationGroupMutationResponse = try await client.post(
            APIEndpoints.createAuthorizationGroup,
            body: AuthorizationGroupCreateRequest(
                name: name,
                userIds: userIDs,
                nodeIds: nodeIDs,
                previewToken: previewToken,
                mtlsBindings: mtlsBindings
            )
        )
        await refresh()
        return result
    }

    func updateAuthorizationGroup(
        _ group: AuthorizationGroupSummary,
        name: String,
        userIDs: [String],
        nodeIDs: [String],
        previewToken: String,
        mtlsBindings: [AuthorizationMTLSBinding]
    ) async throws -> AuthorizationGroupMutationResponse {
        let client = try requireConnectedAPI()
        let result: AuthorizationGroupMutationResponse = try await client.put(
            APIEndpoints.updateAuthorizationGroup(groupID: group.id),
            body: AuthorizationGroupUpdateRequest(
                expectedRevision: group.revision,
                name: name,
                userIds: userIDs,
                nodeIds: nodeIDs,
                previewToken: previewToken,
                mtlsBindings: mtlsBindings
            )
        )
        await refresh()
        return result
    }

    func previewAuthorizationGroupDeletion(_ group: AuthorizationGroupSummary) async throws -> AuthorizationChangePreview {
        try await requireConnectedAPI().post(
            APIEndpoints.previewAuthorizationGroupChange(groupID: group.id),
            body: AuthorizationGroupPreviewRequest(action: "delete", expectedRevision: group.revision)
        )
    }

    func deleteAuthorizationGroup(
        _ group: AuthorizationGroupSummary,
        previewToken: String
    ) async throws -> AuthorizationGroupMutationResponse {
        let result: AuthorizationGroupMutationResponse = try await requireConnectedAPI().delete(
            APIEndpoints.deleteAuthorizationGroup(groupID: group.id),
            body: AuthorizationGroupDeleteRequest(expectedRevision: group.revision, previewToken: previewToken)
        )
        await refresh()
        return result
    }

    func previewUserAuthorizationGroups(
        _ user: UserSummary,
        groupIDs: [String],
        mtlsBindings: [AuthorizationMTLSBinding]
    ) async throws -> AuthorizationChangePreview {
        try await requireConnectedAPI().post(
            APIEndpoints.previewUserAuthorizationGroups(id: user.id),
            body: UserAuthorizationGroupsPreviewRequest(
                expectedRevision: user.revision,
                groupIds: groupIDs,
                mtlsBindings: mtlsBindings
            )
        )
    }

    func updateUserAuthorizationGroups(
        _ user: UserSummary,
        groupIDs: [String],
        previewToken: String,
        mtlsBindings: [AuthorizationMTLSBinding]
    ) async throws -> UserAuthorizationGroupsMutationResponse {
        let result: UserAuthorizationGroupsMutationResponse = try await requireConnectedAPI().put(
            APIEndpoints.updateUserAuthorizationGroups(id: user.id),
            body: UserAuthorizationGroupsUpdateRequest(
                expectedRevision: user.revision,
                previewToken: previewToken,
                groupIds: groupIDs,
                mtlsBindings: mtlsBindings
            )
        )
        await refresh()
        return result
    }
}
