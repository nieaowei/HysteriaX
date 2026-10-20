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
    @MainActor static func main() {
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
    }
}
