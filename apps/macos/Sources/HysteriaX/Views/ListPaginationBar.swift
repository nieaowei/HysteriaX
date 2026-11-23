import SwiftUI

struct ListPaginationBar: View {
    let summary: String?
    let page: Int
    let pageCount: Int
    @Binding var pageSize: Int
    var pageSizes: [Int] = [25, 50, 100]
    let isLoading: Bool
    var isOffline: Bool = false
    var offlineText: String? = nil
    var error: String? = nil
    var onRetry: (() -> Void)? = nil
    var canRetry: Bool = true
    var canNavigate: Bool = true
    let identifierPrefix: String
    var pageSizeIdentifier: String? = nil
    let onSelectPage: (Int) -> Void

    @State private var showingPageJump = false
    @State private var pageInput = ""

    private var effectivePageSizeIdentifier: String {
        pageSizeIdentifier ?? "\(identifierPrefix).pageSize"
    }

    private var navigationDisabled: Bool {
        isLoading || !canNavigate
    }

    var body: some View {
        VStack(spacing: 0) {
            Divider()
            VStack(alignment: .leading, spacing: 6) {
                if let error {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.red)
                            .accessibilityHidden(true)
                        Text(error)
                            .foregroundStyle(.red)
                            .textSelection(.enabled)
                        if let onRetry {
                            Button(L10n.text("重试"), action: onRetry)
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                                .disabled(!canRetry)
                        }
                    }
                    .font(.caption)
                }
                HStack(spacing: 12) {
                    if isOffline {
                        Label(offlineText ?? L10n.text("离线快照"), systemImage: "wifi.slash")
                            .foregroundStyle(.secondary)
                    }
                    if let summary {
                        Text(summary)
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                    }
                    if isLoading {
                        ProgressView()
                            .controlSize(.small)
                    }
                    Spacer(minLength: 0)
                    Picker(L10n.text("每页条数"), selection: $pageSize) {
                        ForEach(pageSizes, id: \.self) { size in
                            Text(String(size)).tag(size)
                        }
                    }
                    .fixedSize()
                    .accessibilityIdentifier(effectivePageSizeIdentifier)

                    HStack(spacing: 6) {
                        Button {
                            onSelectPage(max(1, page - 1))
                        } label: {
                            HStack(spacing: 3) {
                                Image(systemName: "chevron.left")
                                    .font(.caption2.weight(.semibold))
                                Text(L10n.text("上一页"))
                            }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(page <= 1 || navigationDisabled)
                        .accessibilityLabel(L10n.text("上一页"))
                        .accessibilityIdentifier("\(identifierPrefix).previousPage")

                        if pageCount > 2 {
                            Button {
                                pageInput = String(page)
                                showingPageJump.toggle()
                            } label: {
                                Text("\(page) / \(pageCount)")
                                    .monospacedDigit()
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .disabled(navigationDisabled)
                            .help(L10n.text("跳转到指定页"))
                            .accessibilityLabel(L10n.text("跳转到指定页"))
                            .popover(isPresented: $showingPageJump, arrowEdge: .top) {
                                pageJumpPopover
                            }
                        }

                        Button {
                            onSelectPage(min(pageCount, page + 1))
                        } label: {
                            HStack(spacing: 3) {
                                Text(L10n.text("下一页"))
                                Image(systemName: "chevron.right")
                                    .font(.caption2.weight(.semibold))
                            }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(page >= pageCount || navigationDisabled)
                        .accessibilityLabel(L10n.text("下一页"))
                        .accessibilityIdentifier("\(identifierPrefix).nextPage")
                    }
                }
                .font(.callout)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(.bar)
            .accessibilityIdentifier("\(identifierPrefix).pagination")
        }
    }

    private var pageJumpPopover: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(L10n.text("跳转到指定页"))
                .font(.headline)
            HStack(spacing: 8) {
                TextField(L10n.text("页码"), text: $pageInput)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 72)
                    .monospacedDigit()
                    .onSubmit { submitJump() }
                Button(L10n.text("跳转")) { submitJump() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(parsedTargetPage == nil)
            }
            HStack(spacing: 8) {
                Button(L10n.text("首页")) {
                    showingPageJump = false
                    if page != 1 { onSelectPage(1) }
                }
                .disabled(page <= 1)
                Spacer()
                Button(L10n.text("末页")) {
                    showingPageJump = false
                    if page != pageCount { onSelectPage(pageCount) }
                }
                .disabled(page >= pageCount)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(12)
        .frame(width: 210)
    }

    private var parsedTargetPage: Int? {
        let trimmed = pageInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value = Int(trimmed) else { return nil }
        return min(pageCount, max(1, value))
    }

    private func submitJump() {
        guard let target = parsedTargetPage else { return }
        showingPageJump = false
        if target != page {
            onSelectPage(target)
        }
    }
}
