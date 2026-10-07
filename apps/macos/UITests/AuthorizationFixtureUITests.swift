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
        app.buttons["authorizationGroups.editor.users"].firstMatch.click()
        let all = app.buttons["authorization.selectAllFiltered"].firstMatch
        XCTAssertTrue(all.waitForExistence(timeout: 5))
        all.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        let ordinary = app.descendants(matching: .any)["authorization.selection.node-1"].firstMatch
        XCTAssertTrue(ordinary.waitForExistence(timeout: 5))
        ordinary.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        // Cancelling the nested picker must discard its own selection edits.
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        app.descendants(matching: .any)["authorization.selection.node-2"].firstMatch.click()
        app.buttons["authorization.selection.cancel"].firstMatch.click()
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        let cancelledNode = app.descendants(matching: .any)["authorization.selection.node-2"].firstMatch
        XCTAssertTrue(cancelledNode.waitForExistence(timeout: 5))
        let cancelledValue = (cancelledNode.value as? String).flatMap(Int.init) ?? (cancelledNode.value as? NSNumber)?.intValue
        XCTAssertEqual(cancelledValue, 0)
        app.buttons["authorization.selection.done"].firstMatch.click()
        let preview = app.buttons["authorizationGroups.editor.preview"].firstMatch
        preview.click()
        let save = app.buttons["authorizationGroups.editor.save"].firstMatch
        XCTAssertTrue(save.waitForExistence(timeout: 5))
        let ready = NSPredicate(format: "enabled == true")
        expectation(for: ready, evaluatedWith: save)
        waitForExpectations(timeout: 5)
        let editor = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        editor.name = "Bulk authorization group editor and impact preview"
        editor.lifetime = .keepAlways
        add(editor)
        // Adding an mTLS node requires personal bindings and blocks commit.
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        let mtls = app.descendants(matching: .any)["authorization.selection.node-2"].firstMatch
        XCTAssertTrue(mtls.waitForExistence(timeout: 5))
        mtls.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        XCTAssertFalse(save.isEnabled)
        preview.click()
        XCTAssertTrue(app.descendants(matching: .any)["authorizationGroups.editor.mtlsBindings"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(save.isEnabled)
        let certificates = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        certificates.name = "Missing personal mTLS bindings block group commit"
        certificates.lifetime = .keepAlways
        add(certificates)
        app.buttons["authorizationGroups.editor.nodes"].firstMatch.click()
        app.descendants(matching: .any)["authorization.selection.node-2"].firstMatch.click()
        app.buttons["authorization.selection.done"].firstMatch.click()
        preview.click()
        expectation(for: ready, evaluatedWith: save)
        waitForExpectations(timeout: 5)
        save.click()
        XCTAssertTrue(app.descendants(matching: .any)["authorization.mutationReceipt"].firstMatch.waitForExistence(timeout: 10))
        let receipt = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        receipt.name = "Atomic group save result"
        receipt.lifetime = .keepAlways
        add(receipt)
        app.terminate()
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
        let changeCount = app.descendants(matching: .any)["authorization.members.changeCount"].firstMatch
        XCTAssertTrue(changeCount.waitForExistence(timeout: 5))
        XCTAssertTrue(displayedText(changeCount).contains("待移除 2 人"), displayedText(changeCount))
        let pending = app.descendants(matching: .any)["authorizationGroups.member.user-101"].firstMatch
        XCTAssertTrue(pending.waitForExistence(timeout: 5)); pending.click()
        app.buttons["authorization.members.undo"].firstMatch.click()
        XCTAssertTrue(displayedText(changeCount).contains("待移除 1 人"), displayedText(changeCount))
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
        XCTAssertTrue(displayedText(changeCount).contains("待添加 2 人"), displayedText(changeCount))
        app.buttons["authorizationGroups.editor.preview"].firstMatch.click()
        let summary = app.descendants(matching: .any)["authorizationGroups.editor.previewSummary"].firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 5))
        let save = app.buttons["authorizationGroups.editor.save"].firstMatch
        expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: save)
        waitForExpectations(timeout: 5)
        let preview = XCTAttachment(screenshot: app.sheets.firstMatch.screenshot())
        preview.name = "Large group membership delta and permission impact"
        preview.lifetime = .keepAlways; self.add(preview)
        app.buttons["取消"].firstMatch.click()
        app.buttons["authorizationGroups.addMembers"].firstMatch.click()
        XCTAssertTrue(changeCount.waitForExistence(timeout: 5))
        XCTAssertTrue(displayedText(changeCount).contains("待添加 0 人 · 待移除 0 人"), displayedText(changeCount))
        app.buttons["取消"].firstMatch.click()
    }


    @MainActor
    private func displayedText(_ element: XCUIElement) -> String {
        element.value as? String ?? element.label
    }

}
