import XCTest
import Foundation
import AppKit

private struct UITestCredentials: Decodable {
    let serviceAddress: String
    let adminToken: String
    let userName: String
    let nodeOneID: String
    let nodeOneName: String
    let nodeTwoID: String
    let nodeTwoName: String
    let privateKeyPEM: String
    let certificatePEM: String
}

private enum UITestConfiguration {
    static let credentialsPath = "__HYSTERIAX_UI_CREDENTIALS_PATH__"
    static let userName = "__HYSTERIAX_UI_TEST_USER_NAME__"
}

@MainActor
final class HysteriaXLiveUITests: XCTestCase {
    func testLiveManagementWorkflow() async throws {
        let credentialsData = try Data(contentsOf: URL(fileURLWithPath: UITestConfiguration.credentialsPath))
        let credentials = try JSONDecoder().decode(UITestCredentials.self, from: credentialsData)
        XCTAssertEqual(credentials.userName, UITestConfiguration.userName)
        print("SYSTEM_SETTINGS_BEFORE_LAUNCH=" + systemSettingsProcessIDs())
        print("FRONTMOST_BEFORE_LAUNCH=" + frontmostBundleIdentifier())

        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_UI_TEST_SERVICE_ADDRESS"] = credentials.serviceAddress
        app.launchEnvironment["HYSTERIAX_UI_TEST_ADMIN_TOKEN"] = credentials.adminToken
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        print("FRONTMOST_AFTER_LAUNCH=" + frontmostBundleIdentifier())

        let settingsButton = app.buttons["gearshape"].firstMatch
        XCTAssertTrue(settingsButton.waitForExistence(timeout: 20))
        settingsButton.click()
        print("FRONTMOST_AFTER_HYSTERIAX_SETTINGS=" + frontmostBundleIdentifier())
        print("SYSTEM_SETTINGS_AFTER_HYSTERIAX_SETTINGS=" + systemSettingsProcessIDs())
        XCTAssertTrue(app.descendants(matching: .any)["settings.keychainMode"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["settings.keychainMode"].waitForExistence(timeout: 10))

        let serviceAddress = app.textFields["settings.serviceAddress"].firstMatch
        XCTAssertTrue(serviceAddress.waitForExistence(timeout: 15))
        XCTAssertEqual(serviceAddress.value as? String, credentials.serviceAddress)

        let adminToken = app.secureTextFields["settings.adminToken"].firstMatch
        XCTAssertTrue(adminToken.waitForExistence(timeout: 10))
        print("SYSTEM_SETTINGS_BEFORE_VERIFY=" + systemSettingsProcessIDs())
        button(app, "验证并保存").click()
        print("FRONTMOST_IMMEDIATELY_AFTER_VERIFY=" + frontmostBundleIdentifier())

        XCTAssertTrue(
            app.descendants(matching: .any)["settings.connectionState"].waitForExistence(timeout: 60),
            "The Settings view should expose its connection state."
        )
        try await Task.sleep(for: .seconds(2))
        print("SYSTEM_SETTINGS_AFTER_VERIFY=" + systemSettingsProcessIDs())
        print("FRONTMOST_AFTER_VERIFY=" + frontmostBundleIdentifier())
        XCTAssertTrue(button(app, "在凭据中心管理 Token、证书和私钥").waitForExistence(timeout: 10))

        openSection(app, "节点")
        print("FRONTMOST_AFTER_NODES=" + frontmostBundleIdentifier())
        XCTAssertTrue(identified(app, "nodes.row.\(credentials.nodeOneID)").waitForExistence(timeout: 20))
        XCTAssertTrue(identified(app, "nodes.row.\(credentials.nodeTwoID)").waitForExistence(timeout: 20))
        button(app, "添加节点").click()
        for identifier in [
            "node.create.name",
            "node.create.sshHost",
            "node.create.sshPort",
            "node.create.sshUsername",
            "node.create.publicHost",
            "node.create.publicPort",
        ] {
            XCTAssertTrue(
                identified(app, identifier).waitForExistence(timeout: 10),
                "The add-node form should expose its field: \(identifier)"
            )
        }
        XCTAssertTrue(identified(app, "credential.picker").waitForExistence(timeout: 10))
        button(app, "取消").click()
        XCTAssertFalse(identified(app, "node.create.name").waitForExistence(timeout: 2))
        identified(app, "nodes.row.\(credentials.nodeOneID)").click()
        button(app, "代理配置").click()
        let tlsMode = app.popUpButtons["proxy.tls.mode"].firstMatch
        XCTAssertTrue(tlsMode.waitForExistence(timeout: 20))
        for _ in 0..<8 where !tlsMode.isHittable {
            app.scrollViews.containing(.popUpButton, identifier: "proxy.tls.mode").firstMatch.scroll(byDeltaX: 0, deltaY: -250)
        }
        tlsMode.click()
        app.menuItems["凭据中心证书"].firstMatch.click()
        for identifier in ["proxy.credential.identity", "proxy.credential.clientCA", "proxy.credential.ech"] {
            XCTAssertTrue(app.popUpButtons[identifier].firstMatch.waitForExistence(timeout: 10))
        }
        for target in ["identity", "clientCA", "ech"] {
            XCTAssertTrue(app.buttons["proxy.credential.create.\(target)"].firstMatch.exists)
        }
        let createCA = app.buttons["proxy.credential.create.identity"].firstMatch
        for _ in 0..<8 where !createCA.isHittable {
            app.scrollViews.containing(.popUpButton, identifier: "proxy.credential.identity").firstMatch.scroll(byDeltaX: 0, deltaY: -250)
        }
        createCA.click()
        let inlineName = "HX UI Credential Inline " + credentials.userName
        let inlineNameField = app.textFields["credential.editor.name"].firstMatch
        XCTAssertTrue(inlineNameField.waitForExistence(timeout: 10))
        inlineNameField.click(); inlineNameField.typeText(inlineName)
        let inlineContent = app.textViews["credential.editor.content.certificate"].firstMatch
        XCTAssertTrue(inlineContent.waitForExistence(timeout: 10))
        inlineContent.click(); inlineContent.typeText("-----BEGIN CERTIFICATE-----\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(String(credentials.certificatePEM.dropFirst("-----BEGIN CERTIFICATE-----\n".count)), forType: .string)
        app.typeKey("v", modifierFlags: .command)
        XCTAssertEqual(inlineContent.value as? String, credentials.certificatePEM, "PEM punctuation and newlines must remain unchanged.")
        let inlineKey = app.textViews["credential.editor.content.private_key"].firstMatch
        inlineKey.click()
        NSPasteboard.general.clearContents(); NSPasteboard.general.setString(credentials.privateKeyPEM, forType: .string)
        app.typeKey("v", modifierFlags: .command)
        XCTAssertEqual(inlineKey.value as? String, credentials.privateKeyPEM)
        identified(app, "credential.editor.save").click()
        XCTAssertTrue(identified(app, "credential.editor.save").waitForNonExistence(timeout: 30))
        XCTAssertTrue(String(describing: app.popUpButtons["proxy.credential.identity"].firstMatch.value).contains(inlineName), "Inline creation should select the new credential without leaving proxy configuration.")
        XCTAssertFalse(app.popUpButtons["proxy.credential.certificate"].exists)
        XCTAssertFalse(app.popUpButtons["proxy.credential.privateKey"].exists)
        XCTAssertFalse(app.textFields["证书路径或 resource:// 引用"].exists)
        XCTAssertFalse(app.textFields["私钥路径或 resource:// 引用"].exists)
        let resourceKind = app.popUpButtons["proxy.resource.kind"].firstMatch
        XCTAssertTrue(resourceKind.waitForExistence(timeout: 10))
        for _ in 0..<8 where !resourceKind.isHittable {
            app.scrollViews.containing(.popUpButton, identifier: "proxy.resource.kind").firstMatch.scroll(byDeltaX: 0, deltaY: -250)
        }
        resourceKind.click()
        for title in ["ACL 规则", "GeoIP 数据", "GeoSite 数据"] { XCTAssertTrue(app.menuItems[title].firstMatch.exists) }
        for title in ["证书", "私钥", "ECH 密钥"] { XCTAssertFalse(app.menuItems[title].firstMatch.exists) }
        app.menuItems["ACL 规则"].firstMatch.click()
        let proxyScreenshot = XCTAttachment(screenshot: app.screenshot())
        proxyScreenshot.name = "Proxy configuration credential selectors and ordinary resources"
        proxyScreenshot.lifetime = .keepAlways
        add(proxyScreenshot)
        button(app, "取消").click()
        XCTAssertTrue(tlsMode.waitForNonExistence(timeout: 10))
        openSection(app, "凭据")
        XCTAssertTrue(identified(app, "credentials.table").waitForExistence(timeout: 15))
        XCTAssertTrue(identified(app, "sidebar.jobs").isHittable, "Credential page must keep the outer sidebar visible and clickable.")
        let credentialPage = XCTAttachment(screenshot: app.screenshot())
        credentialPage.name = "Credential page before editing"
        credentialPage.lifetime = .keepAlways
        add(credentialPage)
        button(app, "创建凭据").click()
        XCTAssertTrue(identified(app, "credential.editor.save").waitForExistence(timeout: 10))
        button(app, "取消").click()
        let credentialName = "HX UI Credential " + credentials.userName
        button(app, "创建凭据").click()
        let credentialNameField = app.textFields["credential.editor.name"].firstMatch
        XCTAssertTrue(credentialNameField.waitForExistence(timeout: 10))
        credentialNameField.click(); credentialNameField.typeText(credentialName)
        let kindPicker = app.popUpButtons["credential.editor.kind"].firstMatch
        kindPicker.click(); app.menuItems["API Token"].firstMatch.click()
        let secretField = app.secureTextFields["credential.editor.token"].firstMatch
        XCTAssertTrue(secretField.waitForExistence(timeout: 10))
        secretField.click(); secretField.typeText("ui-fixture-initial-value")
        identified(app, "credential.editor.save").click()
        XCTAssertTrue(identified(app, "credential.editor.save").waitForNonExistence(timeout: 30), "Credential editor should close after its write and refresh complete.")
        let credentialRow = app.staticTexts[credentialName].firstMatch
        XCTAssertTrue(credentialRow.waitForExistence(timeout: 20))
        credentialRow.click()
        XCTAssertTrue(button(app, "发布新版本").waitForExistence(timeout: 15))
        XCTAssertFalse(app.secureTextFields["credential.editor.token"].exists)
        button(app, "发布新版本").click()
        let replacementSecret = app.secureTextFields["credential.editor.token"].firstMatch
        XCTAssertTrue(replacementSecret.waitForExistence(timeout: 10))
        replacementSecret.click(); replacementSecret.typeText("ui-fixture-replacement-value")
        identified(app, "credential.editor.save").click()
        XCTAssertTrue(identified(app, "credential.editor.save").waitForNonExistence(timeout: 30), "Credential editor should close after its write and refresh complete.")
        try await Task.sleep(for: .seconds(2))
        let (credentialStatus, credentialData) = try await request(base: credentials.serviceAddress, token: credentials.adminToken, path: "/api/v1/credentials")
        XCTAssertEqual(credentialStatus, 200)
        let credentialList = try XCTUnwrap(JSONSerialization.jsonObject(with: credentialData) as? [[String: Any]])
        let createdCredential = try XCTUnwrap(credentialList.first { $0["name"] as? String == credentialName })
        XCTAssertEqual(createdCredential["latest_version"] as? Int, 2)
        let responseText = String(decoding: credentialData, as: UTF8.self)
        XCTAssertFalse(responseText.contains("ui-fixture-initial-value"))
        XCTAssertFalse(responseText.contains("ui-fixture-replacement-value"))
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Credential center"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        openSection(app, "任务")
        print("FRONTMOST_AFTER_JOBS=" + frontmostBundleIdentifier())
        XCTAssertTrue(identified(app, "jobs.title").waitForExistence(timeout: 10))
        openSection(app, "审计")
        print("FRONTMOST_AFTER_AUDIT=" + frontmostBundleIdentifier())
        XCTAssertTrue(identified(app, "audit.title").waitForExistence(timeout: 10))

        openSection(app, "用户")
        print("FRONTMOST_AFTER_USERS=" + frontmostBundleIdentifier())
        button(app, "添加用户").click()
        let userNameField = app.textFields["user.create.name"].firstMatch
        XCTAssertTrue(userNameField.waitForExistence(timeout: 10))
        userNameField.click()
        userNameField.typeText(UITestConfiguration.userName)
        button(app, "创建用户").click()
        XCTAssertTrue(userNameField.waitForNonExistence(timeout: 30), "User form should close after creation and refresh complete.")
        let userID = try await userID(named: UITestConfiguration.userName, credentials: credentials)
        let userRow = identified(app, "users.row.\(userID)")
        XCTAssertTrue(userRow.waitForExistence(timeout: 15))
        userRow.click()

        assignUser(app, nodeID: credentials.nodeOneID, nodeName: credentials.nodeOneName)
        acknowledgeNotice(app)
        assignUser(app, nodeID: credentials.nodeTwoID, nodeName: credentials.nodeTwoName)
        acknowledgeNotice(app)
        XCTAssertTrue(identified(app, "user.selected.summary").waitForExistence(timeout: 15))

        chooseMenuAction(app, menu: "订阅", action: "生成/轮换订阅地址")
        acknowledgeNotice(app)
        let subscriptionToken = try await activeSubscriptionToken(userID, credentials: credentials)
        let subscriptionBeforeRotation = try await subscriptionYAML(subscriptionToken, credentials: credentials)
        XCTAssertEqual(subscriptionBeforeRotation.components(separatedBy: "type: hysteria2").count - 1, 2)

        chooseMenuAction(app, menu: "用户", action: "轮换连接密码")
        acknowledgeNotice(app)
        let subscriptionAfterRotation = try await subscriptionYAML(subscriptionToken, credentials: credentials)
        XCTAssertNotEqual(subscriptionAfterRotation, subscriptionBeforeRotation)

        chooseMenuAction(app, menu: "用户", action: "停用")
        acknowledgeNotice(app)
        let disabledUser = try await userSummary(userID, credentials: credentials)
        XCTAssertEqual(disabledUser["enabled"] as? Bool, false)
        let (disabledSubscriptionStatus, _) = try await request(
            base: credentials.serviceAddress,
            path: "/sub/\(subscriptionToken)/clash.yaml"
        )
        XCTAssertEqual(disabledSubscriptionStatus, 403)

        chooseMenuAction(app, menu: "用户", action: "删除用户…")
        let deleteButtons = app.buttons.matching(NSPredicate(format: "label == %@", "删除用户"))
            .allElementsBoundByIndex
        let deleteButton = deleteButtons.last(where: { $0.isHittable })
        XCTAssertNotNil(deleteButton, "The destructive confirmation button should be hittable.")
        deleteButton?.click()
        let deletedUser = identified(app, "users.row.\(userID)")
        let deletionDeadline = Date().addingTimeInterval(30)
        while deletedUser.exists && Date() < deletionDeadline {
            try? await Task.sleep(for: .milliseconds(250))
        }
        XCTAssertFalse(deletedUser.exists, "Deleting the temporary user should remove it from the list.")
        let (deletedUserStatus, _) = try await request(
            base: credentials.serviceAddress,
            token: credentials.adminToken,
            path: "/api/v1/users/\(userID)"
        )
        XCTAssertEqual(deletedUserStatus, 404)

        print("LIVE_UI_WORKFLOW=passed")
        app.terminate()
        print("FRONTMOST_AFTER_TERMINATE=" + frontmostBundleIdentifier())
    }

    private func systemSettingsProcessIDs() -> String {
        NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.systempreferences")
            .map { String($0.processIdentifier) }
            .joined(separator: ",")
    }

    private func frontmostBundleIdentifier() -> String {
        NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? "none"
    }

    private func openSection(_ app: XCUIApplication, _ title: String) {
        let sectionID: String
        switch title {
        case "概览": sectionID = "overview"
        case "节点": sectionID = "nodes"
        case "用户": sectionID = "users"
        case "凭据": sectionID = "credentials"
        case "任务": sectionID = "jobs"
        case "审计": sectionID = "audit"
        default: XCTFail("Unknown sidebar section: \(title)"); return
        }
        let section = app.descendants(matching: .any)["sidebar.\(sectionID)"]
        XCTAssertTrue(section.waitForExistence(timeout: 15), "Sidebar section should exist: \(title)")
        section.click()
    }

    private func assignUser(_ app: XCUIApplication, nodeID: String, nodeName: String) {
        let assignmentButton = identified(app, "users.assignNodeMenu")
        XCTAssertTrue(assignmentButton.waitForExistence(timeout: 15))
        assignmentButton.click()
        let node = identified(app, "user.assignments.node.\(nodeID)")
        XCTAssertTrue(node.waitForExistence(timeout: 15), "The node assignment checkbox should exist.")
        node.click()
        identified(app, "user.assignments.save").click()
        let complete = button(app, "完成")
        XCTAssertTrue(complete.waitForExistence(timeout: 30))
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: complete)
        XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 30), .completed)
        complete.click()
        XCTAssertTrue(node.waitForNonExistence(timeout: 15))
    }

    private func chooseMenuAction(_ app: XCUIApplication, menu: String, action: String) {
        let menuIdentifier = menu == "订阅" ? "users.subscriptionMenu" : "users.actionsMenu"
        let actionIdentifiers = [
            "生成/轮换订阅地址": "users.subscription.rotate",
            "轮换连接密码": "users.actions.rotateCredentials",
            "停用": "users.actions.toggleEnabled",
            "删除用户…": "users.actions.delete",
        ]
        guard let actionIdentifier = actionIdentifiers[action] else {
            XCTFail("Unknown menu action: \(action)")
            return
        }
        let menuButton = identified(app, menuIdentifier)
        XCTAssertTrue(menuButton.waitForExistence(timeout: 15))
        menuButton.click()
        let menuItem = identified(app, actionIdentifier)
        XCTAssertTrue(menuItem.waitForExistence(timeout: 10), "Menu action should exist: \(action)")
        menuItem.click()
    }

    private func acknowledgeNotice(_ app: XCUIApplication) {
        let ok = button(app, "好")
        if ok.waitForExistence(timeout: 10) {
            ok.click()
        }
    }

    private func button(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label == %@", title)).firstMatch
    }

    private func staticText(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        app.staticTexts.matching(NSPredicate(format: "label == %@", title)).firstMatch
    }

    private func identified(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    private func userID(named name: String, credentials: UITestCredentials) async throws -> String {
        let (status, data) = try await request(
            base: credentials.serviceAddress,
            token: credentials.adminToken,
            path: "/api/v1/users"
        )
        XCTAssertEqual(status, 200)
        let json = try JSONSerialization.jsonObject(with: data)
        let users = (json as? [[String: Any]]) ?? ((json as? [String: Any])?["users"] as? [[String: Any]]) ?? []
        guard let user = users.first(where: { $0["name"] as? String == name }),
              let id = user["id"] as? String else {
            throw NSError(domain: "HysteriaXLiveUITests", code: 1, userInfo: [NSLocalizedDescriptionKey: "Created test user was not returned by the API."])
        }
        return id
    }

    private func userSummary(_ id: String, credentials: UITestCredentials) async throws -> [String: Any] {
        let (status, data) = try await request(
            base: credentials.serviceAddress,
            token: credentials.adminToken,
            path: "/api/v1/users/\(id)"
        )
        XCTAssertEqual(status, 200)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private func activeSubscriptionToken(_ id: String, credentials: UITestCredentials) async throws -> String {
        let (status, data) = try await request(
            base: credentials.serviceAddress,
            token: credentials.adminToken,
            path: "/api/v1/users/\(id)/subscription"
        )
        XCTAssertEqual(status, 200)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let active = try XCTUnwrap(json["active"] as? [String: Any])
        return try XCTUnwrap(active["token"] as? String)
    }

    private func subscriptionYAML(_ token: String, credentials: UITestCredentials) async throws -> String {
        let (status, data) = try await request(
            base: credentials.serviceAddress,
            path: "/sub/\(token)/clash.yaml"
        )
        XCTAssertEqual(status, 200)
        return try XCTUnwrap(String(data: data, encoding: .utf8))
    }

    private func request(
        base: String,
        token: String? = nil,
        path: String
    ) async throws -> (Int, Data) {
        var request = URLRequest(url: URL(string: base + path)!)
        if let token {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        return (try XCTUnwrap((response as? HTTPURLResponse)?.statusCode), data)
    }
}
