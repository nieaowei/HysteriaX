import SwiftUI

struct ManagedIdentityIssuanceSummary: View {
    let immediate: Int
    let pending: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L10n.text("系统将签发 {0} 个共享身份 · {1} 个待恢复后签发", String(immediate), String(pending)))
                .font(.callout.weight(.medium))
            Text(L10n.text("同一用户在共享 CA 的节点间复用证书，无需选择个人证书。"))
                .font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityIdentifier("authorization.managedIssuance")
    }
}
