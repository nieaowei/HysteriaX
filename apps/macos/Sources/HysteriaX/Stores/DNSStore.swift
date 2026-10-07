import Foundation

private struct DNSDisplaySnapshot: Codable {
    let connections: [DNSConnection]
    let zones: [DNSZone]
    let records: [DNSRecord]
    let updatedAt: Date
}

extension ManagementStore {
    private var dnsSnapshotKey: String { "dnsDisplaySnapshot.\(serviceAddress)" }

    func restoreDNSSnapshot() {
        guard let data = UserDefaults.standard.data(forKey: dnsSnapshotKey),
              let snapshot = try? JSONDecoder().decode(DNSDisplaySnapshot.self, from: data) else { return }
        dnsConnections = snapshot.connections
        dnsZones = snapshot.zones
        dnsRecords = snapshot.records
        dnsUpdatedAt = snapshot.updatedAt
    }

    func refreshDNS() async {
        guard supportsDNSManagement, isConnected else { return }
        let generation = UUID()
        dnsRequestGeneration = generation
        let service = serviceAddress
        dnsIsLoading = true
        defer { if dnsRequestGeneration == generation { dnsIsLoading = false } }
        do {
            let client = try requireConnectedAPI()
            async let connections = client.get(APIEndpoints.listDNSConnections)
            async let zones = client.get(APIEndpoints.listDNSZones)
            async let records = client.get(APIEndpoints.listDNSRecords())
            let snapshot = try await DNSDisplaySnapshot(connections: connections, zones: zones, records: records, updatedAt: Date())
            guard service == serviceAddress, dnsRequestGeneration == generation, !Task.isCancelled else { return }
            dnsConnections = snapshot.connections
            dnsZones = snapshot.zones
            dnsRecords = snapshot.records
            dnsUpdatedAt = snapshot.updatedAt
            dnsError = nil
            if let data = try? JSONEncoder().encode(snapshot) { UserDefaults.standard.set(data, forKey: dnsSnapshotKey) }
        } catch {
            guard service == serviceAddress, dnsRequestGeneration == generation, !Task.isCancelled else { return }
            dnsError = error.localizedDescription
        }
    }

    func createDNSConnection(name: String, credentialID: String) async throws {
        guard let credential = credentials.first(where: { $0.id == credentialID && !$0.archived && $0.kind == "dns" && $0.metadata["provider"]?.stringValue == "cloudflare" }) else {
            throw APIClientError.server(L10n.text("请选择有效的 Cloudflare DNS 凭据。"))
        }
        let _: DNSConnection = try await requireConnectedAPI().post(APIEndpoints.createDNSConnection,
            body: DNSConnectionCreateRequest(name: name, credentialId: credentialID, credentialVersion: credential.latestVersion))
        await refreshDNS()
    }

    func verifyDNSConnection(_ connection: DNSConnection, refresh: Bool = false) async throws {
        let operation = refresh ? APIEndpoints.refreshDNSConnection(id: connection.id) : APIEndpoints.verifyDNSConnection(id: connection.id)
        let _: DNSActionReceipt = try await requireConnectedAPI().post(operation,
            body: DNSActionRequest(expectedRevision: connection.revision, idempotencyKey: UUID().uuidString))
        await self.refresh()
    }

    func setDNSZoneEnabled(_ zone: DNSZone, enabled: Bool) async throws {
        let _: DNSZone = try await requireConnectedAPI().patch(APIEndpoints.updateDNSZone(id: zone.id),
            body: DNSZonePatchRequest(expectedRevision: zone.revision, enabled: enabled))
        await refreshDNS()
    }

    func refreshDNSZone(_ zone: DNSZone) async throws {
        let _: DNSActionReceipt = try await requireConnectedAPI().post(APIEndpoints.refreshDNSZone(id: zone.id),
            body: DNSActionRequest(expectedRevision: zone.revision, idempotencyKey: UUID().uuidString))
        await refresh()
    }

    func dnsRecordDetail(_ id: String) async throws -> DNSRecord {
        try await requireConnectedAPI().get(APIEndpoints.getDNSRecord(id: id))
    }

