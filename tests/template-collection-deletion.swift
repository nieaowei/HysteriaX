import AppKit
import SwiftUI

// The regression exercises collection rows; no editor sheet is opened.
struct TemplateStructuredFieldEditor: View {
    let store: SubscriptionTemplateStore
    let definition: JSONValue
    let path: [String]
    let depth: Int
    let title: String
    var body: some View { Text(title) }
}

@main struct TemplateCollectionDeletionChecks {
    @MainActor static func main() throws {
        _ = NSApplication.shared
        let store = SubscriptionTemplateStore()
        let resolver: JSONValue = .object(["server": .string("dns-main")])
        let path = ["outbounds", "0", "domain_resolver"]
        store.document = .object(["schema_version": .integer(2), "common": .object([
            "outbounds": .array([.object(["tag": .string("direct"), "domain_resolver": resolver])])
        ])])
        store.collectionSelections[SubscriptionTemplateStore.pointer(path)] = "0"
        let host = NSHostingView(rootView: TemplateCollectionFields(store: store, definition: .object(["type": .string("object")]), path: path, depth: 0, isSingleObject: true))
        let window = NSWindow(contentRect: NSRect(x: -2000, y: -2000, width: 800, height: 400), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.contentView = nil; window.close() }
        window.orderFront(nil)
        func settle() {
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.15))
            host.layoutSubtreeIfNeeded()
        }
        settle()
        for _ in 0..<5 {
            store.deleteField(parent: Array(path.dropLast()), key: path.last!)
            settle()
            precondition(store.effective.at(path) == nil)
            precondition(store.effective["outbounds"].arrayValue?.count == 1)
            store.undo()
            settle()
            precondition(store.effective.at(path) == resolver)
            store.redo()
            settle()
            store.set(path, resolver)
            settle()
        }
        print("Resolver collection delete/undo/redo/re-add: passed")

        // Legacy domain_resolver strings use the same overview as objects.
        do {
            let resolverPath = path
            store.set(resolverPath, .string("legacy-dns"))
            host.rootView = TemplateCollectionFields(store: store, definition: .object(["type": .string("object")]), path: resolverPath, depth: 0, isSingleObject: true)
            settle()
            store.deleteField(parent: Array(resolverPath.dropLast()), key: resolverPath.last!)
            settle()
            precondition(store.effective.at(resolverPath) == nil)
            store.undo()
            settle()
            precondition(store.effective.at(resolverPath) == .string("legacy-dns"))
        }

        // Exercise removal of middle, last and final rows in ordinary collections too.
        for collectionPath in [["dns", "servers"], ["dns", "rules"]] {
            let values: [JSONValue] = (0..<3).map { .object(["tag": .string("item-\($0)")]) }
            store.set(collectionPath, .array(values))
            host.rootView = TemplateCollectionFields(store: store, definition: .object(["type": .string("object")]), path: collectionPath, depth: 0)
            settle()
            for index in [1, 1, 0] {
                store.deleteItem(path: collectionPath, index: index)
                settle()
            }
            precondition(store.effective.at(collectionPath) == .array([]))
            for _ in 0..<3 { store.undo(); settle() }
            precondition(store.effective.at(collectionPath) == .array(values))
            for _ in 0..<3 { store.redo(); settle() }
            precondition(store.effective.at(collectionPath) == .array([]))
        }
        print("Legacy resolvers and server/rule collection deletion: passed")
        let largePath = ["dns", "servers"]
        let largeValues: [JSONValue] = (0..<20).map { .object(["tag": .string("server-\($0)"), "type": .string("https"), "server": .string("example.com")]) }
        store.set(largePath, .array(largeValues))
        host.rootView = TemplateCollectionFields(store: store, definition: .object(["type": .string("object")]), path: largePath, depth: 0)
        settle()
        precondition(host.fittingSize.height > 700, "Collection must grow with its rows instead of imposing a clipped inner list")
        print("Large collection uses natural row height: passed")
        store.catalog = .object(["format": .string("singbox"), "schema": try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1])))])
        store.document = .object(["schema_version": .integer(2), "common": .object([
            "dns": .object(["servers": .array([.object(["type": .string("udp"), "tag": .string("dns-main"), "server": .string("1.1.1.1")])]),
                "rules": .array([.object(["server": .string("dns-main"), "action": .string("route")])])])
        ])])
        host.rootView = TemplateCollectionFields(store: store, definition: store.schema.definition(at: ["dns", "servers"], value: store.effective)["items"], path: ["dns", "servers"], depth: 0)
        settle()
        let original = store.document
        let undoCount = store.undoStack.count
        store.deleteItem(path: ["dns", "servers"], index: 0); settle()
        precondition(store.document == original && store.pendingDeletion != nil && !window.sheets.isEmpty, "Referenced deletion must present a preview before changing the draft")
        store.pendingDeletion = nil; settle(); settle()
        precondition(store.document == original && window.sheets.isEmpty)
        store.deleteItem(path: ["dns", "servers"], index: 0); settle()
        store.confirmCascadeDeletion(); settle(); settle()
        precondition(store.effective["dns"]["servers"] == .array([]) && store.effective["dns"]["rules"] == .array([]))
        precondition(store.undoStack.count == undoCount + 1 && window.sheets.isEmpty)
        store.undo(); settle(); precondition(store.document == original)
        print("Native cascade preview presentation, cancel, confirmation, atomic removal and undo: passed")
    }
}
