import SwiftUI

/// Finite choices use checkboxes; free-form lists use tags with optional bulk editing.
struct TemplateScalarListEditor: View {
    @Bindable var store: SubscriptionTemplateStore
    let path: [String]
    let itemDefinition: JSONValue
    var allowsScalar = false
    private var schema: JSONValue { store.schema.resolve(itemDefinition) }
    private var choices: [JSONValue]? {
        if let reference = schema["x-tag-reference"].stringValue { return store.referenceNames(reference).map(JSONValue.string) }
        return schema["enum"].arrayValue ?? schema["x-enum-choices"].arrayValue
    }
    private var values: Binding<[JSONValue]> {
        Binding(get: {
            let value = store.effective.at(path)
            if let value, value != .null, value.arrayValue == nil,
               allowsScalar || value.schemaType == store.schema.resolve(itemDefinition)["type"].stringValue { return [value] }
            return value?.arrayValue ?? []
        }, set: { values in
            if allowsScalar, values.count == 1, store.effective.at(path)?.arrayValue == nil {
                store.set(path, values[0])
            } else {
                store.set(path, .array(values))
            }
        })
    }
    private var selectableValues: [JSONValue] {
        ((choices ?? []) + values.wrappedValue).reduce(into: []) { result, value in
            if !result.contains(value) { result.append(value) }
        }
    }
    var body: some View {
        if choices != nil {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(selectableValues.enumerated()), id: \.offset) { _, choice in
                    HStack {
                        Spacer()
                        Text(TemplateSchema.pickerTitle(choice))
                            .multilineTextAlignment(.trailing)
                        Toggle(TemplateSchema.pickerTitle(choice), isOn: Binding(get: {
                            values.wrappedValue.contains(choice)
                        }, set: { selected in
                            var next = values.wrappedValue
                            if selected {
                                if !next.contains(choice) { next.append(choice) }
                            } else { next.removeAll { $0 == choice } }
                            values.wrappedValue = next
                        }))
                        .toggleStyle(.checkbox)
                        .labelsHidden()
                        .accessibilityIdentifier("templates.choice.\(SubscriptionTemplateStore.pointer(path)).\(choice.stringValue ?? choice.pretty)")
                    }
                }
                if selectableValues.isEmpty { Text(L10n.text("暂无条目")).foregroundStyle(.secondary) }
            }
        } else {
            TemplateLineListInput(values: values, identifier: "templates.lines." + SubscriptionTemplateStore.pointer(path), integers: schema["type"].stringValue == "integer", prefersMultiline: path.last?.localizedCaseInsensitiveContains("regex") == true || path.last?.localizedCaseInsensitiveContains("url") == true)
            .id(store.layer)
        }
    }
}

struct TemplateLineListInput: View {
    @Binding var values: [JSONValue]
    let identifier: String
    let integers: Bool
    @State private var tokens: TemplateTokenListDraft
    @State private var input = ""
    @State private var editingID: UUID?
    @State private var inputHasError = false
    @State private var bulkText: String
    @State private var isBulkEditing: Bool
    @State private var dragScope = UUID()
    @FocusState private var inputFocused: Bool