    func saveDNSRecord(_ record: DNSRecord?, zoneID: String, input: DNSRecordInput, idempotencyKey: String) async throws {
        let client = try requireConnectedAPI()
        if let record {
            let _: DNSActionReceipt = try await client.patch(APIEndpoints.updateDNSRecord(id: record.id),
                body: DNSRecordUpdateRequest(expectedRevision: record.revision, idempotencyKey: idempotencyKey, record: input))
        } else {
            let _: DNSActionReceipt = try await client.post(APIEndpoints.createDNSRecord,
                body: DNSRecordCreateRequest(zoneId: zoneID, idempotencyKey: idempotencyKey, record: input))
        }
        await refresh()
    }

    func deleteDNSRecord(_ record: DNSRecord) async throws {
        let _: DNSActionReceipt = try await requireConnectedAPI().delete(APIEndpoints.deleteDNSRecord(id: record.id),
            body: DNSActionRequest(expectedRevision: record.revision, idempotencyKey: UUID().uuidString))
        await refresh()
    }

    func checkDNSRecord(_ record: DNSRecord) async throws {
        let _: DNSActionReceipt = try await requireConnectedAPI().post(APIEndpoints.checkDNSRecord(id: record.id),
            body: DNSActionRequest(expectedRevision: record.revision, idempotencyKey: UUID().uuidString))
        await refresh()
    }

    func retryDNSJob(_ job: JobSummary) async throws {
        guard let id = job.resourceId else { throw APIClientError.invalidResponse }
        let revision: Int
        switch job.resourceType {
        case "dns_record": revision = try await dnsRecordDetail(id).revision
        case "dns_zone":
            guard let zone = dnsZones.first(where: { $0.id == id }) else { throw APIClientError.server(L10n.text("域名区域已不存在。")) }
            revision = zone.revision
        case "dns_connection":
            let connection = try await requireConnectedAPI().get(APIEndpoints.getDNSConnection(id: id))
            revision = connection.revision
        default: throw APIClientError.invalidResponse
        }
        let _: JobReceipt = try await requireConnectedAPI().post(APIEndpoints.retryJob(id: job.id), body: RevisionRequest(expectedRevision: revision))
        await refresh()
    }

    func setDNSBinding(nodeID: String, revision: Int, allocation: DNSAllocation) async throws {
        let _: DNSActionReceipt = try await requireConnectedAPI().put(APIEndpoints.setDNSBinding(id: nodeID),
            body: DNSBindingSetRequest(expectedRevision: revision, allocation: allocation))
        await refresh()
    }

    func removeDNSBinding(nodeID: String, revision: Int, publicHost: String, idempotencyKey: String) async throws {
        let _: DNSActionReceipt = try await requireConnectedAPI().delete(APIEndpoints.removeDNSBinding(id: nodeID),
            body: DNSBindingRemoveRequest(expectedRevision: revision, idempotencyKey: idempotencyKey, publicHost: publicHost))
        await refresh()
    }

    func showDNSRecord(_ id: String?) {
        requestedDNSRecordID = id
        requestedSection = "dns"
    }
}

extension ManagementStore {
    func renameDNSConnection(_ connection: DNSConnection, name: String) async throws {
        let _: DNSConnection = try await requireConnectedAPI().patch(APIEndpoints.updateDNSConnection(id: connection.id),
            body: DNSConnectionPatchRequest(expectedRevision: connection.revision, name: name))
        await refreshDNS()
    }
    func deleteDNSConnection(_ connection: DNSConnection) async throws {
        try await requireConnectedAPI().deleteNoContent(APIEndpoints.deleteDNSConnection(id: connection.id, expectedRevision: connection.revision))
        await refreshDNS()
    }
    func setExternalPublicHost(_ detail: NodeDetail, host: String) async throws {
        let _: NodeUpdateResponse = try await requireConnectedAPI().patch(APIEndpoints.updateNode(id: detail.id),
            body: NodePatchRequest(expectedRevision: detail.revision, publicHost: host))
        await refresh()
    }
}
