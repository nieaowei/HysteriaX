import AppKit
import XCTest

final class SubscriptionTemplateUITests: XCTestCase {
    private let templateID = "__TEMPLATE_ID__"
    private let exportTemplateID = "__EXPORT_TEMPLATE_ID__"
    private let exportDirectory = "__EXPORT_DIR__"
    private let userID = "__USER_ID__"
    private let subscriptionToken = "__SUBSCRIPTION_TOKEN__"
    private let base = "__BASE_URL__"
    private let token = "__ADMIN_TOKEN__"

    @MainActor
    func testTemplateLayoutAtCompactAndWideWidths() throws {
        continueAfterFailure = false
        for width in [920, 1350] {
            let app = XCUIApplication()
            app.launchArguments = ["-appLanguage", "zh-Hans", "-layoutWidth", String(width)]
            app.launch(); app.activate()
            defer { app.terminate() }
            try click(app.descendants(matching: .any)["sidebar.templates"].firstMatch)
            try click(app.descendants(matching: .any)["templates.row.default-singbox"].firstMatch)
            let window = app.windows.firstMatch
            func contained(_ element: XCUIElement) {
                XCTAssertTrue(element.waitForExistence(timeout: 15))
                let bounds = window.frame.insetBy(dx: -1, dy: -1)
                XCTAssertTrue(bounds.contains(element.frame), "Control extends outside window at width \(width): \(element.identifier) \(element.frame), window \(window.frame)")
                XCTAssertTrue(element.isHittable)
            }
            contained(app.descendants(matching: .any)["sidebar.templates"].firstMatch)
            contained(app.textFields["templates.fieldSearch"].firstMatch)
            contained(app.buttons["templates.save"].firstMatch)
            contained(app.buttons["templates.publish"].firstMatch)
            let toggle = app.buttons["templates.inspector.toggle"].firstMatch
            contained(toggle)
            if width == 920 {
                XCTAssertFalse(app.popUpButtons["templates.preview.platform"].firstMatch.exists)
                attach(app, "Compact template editor at 920 points")
                try click(toggle)
                XCTAssertTrue(app.sheets.firstMatch.waitForExistence(timeout: 10))
                contained(app.popUpButtons["templates.preview.user"].firstMatch)
                contained(app.popUpButtons["templates.preview.platform"].firstMatch)
                attach(app, "Compact preview panel")
                try click(app.sheets.buttons["完成"].firstMatch)
            } else {
                contained(app.popUpButtons["templates.preview.user"].firstMatch)
                contained(app.popUpButtons["templates.preview.platform"].firstMatch)
                attach(app, "Wide template editor with inline preview at 1350 points")
                try click(toggle)
                XCTAssertTrue(app.popUpButtons["templates.preview.platform"].firstMatch.waitForNonExistence(timeout: 5))
                contained(app.textFields["templates.fieldSearch"].firstMatch)
                try click(toggle)
                contained(app.popUpButtons["templates.preview.platform"].firstMatch)
            }
            app.terminate()
        }
    }

