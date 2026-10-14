import Foundation
import Observation

/// Shared across page lifetimes; stores summaries only, never credential payloads.
@MainActor
@Observable
final class ListPageCache {
    private struct Entry {
        let data: Data
        var expiresAt: Date
        var accessedAt: Date
    }
    private struct Flight {
        let id: UUID
        let task: Task<Data, Error>
    }
    @ObservationIgnored private var entries: [String: Entry] = [:]
    @ObservationIgnored private var flights: [String: Flight] = [:]
    @ObservationIgnored private var generation = UUID()
    private var revision = 0
    private let maxEntries: Int
    private let maxBytes: Int
    private let now: () -> Date

    init(maxEntries: Int = 48, maxBytes: Int = 16 * 1024 * 1024, now: @escaping () -> Date = Date.init) {
        self.maxEntries = maxEntries
        self.maxBytes = maxBytes
        self.now = now
    }

    func peek<Value: Decodable>(_ key: String, as: Value.Type) -> Value? {
        _ = revision
        guard let entry = entries[key] else { return nil }
        return try? JSONDecoder().decode(Value.self, from: entry.data)
    }

    /// Keep stale results visible while preventing an old flight from refilling them.
    func invalidate(clear: Bool = false, types: [String]? = nil) {
        func matches(_ key: String) -> Bool {
            clear || types.map { names in names.contains { key.contains($0) } } ?? true
        }
        if clear || types == nil { generation = UUID() }
        for key in Array(flights.keys) where matches(key) {
            flights.removeValue(forKey: key)?.task.cancel()
        }
        if clear { entries.removeAll() }
        else {
            for key in Array(entries.keys) where matches(key) {
                entries[key]?.expiresAt = .distantPast
            }
        }
        revision += 1
    }

    func load<Value: Codable & Sendable>(
        _ key: String, ttl: TimeInterval, revalidate: Bool = false,
        fetch: @escaping @MainActor @Sendable (Bool) async throws -> Value
    ) async throws -> Value {
        if !revalidate, let entry = entries[key], entry.expiresAt > now(), let value = peek(key, as: Value.self) {
            entries[key]?.accessedAt = now()
            return value
        }
        let capturedGeneration = generation
        let flight: Flight
        if let existing = flights[key] { flight = existing }
        else {
            let bypassCache = (revalidate && entries[key] != nil) || entries[key]?.expiresAt == .distantPast
            flight = Flight(id: UUID(), task: Task { try JSONEncoder().encode(try await fetch(bypassCache)) })
            flights[key] = flight
        }
        do {
            let data = try await flight.task.value
            guard generation == capturedGeneration, !flight.task.isCancelled else { throw CancellationError() }
            if flights[key]?.id == flight.id {
                flights.removeValue(forKey: key)
                if data.count <= min(maxBytes, 2 * 1024 * 1024) {
                    entries[key] = Entry(data: data, expiresAt: now().addingTimeInterval(ttl), accessedAt: now())
                    while entries.count > maxEntries || entries.values.reduce(0, { $0 + $1.data.count }) > maxBytes {
                        guard let oldest = entries.min(by: { $0.value.accessedAt < $1.value.accessedAt })?.key else { break }
                        entries.removeValue(forKey: oldest)
                    }
                    revision += 1
                }
            }
            try Task.checkCancellation()
            return try JSONDecoder().decode(Value.self, from: data)
        } catch {
            if flights[key]?.id == flight.id { flights.removeValue(forKey: key) }
            throw error
        }
    }
}
