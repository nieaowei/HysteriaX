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
            XCTAssertEqual(app.radioButtons.count, 2)
            app.radioButtons["授权组"].firstMatch.click()
            XCTAssertTrue(group.waitForExistence(timeout: 5))
            XCTAssertTrue(app.descendants(matching: .any)["authorizationGroups.detail"].firstMatch.waitForExistence(timeout: 5))
            XCTAssertEqual(app.radioButtons.count, 2)
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
        let editor = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
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
        let certificates = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
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
        let receipt = XCTAttachment(screenshot: app.windows.firstMatch.screenshot())
        receipt.name = "Atomic group save result"
        receipt.lifetime = .keepAlways
        add(receipt)
        app.terminate()
    }
}
