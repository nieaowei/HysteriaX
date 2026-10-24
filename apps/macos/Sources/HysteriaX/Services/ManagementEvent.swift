import Foundation

struct ManagementEventCursor: Equatable, Sendable {
    let epoch: String
    let sequence: Int64

    init(_ value: String) throws {
        let parts = value.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0] == "v2",
              let uuid = UUID(uuidString: String(parts[1])), uuid.uuidString.lowercased() == parts[1],
              let sequence = Int64(parts[2]), sequence >= 0, String(sequence) == parts[2] else {
            throw APIClientError.invalidResponse
        }
        epoch = String(parts[1])
        self.sequence = sequence
    }
}

struct ManagementEvent: Sendable {
    let cursor: String
    let kind: String
    let sourceID: Int64?
}

struct ManagementEventParser {
    private let v2: Bool
    private var id: String?
    private var kind = "message"
    private var data: [String] = []
    private var bytes = 0

    init(v2: Bool) { self.v2 = v2 }

    mutating func append(_ line: String) throws -> ManagementEvent? {
        bytes += line.utf8.count + 1
        guard bytes <= 2 * 1024 * 1024 else { throw APIClientError.invalidResponse }
        if line.isEmpty {
            defer { id = nil; kind = "message"; data = []; bytes = 0 }
            guard !data.isEmpty else { return nil }
            if kind == "stream.reset" { throw APIClientError.eventCursorExpired }
            guard let id, !id.isEmpty else { throw APIClientError.invalidResponse }
            if v2 { _ = try ManagementEventCursor(id) }
            else if Int64(id).map({ $0 >= 0 }) != true { throw APIClientError.invalidResponse }
            let payload = Data(data.joined(separator: "\n").utf8)
            struct Identity: Decodable {
                let event_id: Int64?
                let cursor: String?
            }
            let identity = try JSONDecoder().decode(Identity.self, from: payload)
            if v2 {
                if kind == "stream.cursor" {
                    guard identity.cursor == id else { throw APIClientError.invalidResponse }
                } else {
                    guard let source = identity.event_id, source > 0 else { throw APIClientError.invalidResponse }
                }
            }
            return ManagementEvent(cursor: id, kind: kind, sourceID: identity.event_id)
        }
        guard !line.hasPrefix(":") else { return nil }
        let parts = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
        let value = parts.count == 2 ? String(parts[1].hasPrefix(" ") ? parts[1].dropFirst() : parts[1][...]) : ""
        switch parts[0] {
        case "id":
            guard !value.contains("\0") else { throw APIClientError.invalidResponse }
            id = value
        case "event": kind = value
        case "data": data.append(value)
        default: break
        }
        return nil
    }
}

/// SSE dispatch depends on empty lines. Decode line endings explicitly rather
/// than using a general-purpose text line sequence that can omit empty lines.
struct ManagementEventDecoder {
    private var parser: ManagementEventParser
    private var line = Data()
    private var previousCR = false
    private var firstLine = true

    init(v2: Bool) { parser = ManagementEventParser(v2: v2) }

    mutating func append(_ byte: UInt8) throws -> ManagementEvent? {
        if byte == 10, previousCR { previousCR = false; return nil }
        previousCR = byte == 13
        if byte == 10 || byte == 13 {
            guard var text = String(data: line, encoding: .utf8) else { throw APIClientError.invalidResponse }
            line.removeAll(keepingCapacity: true)
            if firstLine {
                firstLine = false
                if text.hasPrefix("\u{feff}") { text.removeFirst() }
            }
            return try parser.append(text)
        }
        guard line.count < 2 * 1024 * 1024 else { throw APIClientError.invalidResponse }
        line.append(byte)
        return nil
    }
}
