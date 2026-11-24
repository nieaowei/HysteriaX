import SwiftUI

struct TemplateDiffView: View {
    let diff: JSONValue
    var emptyMessage: String = L10n.text("没有检测到配置差异。")
    var toast: ToastCenter? = nil

    private struct Entry: Identifiable {
        let index: Int
        let path: String
        let operation: String
        let before: JSONValue?
        let after: JSONValue?
        var id: String { "\(index):\(operation):\(path)" }

        var displayPath: String {
            path.isEmpty ? "/" : path
        }

        var tone: StatusTone {
            switch operation {
            case "add": .positive
            case "remove": .danger
            default: .warning
            }
        }

        var label: String {
            switch operation {
            case "add": L10n.text("新增")
            case "remove": L10n.text("删除")
            default: L10n.text("修改")
            }
        }
    }

    private var entries: [Entry] {
        (diff.arrayValue ?? []).enumerated().map { index, item in
            Entry(
                index: index,
                path: item["path"].stringValue ?? "",
                operation: item["operation"].stringValue ?? "replace",
                before: item.objectValue?["before"],
                after: item.objectValue?["after"]
            )
        }
    }

    var body: some View {
        let items = entries
        if items.isEmpty {
            HStack(spacing: 8) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                Text(emptyMessage)
                    .foregroundStyle(.secondary)
            }
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            let addCount = items.filter { $0.operation == "add" }.count
            let replaceCount = items.filter { $0.operation == "replace" }.count
            let removeCount = items.filter { $0.operation == "remove" }.count
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 8) {
                    if addCount > 0 {
                        StatusBadge(L10n.text("新增 {0}", String(addCount)), tone: .positive, size: .compact, showDot: false)
                    }
                    if replaceCount > 0 {
                        StatusBadge(L10n.text("修改 {0}", String(replaceCount)), tone: .warning, size: .compact, showDot: false)
                    }
                    if removeCount > 0 {
                        StatusBadge(L10n.text("删除 {0}", String(removeCount)), tone: .danger, size: .compact, showDot: false)
                    }
                    Spacer()
                }
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(items) { item in
                        entryCard(item)
                    }
                }
                DisclosureGroup(L10n.text("查看原始 JSON 差异")) {
                    Text(diff.pretty)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                        .background(.quaternary.opacity(0.25), in: RoundedRectangle(cornerRadius: 6))
                }
                .font(.caption)
            }
        }
    }

    @ViewBuilder
    private func entryCard(_ item: Entry) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                StatusBadge(item.label, tone: item.tone, size: .compact, showDot: false)
                Text(item.displayPath)
                    .font(.system(.caption, design: .monospaced).weight(.semibold))
                    .textSelection(.enabled)
                    .lineLimit(2)
                Spacer()
                CopyActionButton(value: item.displayPath, title: L10n.text("已复制字段路径"), toast: toast)
            }
            switch item.operation {
            case "add":
                if let after = item.after {
                    valueBlock(after, label: nil, color: .green)
                }
            case "remove":
                if let before = item.before {
                    valueBlock(before, label: nil, color: .red)
                }
            default:
                let beforeText = formatted(item.before ?? .null)
                let afterText = formatted(item.after ?? .null)
                if !beforeText.contains("\n"), !afterText.contains("\n"), beforeText.count + afterText.count <= 60 {
                    HStack(alignment: .top, spacing: 8) {
                        valueTextBlock(beforeText, label: L10n.text("变更前"), color: .red)
                        Image(systemName: "arrow.right")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .padding(.top, 18)
                        valueTextBlock(afterText, label: L10n.text("变更后"), color: .green)
                    }
                } else {
                    VStack(alignment: .leading, spacing: 6) {
                        valueTextBlock(beforeText, label: L10n.text("变更前"), color: .red)
                        valueTextBlock(afterText, label: L10n.text("变更后"), color: .green)
                    }
                }
            }
        }
        .padding(10)
        .background(.quaternary.opacity(0.18), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(.primary.opacity(0.08), lineWidth: 0.5)
        )
    }

    private func valueBlock(_ value: JSONValue, label: String?, color: Color) -> some View {
        valueTextBlock(formatted(value), label: label, color: color)
    }

    private func valueTextBlock(_ text: String, label: String?, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            if let label {
                Text(label)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Text(text)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(7)
                .background(color.opacity(0.08), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
        }
    }

    private func formatted(_ value: JSONValue) -> String {
        switch value {
        case .string(let text): text.isEmpty ? "\"\"" : text
        case .integer(let number): String(number)
        case .unsignedInteger(let number): String(number)
        case .bool(let flag): flag ? "true" : "false"
        case .decimal(let number): String(number)
        case .null: "null"
        case .array, .object: value.pretty
        }
    }
}
