import AppKit
import SwiftUI

struct NodeCreatedView: View {
    let name: String
    let token: String?
    let finish: () -> Void
    @State private var didCopy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            HStack(alignment: .center, spacing: 16) {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 42))
                    .foregroundStyle(.green)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.text("节点添加成功"))
                        .font(.title2.bold())
                    Text(name)
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .textSelection(.enabled)
                }
            }

            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Label(L10n.text("节点认证令牌"), systemImage: "key.fill")
                        .font(.headline)
                    Spacer()
                    if let token, !token.isEmpty {
                        Button {
                            NSPasteboard.general.clearContents()
                            didCopy = NSPasteboard.general.setString(token, forType: .string)
                        } label: {
                            Label(L10n.text(didCopy ? "已复制" : "复制令牌"), systemImage: didCopy ? "checkmark" : "doc.on.doc")
                        }
                        .accessibilityIdentifier("node.create.copyToken")
                    }
                }
                if let token, !token.isEmpty {
                    Text(token)
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                        .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
                        .accessibilityIdentifier("node.create.token")
                    Label {
                        Text(L10n.text("令牌仅显示一次，请复制并保存在安全位置。"))
                            .fixedSize(horizontal: false, vertical: true)
                    } icon: {
                        Image(systemName: "info.circle")
                    }
                    .font(.callout)
                    .foregroundStyle(.secondary)
                } else {
                    Text(L10n.text("此前请求已创建节点，令牌不会重复显示。"))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(18)
            .background(.primary.opacity(0.03), in: RoundedRectangle(cornerRadius: 12))
            .overlay {
                RoundedRectangle(cornerRadius: 12)
                    .strokeBorder(.primary.opacity(0.08), lineWidth: 1)
            }

            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "slider.horizontal.3")
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.text("下一步：配置 TLS 证书"))
                        .font(.headline)
                    Text(L10n.text("节点尚未部署。请在代理配置页设置 TLS 证书，然后部署节点。"))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Divider()
            HStack {
                Spacer()
                Button(L10n.text("完成"), action: finish)
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("node.create.finish")
            }
        }
        .padding(28)
        .frame(width: 640, alignment: .leading)
        .accessibilityIdentifier("node.create.success")
    }
}
