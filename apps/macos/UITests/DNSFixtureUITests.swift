import XCTest

final class DNSFixtureUITests: XCTestCase {
    @MainActor
    func testDNSPageAndRecordEditor() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_DNS_FIXTURE_DIRECTORY"] = "__DNS_FIXTURES__"
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.dns"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10))
        sidebar.click()
        let record = app.descendants(matching: .any)["dns.record.row.record-1"].firstMatch
        XCTAssertTrue(record.waitForExistence(timeout: 10))
        app.activate()
        record.click()
        XCTAssertTrue(app.staticTexts["DNS only"].firstMatch.waitForExistence(timeout: 5))
        let screenshot = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        screenshot.name = "DNS records and node binding"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.buttons.matching(NSPredicate(format: "label == %@", "编辑…")).firstMatch.click()
        let content = app.textFields["dns.record.content"].firstMatch
        XCTAssertTrue(content.waitForExistence(timeout: 5))
        XCTAssertEqual(content.value as? String, "8.8.8.8")
        let editor = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        editor.name = "DNS record editor"
        editor.lifetime = .keepAlways
        add(editor)
        app.buttons.matching(NSPredicate(format: "label == %@", "取消")).firstMatch.click()
        app.buttons.matching(NSPredicate(format: "label == %@", "连接与域名…")).firstMatch.click()
        XCTAssertTrue(app.staticTexts["DNS 连接与域名区域"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label == %@", "验证并读取域名")).firstMatch.exists)
        let connections = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        connections.name = "DNS connections and zone selection"
        connections.lifetime = .keepAlways
        add(connections)
        app.buttons.matching(NSPredicate(format: "label == %@", "完成")).firstMatch.click()
        app.descendants(matching: .any)["sidebar.nodes"].firstMatch.click()
        app.buttons.matching(NSPredicate(format: "label == %@", "添加节点")).firstMatch.click()
        XCTAssertTrue(app.descendants(matching: .any)["dns.allocation.mode"].firstMatch.waitForExistence(timeout: 5))
        let create = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        create.name = "Node creation DNS address modes"
        create.lifetime = .keepAlways
        add(create)
        let allocation = app.descendants(matching: .any)["dns.allocation.mode"].firstMatch
        allocation.click()
        app.menuItems["自动分配域名"].firstMatch.click()
        XCTAssertTrue(app.textFields["dns.allocation.ipv4"].firstMatch.waitForExistence(timeout: 5))
        let automatic = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        automatic.name = "Automatic allocation fields"
        automatic.lifetime = .keepAlways
        add(automatic)
        app.buttons.matching(NSPredicate(format: "label == %@", "取消")).firstMatch.click()
        app.descendants(matching: .any)["nodes.row.node-1"].firstMatch.click()
        app.descendants(matching: .any)["node.configure.node-1"].firstMatch.click()
        app.menuItems["代理配置"].firstMatch.click()
        let certificate = app.descendants(matching: .any)["proxy.tls.mode"].firstMatch
        XCTAssertTrue(certificate.waitForExistence(timeout: 10))
        for _ in 0..<5 where !certificate.isHittable {
            app.sheets.firstMatch.scrollViews.firstMatch.scroll(byDeltaX: 0, deltaY: -400)
        }
        certificate.click()
        app.menuItems["ACME 自动申请"].firstMatch.click()
        let useDomain = app.buttons.matching(NSPredicate(format: "label == %@", "使用已分配域名")).firstMatch
        XCTAssertTrue(useDomain.waitForExistence(timeout: 5))
        useDomain.click()
        XCTAssertTrue(app.textFields.matching(NSPredicate(format: "value == %@", "hk.example.test")).firstMatch.exists)
        let acme = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        acme.name = "Separate ACME configuration using allocated domain"
        acme.lifetime = .keepAlways
        add(acme)
        app.terminate()
    }
}
