import Foundation

struct ManagedMTLSDraft {
    var mode = "off"
    var caID = ""
    var caVersion: Int?

    init(configuration: [String: JSONValue] = [:]) {
        let mtls = configuration["mtls"]?.objectValue ?? [:]
        mode = mtls["mode"]?.stringValue ?? "off"
        caID = mtls["ca_credential_id"]?.stringValue ?? ""
        if case .integer(let version) = mtls["ca_credential_version"] { caVersion = Int(version) }
    }
    var configuration: JSONValue {
        guard mode == "managed" else { return .object(["mode": .string("off")]) }
        var value: [String: JSONValue] = ["mode": .string("managed"), "ca_credential_id": .string(caID)]
        if let caVersion { value["ca_credential_version"] = .integer(caVersion) }
        return .object(value)
    }
    var validationError: String? {
        mode == "managed" && (caID.isEmpty || caVersion == nil) ? L10n.text("请选择有效 CA 身份并等待读取当前发布版本。") : nil
    }
}
