import CryptoKit
import Foundation

struct ManagementWriteIdentity: Sendable {
    let digest: String
    let key: String
}

/// Keep only keyed digests and opaque request keys; never persist request bodies,
/// bearer tokens, URLs, passwords, or returned credentials in UserDefaults.
actor ManagementWriteJournal {
    static let shared = ManagementWriteJournal()
    private let storageKey: String
    private let limit: Int

    init(storageKey: String = "management.pendingWrites.v2", limit: Int = 512) {
        self.storageKey = storageKey
        self.limit = limit
    }

    func reserve(method: String, url: URL, token: String, body: Data?) throws -> ManagementWriteIdentity {
        let fields = [method, url.absoluteString, body?.base64EncodedString() ?? ""]
        let bytes = try JSONEncoder().encode(fields)
        let digest = HMAC<SHA256>.authenticationCode(for: bytes, using: SymmetricKey(data: Data(token.utf8)))
            .map { String(format: "%02x", $0) }.joined()
        var entries = UserDefaults.standard.dictionary(forKey: storageKey) as? [String: String] ?? [:]
        if let key = entries[digest] {
            guard UserDefaults.standard.synchronize() else { throw APIClientError.invalidResponse }
            return ManagementWriteIdentity(digest: digest, key: key)
        }
        guard entries.count < limit else {
            throw APIClientError.server(L10n.text("有过多结果待确认的请求，请先恢复并重试原请求。"))
        }
        let key = UUID().uuidString
        entries[digest] = key
        UserDefaults.standard.set(entries, forKey: storageKey)
        guard UserDefaults.standard.synchronize() else { throw APIClientError.invalidResponse }
        return ManagementWriteIdentity(digest: digest, key: key)
    }

    func acknowledge(_ identity: ManagementWriteIdentity) {
        var entries = UserDefaults.standard.dictionary(forKey: storageKey) as? [String: String] ?? [:]
        guard entries[identity.digest] == identity.key else { return }
        entries.removeValue(forKey: identity.digest)
        UserDefaults.standard.set(entries, forKey: storageKey)
        UserDefaults.standard.synchronize()
    }
}
