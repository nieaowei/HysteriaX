import Foundation

struct PublicPortDraft {
    var followsListener = true
    var customPort = "443"

    init() {}

    init(port: Int, listenAddress: String) {
        customPort = String(port)
        followsListener = Self.firstListenerPort(listenAddress) == port
    }

    func portText(listenAddress: String) -> String {
        followsListener ? Self.firstListenerPort(listenAddress).map { String($0) } ?? "" : customPort
    }

    mutating func setCustomPort(_ value: String) {
        customPort = value
        followsListener = false
    }

    static func firstListenerPort(_ address: String) -> Int? {
        let address = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let separator = address.lastIndex(of: ":") else { return nil }
        let ports = address[address.index(after: separator)...]
        guard let first = ports.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "," || $0 == "-" }).first,
              let port = Int(first), (1...65535).contains(port) else { return nil }
        return port
    }
}
