import AppKit
import SwiftUI

@MainActor
struct TemplateSourceEditor: View {
    @Bindable var model: SubscriptionTemplateStore
    var toast: ToastCenter? = nil

    private var isMihomo: Bool {
        (model.selected?.format ?? model.catalog["format"].stringValue) == "mihomo"
    }

    private var lineCount: Int {
        guard !model.source.isEmpty else { return 1 }
        return model.source.reduce(into: 1) { count, character in
            if character == "\n" { count += 1 }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                StatusBadge(isMihomo ? "YAML · Mihomo" : "JSON · sing-box", tone: .accent, size: .compact, showDot: false)
                Text(L10n.text("{0} 行 · {1} 字符", String(lineCount), String(model.source.count)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                Text(L10n.text("支持 ⌘F 查找；节点注入在左侧独立导航中管理。"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer()
                if model.source != model.sourceBaseline {
                    Button {
                        model.source = model.sourceBaseline
                        toast?.show(L10n.text("已恢复上次同步源码"), tone: .neutral, symbol: "arrow.uturn.backward")
                    } label: {
                        Label(L10n.text("恢复上次同步"), systemImage: "arrow.uturn.backward")
                            .font(.caption)
                    }
                    .buttonStyle(.borderless)
                    .disabled(model.busy)
                }
                CopyActionButton(value: model.source, title: L10n.text("已复制源码"), toast: toast)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(.quaternary.opacity(0.16))
            Divider()
            MonospacedSourceTextView(
                text: $model.source,
                isEditable: !model.busy,
                identifier: "templates.source"
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

@MainActor
private struct MonospacedSourceTextView: NSViewRepresentable {
    @Binding var text: String
    let isEditable: Bool
    let identifier: String

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = true
        scrollView.backgroundColor = .textBackgroundColor

        let textView = NSTextView()
        textView.minSize = NSSize(width: 0, height: 0)
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.textContainer?.containerSize = NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainerInset = NSSize(width: 8, height: 10)
        textView.font = NSFont.monospacedSystemFont(ofSize: 12.5, weight: .regular)
        textView.textColor = .textColor
        textView.backgroundColor = .textBackgroundColor
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isContinuousSpellCheckingEnabled = false
        textView.isGrammarCheckingEnabled = false
        textView.isAutomaticDataDetectionEnabled = false
        textView.isAutomaticLinkDetectionEnabled = false
        textView.smartInsertDeleteEnabled = false
        textView.isEditable = isEditable
        textView.isSelectable = true
        textView.string = text
        textView.setAccessibilityIdentifier(identifier)
        textView.delegate = context.coordinator

        scrollView.documentView = textView
        scrollView.contentView.postsBoundsChangedNotifications = true

        let ruler = SourceLineNumberRulerView(textView: textView, scrollView: scrollView)
        scrollView.verticalRulerView = ruler
        scrollView.hasVerticalRuler = true
        scrollView.rulersVisible = true

        context.coordinator.textView = textView
        context.coordinator.rulerView = ruler
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        context.coordinator.parent = self
        guard let textView = context.coordinator.textView else { return }
        if textView.isEditable != isEditable {
            textView.isEditable = isEditable
        }
        if textView.accessibilityIdentifier() != identifier {
            textView.setAccessibilityIdentifier(identifier)
        }
        if !context.coordinator.isUpdatingFromTextView, textView.string != text {
            let selected = textView.selectedRanges
            textView.string = text
            let maxLength = (text as NSString).length
            let clamped = selected.compactMap { value -> NSValue? in
                let range = value.rangeValue
                let location = min(range.location, maxLength)
                let length = min(range.length, maxLength - location)
                return NSValue(range: NSRange(location: location, length: length))
            }
            if !clamped.isEmpty {
                textView.selectedRanges = clamped
            }
            context.coordinator.rulerView?.updateThicknessAndRedraw()
        }
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MonospacedSourceTextView
        weak var textView: NSTextView?
        weak var rulerView: SourceLineNumberRulerView?
        var isUpdatingFromTextView = false

        init(_ parent: MonospacedSourceTextView) {
            self.parent = parent
        }

        func textDidChange(_ notification: Notification) {
            guard let textView else { return }
            isUpdatingFromTextView = true
            parent.text = textView.string
            isUpdatingFromTextView = false
            rulerView?.updateThicknessAndRedraw()
        }
    }
}

@MainActor
private final class SourceLineNumberRulerView: NSRulerView {
    private weak var textView: NSTextView?
    nonisolated(unsafe) private var observers: [NSObjectProtocol] = []

    init(textView: NSTextView, scrollView: NSScrollView) {
        self.textView = textView
        super.init(scrollView: scrollView, orientation: .verticalRuler)
        self.clientView = textView
        self.ruleThickness = 40

        let boundsObserver = NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification,
            object: scrollView.contentView,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.needsDisplay = true
            }
        }
        let frameObserver = NotificationCenter.default.addObserver(
            forName: NSView.frameDidChangeNotification,
            object: textView,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.updateThicknessAndRedraw()
            }
        }
        observers = [boundsObserver, frameObserver]
    }

    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    deinit {
        for token in observers {
            NotificationCenter.default.removeObserver(token)
        }
    }

    func updateThicknessAndRedraw() {
        let string = (textView?.string ?? "") as NSString
        var lines = 1
        var index = 0
        while index < string.length {
            if string.character(at: index) == 0x0A {
                lines += 1
            }
            index += 1
        }
        let digits = max(2, String(lines).count)
        let targetThickness = max(40, CGFloat(digits) * 7.5 + 18)
        if abs(ruleThickness - targetThickness) > 0.5 {
            ruleThickness = targetThickness
        }
        needsDisplay = true
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        NSColor.textBackgroundColor.setFill()
        bounds.fill()

        NSColor.separatorColor.setStroke()
        let border = NSBezierPath()
        border.move(to: NSPoint(x: bounds.maxX - 0.5, y: rect.minY))
        border.line(to: NSPoint(x: bounds.maxX - 0.5, y: rect.maxY))
        border.lineWidth = 1
        border.stroke()

        guard let textView,
              let layoutManager = textView.layoutManager,
              let textContainer = textView.textContainer,
              let scrollView else { return }

        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 10.5, weight: .regular),
            .foregroundColor: NSColor.tertiaryLabelColor,
        ]

        let visibleRect = scrollView.contentView.bounds
        let visibleGlyphRange = layoutManager.glyphRange(forBoundingRect: visibleRect, in: textContainer)
        let visibleCharRange = layoutManager.characterRange(forGlyphRange: visibleGlyphRange, actualGlyphRange: nil)
        let nsString = textView.string as NSString
        let insetHeight = textView.textContainerInset.height

        func drawNumber(_ number: Int, atTextY yInTextView: CGFloat, lineHeight: CGFloat) {
            let label = String(number) as NSString
            let size = label.size(withAttributes: attributes)
            let converted = convert(NSPoint(x: 0, y: yInTextView + insetHeight), from: textView)
            let drawY = converted.y + (lineHeight - size.height) * 0.5
            let drawX = bounds.width - size.width - 8
            label.draw(at: NSPoint(x: drawX, y: drawY), withAttributes: attributes)
        }

        if nsString.length == 0 {
            let fontHeight = textView.font?.boundingRectForFont.height ?? 16
            drawNumber(1, atTextY: 0, lineHeight: fontHeight)
            return
        }

        var lineNumber = 1
        var scanIndex = 0
        while scanIndex < visibleCharRange.location && scanIndex < nsString.length {
            if nsString.character(at: scanIndex) == 0x0A {
                lineNumber += 1
            }
            scanIndex += 1
        }
        // Align scan to start of the first visible logical line if visibleCharRange begins mid-line.
        var lineStart = nsString.lineRange(for: NSRange(location: visibleCharRange.location, length: 0)).location
        if lineStart < visibleCharRange.location {
            // Do not double count if visibleCharRange.location was mid-line; lineNumber is already the line at visibleCharRange.location.
        }

        let endLocation = NSMaxRange(visibleCharRange)
        while lineStart <= endLocation && lineStart < nsString.length {
            let lineRange = nsString.lineRange(for: NSRange(location: lineStart, length: 0))
            let glyphIndex = layoutManager.glyphIndexForCharacter(at: lineRange.location)
            let lineRect = layoutManager.lineFragmentRect(forGlyphAt: glyphIndex, effectiveRange: nil, withoutAdditionalLayout: true)
            drawNumber(lineNumber, atTextY: lineRect.minY, lineHeight: lineRect.height)
            lineNumber += 1
            lineStart = NSMaxRange(lineRange)
            if lineRange.length == 0 { break }
        }

        if endLocation >= nsString.length, nsString.length > 0, nsString.character(at: nsString.length - 1) == 0x0A {
            let extraRect = layoutManager.extraLineFragmentRect
            if extraRect.height > 0 {
                drawNumber(lineNumber, atTextY: extraRect.minY, lineHeight: extraRect.height)
            }
        }
    }
}
