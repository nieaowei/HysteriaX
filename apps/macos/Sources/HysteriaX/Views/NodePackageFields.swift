import SwiftUI

struct NodePackageFields: View {
    @Binding var draft: NodePackageDraft

    var body: some View {
        Toggle(L10n.text("设置到期时间"), isOn: $draft.hasExpiry)
        if draft.hasExpiry {
            DatePicker(L10n.text("到期时间（本地显示）"), selection: $draft.expiry)
            Stepper(L10n.text("提前 {0} 天提醒", String(describing: (draft.warningDays))), value: $draft.warningDays, in: 0...365)
        }
        Toggle(L10n.text("设置流量额度"), isOn: $draft.hasQuota)
        if draft.hasQuota {
            TextField(L10n.text("套餐额度（GB）"), text: $draft.quotaGB)
            Picker(L10n.text("计费周期"), selection: $draft.cycle) {
                Text(L10n.text("固定套餐累计")).tag("fixed")
                Text(L10n.text("每月重置")).tag("monthly")
            }
            if draft.cycle == "monthly" {
                Stepper(L10n.text("每月 {0} 日重置", String(describing: (draft.resetDay))), value: $draft.resetDay, in: 1...31)
                TextField(L10n.text("计费时区"), text: $draft.timezone)
                Text(L10n.text("按计费时区零点重置；不足指定日期的月份使用月末。"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            TextField(L10n.text("网卡（留空自动识别）"), text: $draft.interface)
            Picker(L10n.text("计费方向"), selection: $draft.direction) {
                Text(L10n.text("出站 + 入站")).tag("both")
                Text(L10n.text("仅出站")).tag("tx")
                Text(L10n.text("仅入站")).tag("rx")
            }
            Stepper(L10n.text("已用 {0}% 时预警", String(describing: (draft.warningPercent))), value: $draft.warningPercent, in: 1...99)
        }
        Text(L10n.text("到期或流量耗尽后自动限制此节点的代理，续期或重置后自动恢复。网卡统计包含其他服务流量，1 GB = 10亿字节，与供应商账单可能有差异。"))
            .font(.callout).foregroundStyle(.secondary)
    }
}

struct NodePackageStatus: View {
    let package: NodePackage?
    let usage: NodePackageUsage?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("\(PackageDisplay.expiry(package)) · \(PackageDisplay.usage(package, usage))")
            if package?.expiresAt != nil { Text(L10n.text("到期：{0}", String(describing: (DateDisplayText.local(package?.expiresAt))))) }
            if let next = usage?.nextResetAt { Text(L10n.text("下次重置：{0}", String(describing: (DateDisplayText.local(next))))) }
            if package?.quotaBytes != nil {
                Text(L10n.text("网卡：{0} · 采样：{1}", String(describing: (usage?.interface ?? L10n.text("尚未识别"))), String(describing: (DateDisplayText.local(usage?.sampledAt)))))
                if usage?.freshness != "fresh" { Text(L10n.text("网卡统计尚未采集或已陈旧，保留已知用量。")) .foregroundStyle(.orange) }
                if let gap = usage?.gapReason { Text(L10n.text("统计缺口：{0}", String(describing: (PackageDisplay.gap(gap))))).foregroundStyle(.orange) }
            }
            if let usage {
                ForEach(usage.alerts, id: \.id) { alert in
                    Label(PackageDisplay.warning(alert.kind), systemImage: "exclamationmark.triangle")
                        .foregroundStyle(usage.restricted ? Color.red : Color.orange)
                }
                if usage.restricted {
                    Text(L10n.text("停用原因：{0}", String(describing: (usage.reasons.map(PackageDisplay.warning).joined(separator: "、"))))).foregroundStyle(.red)
                    if (usage.pendingDisconnects ?? 0) > 0 { Text(L10n.text("{0} 项断开连接任务待完成", String(describing: (usage.pendingDisconnects ?? 0)))).foregroundStyle(.orange) }
                    if (usage.failedDisconnects ?? 0) > 0 { Text(L10n.text("断开连接失败，请检查节点 SSH 和任务记录。")).foregroundStyle(.red) }
                }
            }
        }
        .font(.callout)
    }
}

struct NodePackageManagementView: View {
    @Bindable var store: ManagementStore
    let detail: NodeDetail
    let onUpdated: (NodeDetail) -> Void
    let onSavingChanged: (Bool) -> Void
    @State private var draft = NodePackageDraft()
    @State private var usageGB = "0"
    @State private var isSaving = false
    @State private var message: String?
    @State private var showingReset = false

    init(store: ManagementStore, detail: NodeDetail, onUpdated: @escaping (NodeDetail) -> Void, onSavingChanged: @escaping (Bool) -> Void = { _ in }) {
        self.store = store
        self.detail = detail
        self.onUpdated = onUpdated
        self.onSavingChanged = onSavingChanged
        _draft = State(initialValue: NodePackageDraft(detail.package))
        _usageGB = State(initialValue: NodePackageDraft.gbText(detail.packageUsage?.usageBytes ?? 0))
    }

    var body: some View {
        NodePackageStatus(package: detail.package, usage: detail.packageUsage)
        NodePackageFields(draft: $draft)
        HStack {
            Spacer()
            Button(L10n.text("保存套餐（立即生效）")) { savePackage() }
                .disabled(isSaving || !store.isConnected || detail.package == nil)
        }
        TextField(L10n.text("已有用量 / 校正用量（GB）"), text: $usageGB)
        HStack {
            Spacer()
            Button(L10n.text("校正用量")) { updateUsage(reset: false) }
            Button(L10n.text("重置为零")) { showingReset = true }
        }
        .disabled(isSaving || !store.isConnected || detail.package == nil)
        Text(L10n.text("套餐和用量通过本区按钮独立保存。校正将建立新的采集基线；重置为零还会开启新的提醒周期，保留每月重置日。"))
            .font(.caption).foregroundStyle(.secondary)
        if detail.package == nil { Text(L10n.text("请先升级管理服务以使用套餐功能。")).foregroundStyle(.orange) }
        if let message { Text(message).foregroundStyle(.secondary) }
        if isSaving { ProgressView().controlSize(.small) }
        Color.clear.frame(height: 0)
        .confirmationDialog(L10n.text("将本节点用量重置为零？"), isPresented: $showingReset) {
            Button(L10n.text("重置为零")) { updateUsage(reset: true) }
        } message: { Text(L10n.text("若没有其他限制，节点将自动恢复代理服务。")) }
    }

    private func savePackage() {
        do {
            let package = try draft.package()
            perform {
                try await store.updateNodePackage(detail, package: package)
            }
        } catch { message = error.localizedDescription }
    }
    private func updateUsage(reset: Bool) {
        do {
            let usage = reset ? 0 : try NodePackageDraft.bytes(usageGB)
            perform { try await store.updateNodePackageUsage(detail, usage: usage, reset: reset) }
        } catch { message = error.localizedDescription }
    }
    private func perform(_ action: @escaping @MainActor () async throws -> Void) {
        isSaving = true
        onSavingChanged(true)
        message = nil
        Task {
            defer { isSaving = false; onSavingChanged(false) }
            do {
                try await action()
                let updated = try await store.nodeDetail(detail.id)
                onUpdated(updated)
                usageGB = NodePackageDraft.gbText(updated.packageUsage?.usageBytes ?? 0)
                message = L10n.text("已保存。")
            } catch { message = error.localizedDescription }
        }
    }
}