    @MainActor
    func testSubscriptionSelectionAndNativeFileExports() async throws {
        continueAfterFailure = false
        let app = XCUIApplication(); app.launchArguments = ["-appLanguage", "zh-Hans"]
        app.launch(); app.activate(); defer { app.terminate() }
        try click(app.descendants(matching: .any)["sidebar.templates"].firstMatch)
        try click(app.descendants(matching: .any)["templates.row.\(exportTemplateID)"].firstMatch)
        try menu(app, "导出完整模板…")
        try saveFile(app)
        let packageURL = try XCTUnwrap(try FileManager.default.contentsOfDirectory(at: URL(fileURLWithPath: exportDirectory), includingPropertiesForKeys: nil).first { $0.pathExtension == "json" })
        let packageData = try Data(contentsOf: packageURL)
        let package = try XCTUnwrap(JSONSerialization.jsonObject(with: packageData) as? [String:Any])
        XCTAssertEqual(package["package_version"] as? Int, 1)
        XCTAssertNotNil((package["document"] as? [String:Any])?["platforms"])
        XCTAssertTrue(String(decoding: packageData, as: UTF8.self).contains("18446744073709551615"))
        try menu(app, "导入配置或模板…")
        try replace(app.textViews["templates.import.source"].firstMatch, String(decoding: packageData, as: UTF8.self))
        try click(app.buttons["templates.import.analyze"].firstMatch)
        try click(app.buttons["确认映射并写入本地草稿"].firstMatch)
        try click(app.buttons["templates.save"].firstMatch)
        let restored = try await api("/api/v1/subscription-templates/\(exportTemplateID)/export")
        XCTAssertTrue(NSDictionary(dictionary: restored["document"] as? [String:Any] ?? [:]).isEqual(to: package["document"] as? [String:Any] ?? [:]))
        try click(app.descendants(matching: .any)["sidebar.users"].firstMatch)
        try click(app.descendants(matching: .any)["users.row.\(userID)"].firstMatch)
        try click(app.descendants(matching: .any)["users.subscriptionMenu"].firstMatch)
        try click(app.menuItems["选择模板与目标平台…"].firstMatch)
        try choose(app.popUpButtons["users.subscription.template"].firstMatch, "0005 UI Export Template (mihomo)", app)
        let copy = app.buttons["users.subscription.copyResolved"].firstMatch
        XCTAssertTrue(copy.waitForExistence(timeout: 10)); XCTAssertFalse(copy.isEnabled)
        try choose(app.popUpButtons["users.subscription.platform"].firstMatch, "Windows", app)
        let copied = try await copyAddress(copy)
        let components = try XCTUnwrap(URLComponents(string: copied))
        let query = Dictionary(uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") })
        XCTAssertEqual(query["template"], exportTemplateID); XCTAssertEqual(query["platform"], "windows"); XCTAssertEqual(query["format"], "mihomo")
        let (copiedBody, copiedResponse) = try await URLSession.shared.data(from: XCTUnwrap(components.url))
        XCTAssertEqual((copiedResponse as? HTTPURLResponse)?.statusCode, 200)
        XCTAssertTrue(String(decoding: copiedBody, as: UTF8.self).contains("mixed-port: 7888"))
        try click(app.buttons["users.subscription.exportResolved"].firstMatch)
        try saveFile(app)
        let output = try XCTUnwrap(try FileManager.default.contentsOfDirectory(at: URL(fileURLWithPath: exportDirectory), includingPropertiesForKeys: nil).first { $0.pathExtension == "yaml" })
        let yaml = try String(contentsOf: output, encoding: .utf8)
        XCTAssertTrue(yaml.contains("mixed-port: 7888")); XCTAssertFalse(yaml.contains("tproxy-port"))
        attach(app, "Explicit template/platform selection and native configuration export")
    }
    @MainActor private func copyAddress(_ button: XCUIElement) async throws -> String {
        let pasteboard = NSPasteboard.general
        let previous = (pasteboard.pasteboardItems ?? []).map { item in item.types.compactMap { type in item.data(forType: type).map { (type, $0) } } }
        defer {
            pasteboard.clearContents()
            let items = previous.map { values in let item = NSPasteboardItem(); for (type, data) in values { item.setData(data, forType: type) }; return item }
            if !items.isEmpty { pasteboard.writeObjects(items) }
        }
        try click(button)
        check(try await eventually { pasteboard.string(forType: .string)?.contains("template=" + self.exportTemplateID) == true })
        return try XCTUnwrap(pasteboard.string(forType: .string))
    }
    @MainActor private func saveFile(_ app: XCUIApplication) throws {
        XCTAssertTrue(app.sheets.firstMatch.waitForExistence(timeout: 15))
        app.typeKey("g", modifierFlags: [.command, .shift])
        let field = app.textFields["PathTextField"].firstMatch
        try replace(field, exportDirectory)
        field.typeKey(.return, modifierFlags: [])
        let labels = ["存储", "保存", "Save", "导出", "Export"]
        let save = app.sheets.buttons.matching(NSPredicate(format: "label IN %@ OR title IN %@", labels, labels)).firstMatch
        try click(save)
        XCTAssertTrue(save.waitForNonExistence(timeout: 10))
    }

    @MainActor
    func testTemplateEditorPublicationAndImport() async throws {
        continueAfterFailure = false
        let app = XCUIApplication(); app.launchArguments = ["-appLanguage", "zh-Hans"]
        app.launch(); app.activate()
        defer { app.terminate() }
        try click(app.descendants(matching: .any)["sidebar.templates"].firstMatch)
        try click(app.descendants(matching: .any)["templates.row.\(templateID)"].firstMatch)
        let search = app.textFields["templates.fieldSearch"].firstMatch
        try replace(search, "mixed-port")
        let port = app.textFields["templates.value./mixed-port"].firstMatch
        try replace(port, "8100"); app.typeKey(.tab, modifierFlags: [])
        try click(app.buttons["templates.save"].firstMatch)
        try await waitForCommonPort(8100)

        try click(app.buttons["templates.sourceMode"].firstMatch)
        let source = app.textViews["templates.source"].firstMatch
        XCTAssertTrue(source.waitForExistence(timeout: 10))
        let validSource = try XCTUnwrap(source.value as? String)
        XCTAssertTrue(validSource.contains("mixed-port: 8100"))
        try replace(source, "mixed-port: [")
        try click(app.buttons["templates.sourceMode"].firstMatch)
        XCTAssertTrue(app.descendants(matching: .any)["templates.error"].firstMatch.waitForExistence(timeout: 10))
        XCTAssertEqual(source.value as? String, "mixed-port: [")
        try replace(source, validSource)
        try click(app.buttons["templates.sourceMode"].firstMatch)
        XCTAssertTrue(search.waitForExistence(timeout: 10))

        try replace(search, "rules")
        let kind = app.popUpButtons["templates.rule.kind./rules.0"].firstMatch
        XCTAssertTrue(kind.waitForExistence(timeout: 10)); XCTAssertEqual(kind.value as? String, "MATCH")
        try choose(app.popUpButtons["templates.rule.target./rules.0"].firstMatch, "DIRECT", app)
        try choose(app.popUpButtons["templates.rule.target./rules.0"].firstMatch, "Select", app)
        try replace(search, "mixed-port")
        try choose(app.popUpButtons["templates.layer"].firstMatch, "Windows", app)
        try replace(port, "0"); app.typeKey(.tab, modifierFlags: [])
        try click(app.buttons["templates.save"].firstMatch)
        check(try await eventually {
            let draft = try await self.api("/api/v1/subscription-templates/\(self.templateID)/draft")
            return ((draft["document"] as? [String:Any])?["platforms"] as? [String:Any]).flatMap { $0["windows"] as? [String:Any] }.flatMap { $0["set"] as? [String:Any] }?["mixed-port"] as? Int == 0
        })
        try replace(search, "tproxy-port")
        let tproxy = app.popUpButtons["templates.state./tproxy-port"].firstMatch
        XCTAssertTrue(tproxy.waitForExistence(timeout: 10)); XCTAssertFalse(tproxy.isEnabled)
        try replace(search, "mixed-port")
        try choose(app.popUpButtons["templates.preview.user"].firstMatch, "0005 UI User", app)
        try choose(app.popUpButtons["templates.preview.platform"].firstMatch, "Windows", app)
        try click(app.buttons["templates.preview"].firstMatch)
        XCTAssertTrue(app.disclosureTriangles["最终配置"].firstMatch.waitForExistence(timeout: 10))
        attach(app, "Template fields, platform override, and preview")
        try click(app.buttons["templates.publish"].firstMatch)
        try await waitForVersion(1)
        check(try await download().contains("mixed-port: 0"))

        try replace(port, "9001"); app.typeKey(.tab, modifierFlags: [])
        try click(app.buttons["templates.save"].firstMatch)
        check(try await eventually {
            let value = try await self.api("/api/v1/subscription-templates/\(self.templateID)/draft")
            return (((value["document"] as? [String:Any])?["platforms"] as? [String:Any])?["windows"] as? [String:Any]).flatMap { $0["set"] as? [String:Any] }?["mixed-port"] as? Int == 9001
        })
        check(try await download().contains("mixed-port: 0"), "Saving a draft must not change the subscription")
        try click(app.buttons["templates.publish"].firstMatch); try await waitForVersion(2)
        check(try await download().contains("mixed-port: 9001"))
        let history = app.buttons["templates.history.toggle"].firstMatch
        XCTAssertTrue(history.waitForExistence(timeout: 15)); XCTAssertTrue(history.label.contains("2"), "Both immutable versions must be loaded: \(history.label)")
        try click(history)
        try click(app.buttons["templates.rollback.1"].firstMatch)
        try click(app.sheets.buttons["回滚并发布"].firstMatch); try await waitForVersion(3)
        check(try await download().contains("mixed-port: 0"), "Rollback must publish history as a new version on the same URL")

        try menu(app, "导入配置或模板…")
        let imported = "mixed-port: 8200\nproxies:\n  - {name: legacy, type: ss, server: example.invalid, password: fixture}\nproxy-groups:\n  - {name: Select, type: select, proxies: [legacy]}\nrules:\n  - MATCH,Select\nfuture-field: {values: [false, null, 0]}\n"
        try replace(app.textViews["templates.import.source"].firstMatch, imported)
        try click(app.buttons["templates.import.analyze"].firstMatch)
        let contains = app.textFields["名称包含"].firstMatch
        XCTAssertTrue(contains.waitForExistence(timeout: 10)); try replace(contains, "edge")
        attach(app, "Import analysis and editable reference mapping")
        try click(app.buttons["确认映射并写入本地草稿"].firstMatch)
        try click(app.buttons["templates.save"].firstMatch)
        try await waitForCommonPort(8200)
        let draft = try await api("/api/v1/subscription-templates/\(templateID)/draft")
        let document = try XCTUnwrap(draft["document"] as? [String:Any])
        XCTAssertEqual((document["injections"] as? [[String:Any]])?.first?["contains"] as? String, "edge")
        XCTAssertNotNil((document["common"] as? [String:Any])?["future-field"])
        check(try await download().contains("mixed-port: 0"))
        let package = try await api("/api/v1/subscription-templates/\(templateID)/export")
        XCTAssertEqual(package["package_version"] as? Int, 1)
        XCTAssertTrue(NSDictionary(dictionary: package["document"] as? [String:Any] ?? [:]).isEqual(to: document))
    }
    @MainActor private func click(_ element: XCUIElement) throws {
        XCTAssertTrue(element.waitForExistence(timeout: 15), "Missing control: \(element)")
        XCTAssertTrue(element.isEnabled); element.click()
    }
    @MainActor private func replace(_ element: XCUIElement, _ text: String) throws {
        XCTAssertTrue(element.waitForExistence(timeout: 15))
        let pasteboard = NSPasteboard.general
        let previous = (pasteboard.pasteboardItems ?? []).map { item in item.types.compactMap { type in item.data(forType: type).map { (type, $0) } } }
        var insertedChangeCount: Int?
        defer {
            if insertedChangeCount == pasteboard.changeCount {
            pasteboard.clearContents()
            let items = previous.map { values in let item = NSPasteboardItem(); for (type, data) in values { item.setData(data, forType: type) }; return item }
            if !items.isEmpty { pasteboard.writeObjects(items) }
            }
        }
        pasteboard.clearContents(); pasteboard.setString(text, forType: .string); insertedChangeCount = pasteboard.changeCount
        element.click(); element.typeKey("a", modifierFlags: .command); element.typeKey("v", modifierFlags: .command)
        var values = [text]
        if let number = Int(text) { let formatter = NumberFormatter(); formatter.numberStyle = .decimal; formatter.locale = Locale(identifier: "zh_CN"); if let formatted = formatter.string(from: NSNumber(value: number)) { values.append(formatted) } }
        let pasted = expectation(for: NSPredicate(format: "value IN %@", values), evaluatedWith: element)
        wait(for: [pasted], timeout: 5)
    }
    @MainActor private func choose(_ element: XCUIElement, _ title: String, _ app: XCUIApplication) throws { try click(element); try click(app.menuItems[title].firstMatch) }
    @MainActor private func menu(_ app: XCUIApplication, _ title: String) throws { try click(app.menuButtons["ellipsis.circle"].firstMatch); try click(app.menuItems[title].firstMatch) }
    @MainActor private func attach(_ app: XCUIApplication, _ name: String) { let attachment = XCTAttachment(screenshot: app.windows.firstMatch.screenshot()); attachment.name = name; attachment.lifetime = .keepAlways; add(attachment) }
    @MainActor private func check(_ value: Bool, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) { XCTAssertTrue(value, message, file: file, line: line) }
    @MainActor private func api(_ path: String) async throws -> [String:Any] {
        var request = URLRequest(url: URL(string: base + path)!); request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization"); request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        let (data, response) = try await URLSession.shared.data(for: request); XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String:Any])
    }
    @MainActor private func download() async throws -> String { let (data, response) = try await URLSession.shared.data(from: URL(string: base + "/sub/\(subscriptionToken)?template=\(templateID)&format=mihomo&platform=windows")!); XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200); return String(decoding: data, as: UTF8.self) }
    @MainActor private func eventually(_ test: @MainActor () async throws -> Bool) async throws -> Bool { let deadline = Date().addingTimeInterval(20); repeat { if try await test() { return true }; try await Task.sleep(for: .milliseconds(100)) } while Date() < deadline; return false }
    @MainActor private func waitForCommonPort(_ port: Int) async throws { check(try await eventually { let value = try await self.api("/api/v1/subscription-templates/\(self.templateID)/draft"); return ((value["document"] as? [String:Any])?["common"] as? [String:Any])?["mixed-port"] as? Int == port }) }
    @MainActor private func waitForVersion(_ version: Int) async throws { check(try await eventually { try await self.api("/api/v1/subscription-templates/\(self.templateID)")["published_version"] as? Int == version }) }
}
