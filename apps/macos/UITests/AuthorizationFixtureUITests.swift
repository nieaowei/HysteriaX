import XCTest
import AppKit

final class AuthorizationFixtureUITests: XCTestCase {
    @MainActor
    func testToolbarTabsSourcesAndBulkEditor() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.users"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10))
        sidebar.click()
        let tabs = app.descendants(matching: .any)["users.authorizationTabs"].firstMatch
        XCTAssertTrue(tabs.waitForExistence(timeout: 10))
        app.radioButtons["用户"].firstMatch.click()
        let user = app.descendants(matching: .any)["users.row.user-1"].firstMatch
        XCTAssertTrue(user.waitForExistence(timeout: 10))
        user.click()
        XCTAssertTrue(app.buttons["users.manageAuthorizationGroups"].firstMatch.waitForExistence(timeout: 5))
        let certificatesButton = app.buttons["users.managedCertificates"].firstMatch
        XCTAssertTrue(certificatesButton.waitForExistence(timeout: 5))
        certificatesButton.click()
        XCTAssertTrue(app.descendants(matching: .any)["users.managedIdentity.identity-1"].firstMatch.waitForExistence(timeout: 15))
        let managedSheet = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        managedSheet.name = "User managed certificates grouped by CA"
        managedSheet.lifetime = .keepAlways
        add(managedSheet)
        app.sheets.firstMatch.buttons["关闭"].firstMatch.click()
        let source = app.descendants(matching: .any)["users.assignment.sourceGroup.group-1"].firstMatch
        XCTAssertTrue(source.waitForExistence(timeout: 5))
        source.click()
        let group = app.descendants(matching: .any)["authorizationGroups.row.group-1"].firstMatch
        XCTAssertTrue(group.waitForExistence(timeout: 5))
        XCTAssertTrue(app.descendants(matching: .any)["authorizationGroups.detail"].firstMatch.waitForExistence(timeout: 5))
        for _ in 0..<3 {
            app.radioButtons["用户"].firstMatch.click()
            XCTAssertTrue(user.waitForExistence(timeout: 5))
            XCTAssertTrue(app.descendants(matching: .any)["users.detail"].firstMatch.waitForExistence(timeout: 5))
            XCTAssertTrue(tabs.exists)
            app.radioButtons["授权组"].firstMatch.click()
            XCTAssertTrue(group.waitForExistence(timeout: 5))
            XCTAssertTrue(app.descendants(matching: .any)["authorizationGroups.detail"].firstMatch.waitForExistence(timeout: 5))
            XCTAssertTrue(tabs.exists)
            XCTAssertLessThanOrEqual(app.buttons["authorization.members.nextPage"].firstMatch.frame.maxX, app.windows.firstMatch.frame.maxX)
        }
        let page = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        page.name = "Authorization groups toolbar tab and detail"
        page.lifetime = .keepAlways
        add(page)
        app.buttons["authorizationGroups.create"].firstMatch.click()
        let name = app.textFields["authorizationGroups.editor.name"].firstMatch
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        name.click()
        let previousClipboard = NSPasteboard.general.string(forType: .string)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString("UI test group", forType: .string)
        name.typeKey("v", modifierFlags: .command)
        NSPasteboard.general.clearContents()
        if let previousClipboard { NSPasteboard.general.setString(previousClipboard, forType: .string) }
        XCTAssertFalse(app.buttons["authorizationGroups.editor.users"].firstMatch.exists)
        XCTAssertFalse(app.buttons["authorizationGroups.editor.nodes"].firstMatch.exists)
        let save = app.buttons["authorizationGroups.editor.save"].firstMatch
        XCTAssertTrue(save.isEnabled)
        save.click()
        XCTAssertTrue(app.descendants(matching: .any)["authorization.mutationReceipt"].firstMatch.waitForExistence(timeout: 10))
        let receipt = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        receipt.name = "Atomic group save result"
        receipt.lifetime = .keepAlways
        add(receipt)
        app.terminate()
    }

    @MainActor
    func testSavedResultDoesNotWaitForSlowGlobalRefresh() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchEnvironment["HYSTERIAX_TEST_REFRESH_DELAY_SECONDS"] = "8"
        app.launchEnvironment["HYSTERIAX_TEST_SLOW_DIRECTORY"] = "1"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.users"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10)); sidebar.click()
        app.radioButtons["授权组"].firstMatch.click()
        let group = app.descendants(matching: .any)["authorizationGroups.row.group-1"].firstMatch
        XCTAssertTrue(group.waitForExistence(timeout: 10)); group.click()
        app.buttons["authorizationGroups.manageNodes"].firstMatch.click()
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        let node = app.descendants(matching: .any)["authorization.selection.node-1"].firstMatch
        XCTAssertTrue(node.waitForExistence(timeout: 5)); node.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        app.buttons["authorizationGroups.editor.save"].firstMatch.click()
        let summary = app.descendants(matching: .any)["authorizationGroups.editor.resultSummary"].firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 4), "Saved result must not wait for the eight-second monitoring read")
        XCTAssertTrue(app.buttons["完成"].firstMatch.isEnabled)
        let directoryStarted = NSPredicate { _, _ in FileManager.default.fileExists(atPath: "__AUTHORIZATION_FIXTURES__/directory-refresh-started") }
        expectation(for: directoryStarted, evaluatedWith: app)
        waitForExpectations(timeout: 15)
        XCTAssertTrue(app.buttons["完成"].firstMatch.isEnabled, "Directory reloading must not disable the saved result sheet")
    }

    @MainActor
    func testDeletionAndMembershipSaveDirectly() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.users"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10)); sidebar.click()
        app.radioButtons["用户"].firstMatch.click()
        let user = app.descendants(matching: .any)["users.row.user-1"].firstMatch
        XCTAssertTrue(user.waitForExistence(timeout: 10)); user.click()
        app.buttons["users.manageAuthorizationGroups"].firstMatch.click()
        app.buttons["users.groups.select"].firstMatch.click()
        let selected = app.descendants(matching: .any)["authorization.selection.group-1"].firstMatch
        XCTAssertTrue(selected.waitForExistence(timeout: 5)); selected.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        XCTAssertFalse(app.buttons["users.groups.preview"].firstMatch.exists)
        let save = app.buttons["users.groups.save"].firstMatch
        XCTAssertTrue(save.isEnabled); save.click()
        let membership = app.descendants(matching: .any)["users.groups.resultSummary"].firstMatch
        XCTAssertTrue(membership.waitForExistence(timeout: 10))
        XCTAssertTrue(displayedText(membership).contains("移除 1 条连接授权"), displayedText(membership))
        app.buttons["完成"].firstMatch.click()
        app.radioButtons["授权组"].firstMatch.click()
        let group = app.descendants(matching: .any)["authorizationGroups.row.group-1"].firstMatch
        XCTAssertTrue(group.waitForExistence(timeout: 10)); group.click()
        app.buttons["authorizationGroups.delete"].firstMatch.click()
        let result = app.descendants(matching: .any)["authorizationGroups.delete.resultSummary"].firstMatch
        XCTAssertTrue(result.waitForExistence(timeout: 10))
        XCTAssertTrue(displayedText(result).contains("授权组已删除"), displayedText(result))
    }

    @MainActor
    func testNodeChangesSaveDirectlyAndShowResult() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.users"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10)); sidebar.click()
        app.radioButtons["授权组"].firstMatch.click()
        let group = app.descendants(matching: .any)["authorizationGroups.row.group-1"].firstMatch
        XCTAssertTrue(group.waitForExistence(timeout: 10)); group.click()
        app.buttons["authorizationGroups.manageNodes"].firstMatch.click()
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        let node = app.descendants(matching: .any)["authorization.selection.node-1"].firstMatch
        XCTAssertTrue(node.waitForExistence(timeout: 5)); node.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        XCTAssertFalse(app.buttons["authorizationGroups.editor.preview"].firstMatch.exists)
        let save = app.buttons["authorizationGroups.editor.save"].firstMatch
        XCTAssertTrue(save.isEnabled); save.click()
        let summary = app.descendants(matching: .any)["authorizationGroups.editor.resultSummary"].firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 10))
        XCTAssertTrue(displayedText(summary).contains("授权组已更新"), displayedText(summary))
    }

    @MainActor
    func testLargeMembershipDraftAndSelection() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.users"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10)); sidebar.click()
        app.radioButtons["授权组"].firstMatch.click()
        let group = app.descendants(matching: .any)["authorizationGroups.row.group-1"].firstMatch
        XCTAssertTrue(group.waitForExistence(timeout: 10)); group.click()
        let member = app.descendants(matching: .any)["authorizationGroups.member.user-1"].firstMatch
        XCTAssertTrue(member.waitForExistence(timeout: 5))
        XCTAssertGreaterThan(member.frame.minX, group.frame.maxX)
        let membersTable = app.descendants(matching: .any)["authorization.members.table"].firstMatch
        XCTAssertGreaterThan(membersTable.frame.height, 200)
        let detail = app.descendants(matching: .any)["authorizationGroups.detail"].firstMatch
        XCTAssertEqual(detail.frame.maxX, app.windows.firstMatch.frame.maxX, accuracy: 2)
        XCTAssertEqual(membersTable.frame.maxX, detail.frame.maxX - 16, accuracy: 2)
        let horizontalScrollers = membersTable.descendants(matching: .scrollBar).allElementsBoundByIndex.filter {
            $0.frame.width > $0.frame.height && $0.isHittable
        }
        XCTAssertTrue(horizontalScrollers.isEmpty, "Long user IDs must fit without a horizontal scrollbar")
        let layout = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        layout.name = "Group list background and full-width member detail"
        layout.lifetime = .keepAlways; self.add(layout)
        XCTAssertLessThanOrEqual(membersTable.frame.maxX, app.windows.firstMatch.frame.maxX)
        XCTAssertLessThanOrEqual(app.buttons["authorization.members.nextPage"].firstMatch.frame.maxX, app.windows.firstMatch.frame.maxX)
        member.click()
        let next = app.buttons["authorization.members.nextPage"].firstMatch
        XCTAssertTrue(next.isEnabled); next.click()
        let pageMember = app.descendants(matching: .any)["authorizationGroups.member.user-101"].firstMatch
        XCTAssertTrue(pageMember.waitForExistence(timeout: 5))
        pageMember.click()
        let count = app.descendants(matching: .any)["authorization.members.selectionCount"].firstMatch
        XCTAssertTrue(displayedText(count).contains("已选 2 人"), displayedText(count))
        app.buttons["authorizationGroups.removeMembers"].firstMatch.click()
        let pending = app.descendants(matching: .any)["authorizationGroups.member.user-101"].firstMatch
        XCTAssertTrue(pending.waitForExistence(timeout: 5)); pending.click()
        app.buttons["authorization.members.undo"].firstMatch.click()
        app.radioButtons["添加成员"].firstMatch.click()
        let search = app.textFields["authorization.members.search"].firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 5)); search.click(); search.typeText("user-202")
        let candidate = app.descendants(matching: .any)["authorizationGroups.member.user-202"].firstMatch
        XCTAssertTrue(candidate.waitForExistence(timeout: 5)); candidate.click()
        search.click(); search.typeKey("a", modifierFlags: .command); search.typeText("user-203")
        let second = app.descendants(matching: .any)["authorizationGroups.member.user-203"].firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 5)); second.click()
        let add = app.buttons["authorization.members.add"].firstMatch
        XCTAssertTrue(add.label.contains("2 人"), add.label); add.click()
        XCTAssertFalse(app.buttons["authorizationGroups.editor.preview"].firstMatch.exists)
        let save = app.buttons["authorizationGroups.editor.save"].firstMatch
        XCTAssertTrue(save.isEnabled)
        save.click()
        let summary = app.descendants(matching: .any)["authorizationGroups.editor.resultSummary"].firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 10))
        XCTAssertTrue(displayedText(summary).contains("新增 2 条连接授权 · 移除 1 条连接授权"), displayedText(summary))
        XCTAssertTrue(app.descendants(matching: .any)["authorization.mutationReceipt"].firstMatch.exists)
        app.buttons["完成"].firstMatch.click()

    }


    @MainActor
    func testCAIdentityCreationPolicyAndPublication() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching: .any)["sidebar.credentials"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout: 10)); sidebar.click()
        let create = app.buttons["ca.create"].firstMatch
        XCTAssertFalse(app.staticTexts["Internal probe"].firstMatch.exists)
        let filters = app.descendants(matching:.any)["credentials.filters"].firstMatch
        XCTAssertTrue(filters.waitForExistence(timeout:5)); filters.click()
        app.menuItems["显示内部探测凭据"].firstMatch.click()
        XCTAssertTrue(app.staticTexts["Internal probe"].firstMatch.waitForExistence(timeout:5))
        XCTAssertTrue(create.waitForExistence(timeout: 10)); create.click()
        let name = app.textFields["ca.editor.name"].firstMatch
        XCTAssertTrue(name.waitForExistence(timeout: 5)); name.click(); name.typeText("New CA")
        let generate = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        generate.name = "Generated CA identity and client validity policy"; generate.lifetime = .keepAlways; add(generate)
        let save = app.buttons["ca.editor.save"].firstMatch
        XCTAssertTrue(save.isEnabled); save.click()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: name)
        waitForExpectations(timeout: 10)
        let row = app.staticTexts["Shared CA"].firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        let policy = app.buttons["客户端证书策略"].firstMatch
        XCTAssertTrue(policy.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["当前发布 CA 到期"].firstMatch.exists)
        XCTAssertTrue(app.staticTexts["准备中版本"].firstMatch.exists)
        policy.click()
        let validity = app.textFields["ca.editor.validity"].firstMatch
        XCTAssertTrue(validity.waitForExistence(timeout: 5))
        let policyShot = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        policyShot.name = "Independent CA policy editor"; policyShot.lifetime = .keepAlways; add(policyShot)
        app.buttons["ca.editor.save"].firstMatch.click()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: validity)
        waitForExpectations(timeout: 10)
        let publish = app.buttons["发布新版本"].firstMatch
        XCTAssertTrue(publish.waitForExistence(timeout: 5)); publish.click()
        XCTAssertTrue(app.staticTexts["发布 CA 新版本"].firstMatch.waitForExistence(timeout: 5))
        let publishShot = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        publishShot.name = "Staged CA publication"; publishShot.lifetime = .keepAlways; add(publishShot)
        app.buttons["取消"].firstMatch.click()
    }

    @MainActor
    func testNodeManagedConfigurationAndRecoveryStatus() throws {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching:.any)["sidebar.nodes"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout:10)); sidebar.click()
        let node = app.staticTexts["香港"].firstMatch
        XCTAssertTrue(node.waitForExistence(timeout:10)); node.click()
        let state = app.staticTexts.matching(NSPredicate(format:"value CONTAINS %@ OR label CONTAINS %@", "Remote activation outcome is unconfirmed", "Remote activation outcome is unconfirmed")).firstMatch
        XCTAssertTrue(state.waitForExistence(timeout:10))
        XCTAssertTrue(app.buttons["恢复同步"].firstMatch.isEnabled)
        app.descendants(matching:.any)["更多"].firstMatch.click()
        XCTAssertFalse(app.menuItems["回滚"].firstMatch.isEnabled)
        app.typeKey(.escape, modifierFlags:[])
        let menu = app.descendants(matching:.any)["node.configure.node-1"].firstMatch
        XCTAssertTrue(menu.waitForExistence(timeout:5)); menu.click()
        app.menuItems["代理配置"].firstMatch.click()
        let mode = app.sheets.firstMatch.radioButtons["关闭"].firstMatch
        XCTAssertTrue(mode.waitForExistence(timeout:10))
        for _ in 0..<5 where !mode.isHittable { app.sheets.firstMatch.scrollViews.firstMatch.scroll(byDeltaX:0,deltaY:-300) }
        XCTAssertTrue(mode.isHittable)
        let yaml = app.sheets.firstMatch.staticTexts["node.config.yamlPreview"]
        XCTAssertTrue(yaml.waitForExistence(timeout:5))
        XCTAssertFalse(displayedText(yaml).contains("mtls:"))
        XCTAssertFalse(displayedText(yaml).contains("ca_credential_id"))
        XCTAssertTrue(displayedText(yaml).contains("clientCA:"))
        let screenshot = XCTAttachment(screenshot:app.sheets.firstMatch.screenshot())
        screenshot.name="Managed CA selection and recovery status"; screenshot.lifetime = .keepAlways; add(screenshot)
        mode.click()
        expectation(for:NSPredicate { _,_ in String(describing:mode.value ?? "") == "1" },evaluatedWith:mode)
        waitForExpectations(timeout:5)
        expectation(for:NSPredicate { _,_ in
            !(yaml.value as? String ?? yaml.label).contains("clientCA:")
        },evaluatedWith:yaml)
        waitForExpectations(timeout:5)
        XCTAssertFalse(displayedText(yaml).contains("mtls:"))
        XCTAssertFalse(displayedText(yaml).contains("clientCA:"))
        app.buttons["保存并同步"].firstMatch.click()
        expectation(for:NSPredicate(format:"exists == false"),evaluatedWith:mode)
        waitForExpectations(timeout:10)
        let body = try JSONSerialization.jsonObject(with:Data(contentsOf:URL(fileURLWithPath:"__AUTHORIZATION_FIXTURES__/node-patch.json"))) as! [String:Any]
        let config = body["config"] as! [String:Any]
        XCTAssertEqual((config["mtls"] as! [String:Any])["mode"] as? String,"off")
        XCTAssertNil((config["tls"] as! [String:Any])["clientCA"])
    }

    @MainActor
    func testCreatedCAIsSelectedAcrossPages() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchEnvironment["HYSTERIAX_TEST_EXTRA_CA_COUNT"] = "55"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching:.any)["sidebar.credentials"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout:10)); sidebar.click()
        app.radioButtons["用户凭据"].firstMatch.click()
        app.buttons["ca.create"].firstMatch.click()
        let name = app.textFields["ca.editor.name"].firstMatch
        XCTAssertTrue(name.waitForExistence(timeout:5)); name.click(); name.typeText("New CA")
        app.buttons["ca.editor.save"].firstMatch.click()
        let policy = app.buttons["客户端证书策略"].firstMatch
        XCTAssertTrue(policy.waitForExistence(timeout:10))
        XCTAssertTrue(app.staticTexts["Shared CA"].firstMatch.exists)
    }

    @MainActor
    func testResetNodeRequiresExplicitPolicyBeforeDeployment() throws {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_AUTHORIZATION_FIXTURE_DIRECTORY"] = "__AUTHORIZATION_FIXTURES__"
        app.launchEnvironment["HYSTERIAX_TEST_NODE_MTLS_STATE"] = "needs_configuration"
        app.launchArguments += ["-appLanguage", "zh-Hans"]
        app.launch()
        addTeardownBlock { await MainActor.run { app.terminate() } }
        let sidebar = app.descendants(matching:.any)["sidebar.nodes"].firstMatch
        XCTAssertTrue(sidebar.waitForExistence(timeout:10)); sidebar.click()
        let node = app.staticTexts["香港"].firstMatch
        XCTAssertTrue(node.waitForExistence(timeout:10)); node.click()
        let sync = app.buttons["同步"].firstMatch
        XCTAssertTrue(sync.waitForExistence(timeout:10)); XCTAssertFalse(sync.isEnabled)
        XCTAssertTrue(app.buttons["SSH 测试"].firstMatch.isEnabled)
        app.descendants(matching:.any)["更多"].firstMatch.click()
        XCTAssertFalse(app.menuItems["部署"].firstMatch.isEnabled)
        XCTAssertFalse(app.menuItems["回滚"].firstMatch.isEnabled)
        app.typeKey(.escape, modifierFlags:[])
    }

    @MainActor
    private func displayedText(_ element: XCUIElement) -> String {
        element.value as? String ?? element.label
    }

}