    init(values: Binding<[JSONValue]>, identifier: String, integers: Bool = false, prefersMultiline: Bool = false) {
        _values = values
        self.identifier = identifier
        self.integers = integers
        _tokens = State(initialValue: TemplateTokenListDraft(values.wrappedValue))
        _bulkText = State(initialValue: TemplateLineListCodec.text(values.wrappedValue))
        _isBulkEditing = State(initialValue: prefersMultiline || Self.needsBulk(values.wrappedValue))
    }
    private static func needsBulk(_ values: [JSONValue]) -> Bool {
        values.count > 50 || values.contains {
            let text = $0.stringValue ?? $0.pretty
            return text.count > 120 || text.contains(where: \.isNewline)
        }
    }
    private var parsedBulk: [JSONValue]? { TemplateLineListCodec.parse(bulkText, integers: integers) }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(L10n.text("共 {0} 项", String(values.count))).font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button(L10n.text(isBulkEditing ? "标签编辑" : "批量编辑")) { switchMode() }
                    .buttonStyle(.borderless)
                    .disabled(isBulkEditing && parsedBulk == nil)
                    .accessibilityIdentifier(identifier + ".mode")
            }
            if isBulkEditing {
                Text(L10n.text("每行一项，支持粘贴多行内容")).font(.caption).foregroundStyle(.secondary)
                TextEditor(text: $bulkText)
                    .font(.system(.body, design: .monospaced))
                    .frame(height: 130)
                    .accessibilityLabel(L10n.text("每行一项，支持粘贴多行内容"))
                    .accessibilityIdentifier(identifier)
                if parsedBulk == nil { validationError }
            } else {
                if !tokens.tokens.isEmpty {
                    TemplateTokenFlow {
                        ForEach(tokens.tokens) { token in tokenView(token) }
                    }
                }
                HStack {
                    TextField(L10n.text("输入后按 Enter 添加，可粘贴多行"), text: $input, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .focused($inputFocused)
                        .onSubmit { submitInput() }
                        .onExitCommand { cancelInput() }
                        .accessibilityIdentifier(identifier + ".input")
                    Button(L10n.text(editingID == nil ? "添加条目" : "完成"), systemImage: editingID == nil ? "plus" : "checkmark") { submitInput() }
                        .labelStyle(.iconOnly).buttonStyle(.borderless)
                        .disabled(input.isEmpty)
                        .accessibilityIdentifier(identifier + ".add")
                }
                if inputHasError { validationError }
            }
        }
        .onChange(of: input) { previous, next in
            inputHasError = false
            if editingID == nil, !previous.contains(where: \.isNewline), next.contains(where: \.isNewline) { submitInput() }
        }
        .onChange(of: bulkText) { _, _ in
            guard bulkText != TemplateLineListCodec.text(values), let parsedBulk, parsedBulk != values else { return }
            values = parsedBulk
        }
        .onChange(of: values) { _, next in
            tokens.synchronize(next)
            if parsedBulk != next { bulkText = TemplateLineListCodec.text(next) }
            if let editingID, !tokens.tokens.contains(where: { $0.id == editingID }) { cancelInput() }
        }
    }
    private var validationError: some View {
        Text(L10n.text("每行请输入一个有效整数")).font(.caption).foregroundStyle(.red)
    }
    private func tokenView(_ token: TemplateTokenListDraft.Token) -> some View {
        let label = TemplateSchema.pickerTitle(token.value)
        return HStack(spacing: 6) {
            Button {
                editingID = token.id
                input = TemplateLineListCodec.text([token.value])
                inputFocused = true
            } label: { Text(label).lineLimit(1).truncationMode(.middle) }
                .buttonStyle(.plain)
            Button {
                tokens.remove(token.id)
                values = tokens.values
                if editingID == token.id { cancelInput() }
            } label: { Image(systemName: "xmark").font(.caption2) }
                .buttonStyle(.plain)
                .accessibilityLabel(L10n.text("删除 {0}", label))
        }
        .padding(.horizontal, 8).padding(.vertical, 5)
        .background(editingID == token.id ? Color.accentColor.opacity(0.15) : Color.primary.opacity(0.06), in: RoundedRectangle(cornerRadius: 6))
        .help(label)
        .draggable(dragScope.uuidString + ":" + token.id.uuidString)
        .dropDestination(for: String.self) { payloads, _ in
            guard let payload = payloads.first, let source = draggedID(payload), tokens.move(source, to: token.id) else { return false }
            values = tokens.values
            return true
        }
        .contextMenu {
            Button(L10n.text("上移")) { move(token.id, offset: -1) }.disabled(tokens.tokens.first?.id == token.id)
            Button(L10n.text("下移")) { move(token.id, offset: 1) }.disabled(tokens.tokens.last?.id == token.id)
        }
        .accessibilityIdentifier(identifier + ".token." + token.id.uuidString)
    }
    private func draggedID(_ payload: String) -> UUID? {
        let prefix = dragScope.uuidString + ":"
        guard payload.hasPrefix(prefix) else { return nil }
        return UUID(uuidString: String(payload.dropFirst(prefix.count)))
    }
    private func move(_ id: UUID, offset: Int) {
        guard let index = tokens.tokens.firstIndex(where: { $0.id == id }), tokens.tokens.indices.contains(index + offset) else { return }
        let target = tokens.tokens[index + offset].id
        if tokens.move(id, to: target) { values = tokens.values }
    }
    @discardableResult private func submitInput() -> Bool {
        guard !input.isEmpty else { return true }
        guard tokens.submit(input, integers: integers, replacing: editingID) else { inputHasError = true; return false }
        values = tokens.values
        cancelInput()
        if Self.needsBulk(values) { bulkText = TemplateLineListCodec.text(values); isBulkEditing = true }
        return true
    }
    private func cancelInput() { input = ""; editingID = nil; inputHasError = false }
    private func switchMode() {
        if isBulkEditing {
            guard parsedBulk != nil else { return }
            isBulkEditing = false
        } else {
            guard submitInput() else { return }
            bulkText = TemplateLineListCodec.text(values)
            isBulkEditing = true
        }
    }
}

/// Wrap tags to the actual available width instead of introducing a nested scroll view.
private struct TemplateTokenFlow: Layout {
    var spacing: CGFloat = 6
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrangement(width: proposal.width, subviews: subviews).size
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let layout = arrangement(width: bounds.width, subviews: subviews)
        for (index, frame) in layout.frames.enumerated() {
            subviews[index].place(at: CGPoint(x: bounds.minX + frame.minX, y: bounds.minY + frame.minY), anchor: .topLeading, proposal: ProposedViewSize(frame.size))
        }
    }
    private func arrangement(width: CGFloat?, subviews: Subviews) -> (size: CGSize, frames: [CGRect]) {
        let finiteWidth = width.flatMap { $0.isFinite ? max(0, $0) : nil }
        let limit = max(1, finiteWidth ?? .infinity)
        var frames: [CGRect] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        var usedWidth: CGFloat = 0
        for subview in subviews {
            let ideal = subview.sizeThatFits(.unspecified)
            let size = subview.sizeThatFits(ProposedViewSize(width: min(limit, ideal.width), height: nil))
            if x > 0, x + size.width > limit { x = 0; y += rowHeight + spacing; rowHeight = 0 }
            frames.append(CGRect(origin: CGPoint(x: x, y: y), size: size))
            usedWidth = max(usedWidth, x + size.width)
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return (CGSize(width: finiteWidth ?? usedWidth, height: y + rowHeight), frames)
    }
}
