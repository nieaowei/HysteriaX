import Foundation
#if HYSTERIAX_UI_TESTING

@MainActor
enum KeychainStore {
    private static var tokenInMemory: String?

    static func readToken() -> String? {
        tokenInMemory
    }

    static func saveToken(_ token: String) throws {
        tokenInMemory = token
    }
}

#else
import Security

enum KeychainStore {
    private static let service = "com.hysteriax.management"
    private static let account = "administrator-token"

    static func readToken() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func saveToken(_ token: String) throws {
        let key: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        SecItemDelete(key as CFDictionary)
        var attributes = key
        attributes[kSecValueData as String] = Data(token.utf8)
        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
    }
}
#endif
