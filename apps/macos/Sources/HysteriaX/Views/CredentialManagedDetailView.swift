import SwiftUI

struct CredentialManagedDetailView: View {
    let detail: CredentialDetail
    let isConnected: Bool
    var onJump: (String, String) -> Void
    var onRetry: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            OverviewColumnsLayout(wideColumns: 2, wideMinimum: 576, narrowColumns: 1) {
                references
                updates
            }
            DisclosureGroup("版本历史（\(detail.versions.count)）") {
                VStack(spacing: 0) {
                    ForEach(detail.versions.sorted { $0.version > $1.version }, id: \.version) { version in
                        HStack {
                            Text("v\(version.version)").monospacedDigit()
                            if version.version == detail.latestVersion {
                                Text("最新").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(DateDisplayText.local(version.createdAt)).foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 6)
                    }
                }
            }
            if let fingerprint = detail.metadata["fingerprint"]?.stringValue {
                DisclosureGroup("指纹") {
                    Text(fingerprint)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, 6)
                }
            }
            if let certificate = detail.metadata["certificate"]?.stringValue {
                DisclosureGroup("公开证书") {
                    Text(certificate)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, 6)
                }
            }
        }
    }

    private var references: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 0) {
                if detail.references.isEmpty {
                    emptyMessage("暂无引用", description: "该凭据尚未被节点或配置使用。")
                }
                ForEach(Array(detail.references.enumerated()), id: \.offset) { index, reference in
                    if index > 0 { Divider() }
                    HStack(alignment: .top, spacing: 12) {
                        VStack(alignment: .leading, spacing: 4) {
                            if reference.entityType == "batch" {
                                Text("更新批次")
                            } else {
                                Button(reference.name ?? reference.entityID) {
                                    onJump(reference.entityType, reference.entityID)
                                }.buttonStyle(.link)
                            }
                            Text(CredentialDisplay.source(reference.source))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 8)
                        Text(reference.version.map { "v\($0)" } ?? "—")
                            .font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 8)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Label("引用与生效版本（\(detail.references.count)）", systemImage: "link")
        }
    }

    private var updates: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                if detail.batches.isEmpty {
                    emptyMessage("暂无更新批次", description: "发布新版本后，可在这里查看生效结果。")
                }
                ForEach(detail.batches) { batch in
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("v\(batch.version)").font(.subheadline.weight(.semibold))
                            Spacer()
                            Text(DateDisplayText.local(batch.createdAt))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        ForEach(Array(batch.items.enumerated()), id: \.offset) { _, item in
                            VStack(alignment: .leading, spacing: 4) {
                                HStack(spacing: 12) {
                                    Button(item.name ?? item.nodeID) { onJump("node", item.nodeID) }
                                        .buttonStyle(.link)
                                    Spacer(minLength: 8)
                                    Text(JobDisplayText.status(item.status))
                                        .font(.caption)
                                        .foregroundStyle(item.errorMessage == nil ? Color.secondary : Color.red)
                                    Button { onJump("job", item.jobId) } label: {
                                        Image(systemName: "arrow.up.right.square")
                                    }
                                    .buttonStyle(.link)
                                    .help("查看任务")
                                    .accessibilityLabel("查看任务")
                                }
                                if let message = item.errorMessage {
                                    Text(message).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                                }
                            }
                        }
                        ForEach(Array((batch.dnsItems ?? []).enumerated()), id: \.offset) { _, item in
                            HStack {
                                Button(item.name) { onJump("dns_connection", item.connectionId) }.buttonStyle(.link)
                                Spacer()
                                Text(JobDisplayText.status(item.status)).font(.caption)
                                Button("查看任务") { onJump("job", item.jobId) }.buttonStyle(.link)
                            }
                            if let error = item.errorMessage { Text(error).font(.caption).foregroundStyle(.red) }
                        }
                        if batch.items.contains(where: { ["failed", "rolled_back", "cancelled"].contains($0.status) }) || (batch.dnsItems ?? []).contains(where: { ["failed", "cancelled"].contains($0.status) }) {
                            Button("重试失败项") { onRetry(batch.id) }
                                .disabled(!isConnected || detail.archived || batch.version != detail.latestVersion)
                        }
                    }
                    if batch.id != detail.batches.last?.id { Divider() }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Label("更新结果（\(detail.batches.count)）", systemImage: "arrow.triangle.2.circlepath")
        }
    }

    private func emptyMessage(_ title: String, description: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).foregroundStyle(.secondary)
            Text(description).font(.caption).foregroundStyle(.tertiary)
        }
        .padding(.vertical, 8)
    }
}
