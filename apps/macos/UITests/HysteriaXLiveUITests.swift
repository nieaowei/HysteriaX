import XCTest
import Foundation

private struct UITestCredentials: Decodable {
    let serviceAddress: String
    let adminToken: String
    let userName: String
    let nodeOneID: String
    let nodeOneName: String
    let nodeTwoID: String
    let nodeTwoName: String
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

        let app = XCUIApplication()
        app.launch()

        let settingsButton = button(app, "设置")
        XCTAssertTrue(settingsButton.waitForExistence(timeout: 20))
        settingsButton.click()
        XCTAssertTrue(app.descendants(matching: .any)["settings.keychainMode"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["settings.keychainMode"].waitForExistence(timeout: 10))

        let serviceAddress = app.textFields["settings.serviceAddress"].firstMatch
        XCTAssertTrue(serviceAddress.waitForExistence(timeout: 15))
        if (serviceAddress.value as? String) != credentials.serviceAddress {
            serviceAddress.click()
            serviceAddress.typeKey("a", modifierFlags: .command)
            serviceAddress.typeText(credentials.serviceAddress)
        }

        let adminToken = app.secureTextFields["settings.adminToken"].firstMatch
        XCTAssertTrue(adminToken.waitForExistence(timeout: 10))
        adminToken.click()
        adminToken.typeText(credentials.adminToken)
        button(app, "验证并保存").click()

        XCTAssertTrue(
            app.descendants(matching: .any)["settings.connectionState"].waitForExistence(timeout: 60),
            "The Settings view should expose its connection state."
        )
        XCTAssertTrue(button(app, "创建并切换").waitForExistence(timeout: 10), "The connected token-management controls should load.")

        openSection(app, "节点")
        XCTAssertTrue(identified(app, "nodes.row.\(credentials.nodeOneID)").waitForExistence(timeout: 20))
        XCTAssertTrue(identified(app, "nodes.row.\(credentials.nodeTwoID)").waitForExistence(timeout: 20))
        openSection(app, "任务")
        XCTAssertTrue(identified(app, "jobs.title").waitForExistence(timeout: 10))
        openSection(app, "审计")
        XCTAssertTrue(identified(app, "audit.title").waitForExistence(timeout: 10))

        openSection(app, "用户")
        button(app, "添加用户").click()
        let userNameField = app.textFields["user.create.name"].firstMatch
        XCTAssertTrue(userNameField.waitForExistence(timeout: 10))
        userNameField.click()
        userNameField.typeText(UITestConfiguration.userName)
        button(app, "创建用户").click()
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
    }

    private func openSection(_ app: XCUIApplication, _ title: String) {
        let sectionID: String
        switch title {
        case "概览": sectionID = "overview"
        case "节点": sectionID = "nodes"
        case "用户": sectionID = "users"
        case "任务": sectionID = "jobs"
        case "审计": sectionID = "audit"
        default: XCTFail("Unknown sidebar section: \(title)"); return
        }
        let section = app.descendants(matching: .any)["sidebar.\(sectionID)"]
        XCTAssertTrue(section.waitForExistence(timeout: 15), "Sidebar section should exist: \(title)")
        section.click()
    }

    private func assignUser(_ app: XCUIApplication, nodeID: String, nodeName: String) {
        let assignmentMenu = identified(app, "users.assignNodeMenu")
        XCTAssertTrue(assignmentMenu.waitForExistence(timeout: 15))
        assignmentMenu.click()
        let node = identified(app, "users.assignNode.\(nodeID)")
        XCTAssertTrue(node.waitForExistence(timeout: 10), "Node assignment menu item should exist.")
        node.click()

        XCTAssertTrue(identified(app, "user.assignment.summary").waitForExistence(timeout: 10))
        let submitButtons = app.buttons.matching(NSPredicate(format: "label == %@", "分配节点"))
            .allElementsBoundByIndex
        guard let submit = submitButtons.last(where: { $0.isHittable }) else {
            XCTFail("The assignment form's submit button should be hittable.")
            return
        }
        submit.click()
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
