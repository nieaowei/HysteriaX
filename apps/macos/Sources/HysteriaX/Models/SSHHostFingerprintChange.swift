import Foundation

struct SSHHostFingerprintChange: Equatable, Sendable {
    let expected: String
    let observed: String

    init?(job: JobSummary) {
        guard job.status == "failed", job.nodeID != nil, let message = job.errorMessage else { return nil }
        // Existing servers return the fingerprints in the task error, rather than a structured result.
        let pattern = #"(?:^|: )SSH host key changed: expected (SHA256:[A-Za-z0-9+/]{43}); observed (SHA256:[A-Za-z0-9+/]{43})(?=$|[\s:])"#
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let match = regex.firstMatch(in: message, range: NSRange(message.startIndex..., in: message)),
              let expectedRange = Range(match.range(at: 1), in: message),
              let observedRange = Range(match.range(at: 2), in: message) else { return nil }
        expected = String(message[expectedRange])
        observed = String(message[observedRange])
        guard expected != observed else { return nil }
    }

    func canConfirm(state: String, savedFingerprint: String?) -> Bool {
        ["fingerprint_changed", "delete_failed"].contains(state) && savedFingerprint == expected
    }
}
