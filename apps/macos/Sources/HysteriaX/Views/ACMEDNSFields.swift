import SwiftUI

struct ACMEDNSFields: View {
    @Binding var draft: ACMEDNSDraft

    var body: some View {
        Picker(L10n.text("DNS 服务商"), selection: $draft.provider) {
            Text(L10n.text("选择服务商")).tag("")
            ForEach(ACMEDNSProvider.allCases) { provider in
                Text(provider.title).tag(provider.rawValue)
            }
            if !draft.provider.isEmpty, draft.definition == nil {
                Text(L10n.text("不支持的服务商：{0}", String(describing: (draft.provider)))).tag(draft.provider)
            }
        }
        if let provider = draft.definition {
            ForEach(provider.fields.filter(\.required)) { field in
                input(field)
            }
            if provider.fields.contains(where: { !$0.required }) {
                DisclosureGroup(L10n.text("可选参数")) {
                    ForEach(provider.fields.filter { !$0.required }) { field in
                        input(field)
                    }
                }
            }
            if let error = draft.validationError {
                Text(error).font(.callout).foregroundStyle(.orange)
            }
        }
        if !draft.unknownKeys.isEmpty {
            DisclosureGroup(L10n.text("原配置中的其他参数（{0}）", String(describing: (draft.unknownKeys.count)))) {
                Text(L10n.text("这些参数会保留。当前 Hysteria 版本可能忽略它们；请检查是否为拼写错误或其他服务商的参数。"))
                    .font(.callout).foregroundStyle(.secondary)
                ForEach(draft.unknownKeys, id: \.self) { key in
                    HStack {
                        SecureField(key, text: valueBinding(key))
                        Button(role: .destructive) {
                            draft.removeValue(for: key)
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .accessibilityLabel(L10n.text("删除参数 {0}", String(describing: (key))))
                    }
                }
            }
        }
        Text(L10n.text("DNS 凭据会加密保存在管理服务。所有 ACME 域名须由同一个 DNS 服务商管理。"))
            .font(.callout).foregroundStyle(.secondary)
        Link(L10n.text("查看 ACME DNS 配置文档"), destination: URL(string: "https://v2.hysteria.network/zh/docs/advanced/ACME-DNS-Config/")!)
    }

    @ViewBuilder
    private func input(_ field: ACMEDNSField) -> some View {
        if field.secret {
            SecureField(field.title, text: valueBinding(field.key))
        } else {
            TextField(field.title, text: valueBinding(field.key))
        }
        if let help = field.help {
            Text(help).font(.caption).foregroundStyle(.secondary)
        }
    }

    private func valueBinding(_ key: String) -> Binding<String> {
        Binding(
            get: { draft.values[key, default: ""] },
            set: { draft.setValue($0, for: key) }
        )
    }
}
