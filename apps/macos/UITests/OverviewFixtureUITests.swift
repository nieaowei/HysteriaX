import XCTest

@MainActor
final class OverviewFixtureUITests: XCTestCase {
    func testOverviewNavigationAndKeyboardDismissal() {
        let app = XCUIApplication()
        app.launchEnvironment["HYSTERIAX_OVERVIEW_FIXTURE_DIRECTORY"] = "__OVERVIEW_FIXTURES__"
        app.launch()
        let overview = app.descendants(matching: .any)["overview.page"].firstMatch
        XCTAssertTrue(overview.waitForExistence(timeout: 15))
        XCTAssertTrue(app.toolbars.descendants(matching: .any)["overview.tabPicker"].firstMatch.exists)
        XCTAssertFalse(app.descendants(matching: .any)["overview.tab.server"].exists)
        let serverMonitoring = app.descendants(matching: .any)["overview.serverMonitoring"].firstMatch
        let issues = app.descendants(matching: .any)["overview.issues"].firstMatch
        XCTAssertTrue(serverMonitoring.waitForExistence(timeout: 5))
        XCTAssertTrue(issues.exists)
        let summaryScroll = app.scrollViews["overview.page"].firstMatch
        XCTAssertTrue(summaryScroll.exists)
        let cpu = app.staticTexts["CPU"].firstMatch
        if !cpu.exists { summaryScroll.scroll(byDeltaX: 0, deltaY: -450) }
        XCTAssertTrue(cpu.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["内存"].firstMatch.exists)
        let disk = app.staticTexts["根目录磁盘"].firstMatch
        if !disk.exists { summaryScroll.scroll(byDeltaX: 0, deltaY: -250) }
        XCTAssertTrue(disk.waitForExistence(timeout: 5))
        summaryScroll.scroll(byDeltaX: 0, deltaY: 1000)
        if overview.frame.width >= 700 {
            XCTAssertGreaterThan(serverMonitoring.frame.minX, issues.frame.minX + 100)
            XCTAssertLessThanOrEqual(issues.frame.width, 300)
        }
        let summaryScreenshot = XCTAttachment(screenshot: app.screenshot())
        summaryScreenshot.name = "Summary with compact tasks and management service"
        summaryScreenshot.lifetime = .keepAlways
        add(summaryScreenshot)
        let failedJob = app.buttons["overview.job.job-1"].firstMatch
        XCTAssertTrue(failedJob.waitForExistence(timeout: 5))
        failedJob.click()
        let retryLink = app.buttons["jobs.retryLink.job-1"].firstMatch
        XCTAssertTrue(retryLink.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["jobs.retry.job-1"].exists)
        retryLink.click()
        XCTAssertTrue(app.staticTexts["此任务为关联重试，成功后会移除原失败提醒。"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["jobs.retry.job-2"].firstMatch.exists)
        app.descendants(matching: .any)["sidebar.overview"].firstMatch.click()
        let issue = app.buttons["overview.issue.node:node-1:proxy_probe"].firstMatch
        XCTAssertTrue(issue.waitForExistence(timeout: 10))
        issue.click()
        XCTAssertTrue(app.descendants(matching: .any)["nodes.row.node-1"].firstMatch.waitForExistence(timeout: 10))
        app.descendants(matching: .any)["sidebar.overview"].firstMatch.click()
        app.descendants(matching: .any)["overview.tab.traffic"].firstMatch.click()
        XCTAssertTrue(app.descendants(matching: .any)["overview.history.traffic"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.descendants(matching: .any)["overview.history.quality"].firstMatch.exists)
        let rangePicker = app.descendants(matching: .any)["overview.history.range"].firstMatch
        XCTAssertEqual(rangePicker.value as? String, "24 小时")
        let trafficScreenshot = XCTAttachment(screenshot: app.screenshot())
        trafficScreenshot.name = "Traffic 24-hour columns"
        trafficScreenshot.lifetime = .keepAlways
        add(trafficScreenshot)
        let thirtyDays = rangePicker.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "30 天")).firstMatch
        XCTAssertTrue(thirtyDays.exists)
        thirtyDays.click()
        XCTAssertEqual(rangePicker.value as? String, "30 天")
        let monthScreenshot = XCTAttachment(screenshot: app.screenshot())
        monthScreenshot.name = "Traffic 30-day columns"
        monthScreenshot.lifetime = .keepAlways
        add(monthScreenshot)
        let sevenDays = rangePicker.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "7 天")).firstMatch
        XCTAssertTrue(sevenDays.exists)
        sevenDays.click()
        let trafficScroll = app.scrollViews["overview.page"].firstMatch
        trafficScroll.scroll(byDeltaX: 0, deltaY: -300)
        let quotaBookmark = app.descendants(matching: .any)["overview.quotaRank"].firstMatch
        XCTAssertTrue(quotaBookmark.waitForExistence(timeout: 5))
        let bookmarkedY = quotaBookmark.frame.minY
        XCTAssertTrue(bookmarkedY.isFinite)
        app.descendants(matching: .any)["overview.tab.summary"].firstMatch.click()
        XCTAssertTrue(app.staticTexts["管理服务器"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.descendants(matching: .any)["overview.history.traffic"].firstMatch.exists)
        app.descendants(matching: .any)["overview.tab.traffic"].firstMatch.click()
        XCTAssertEqual(app.descendants(matching: .any)["overview.history.range"].firstMatch.value as? String, "7 天")
        XCTAssertTrue(quotaBookmark.waitForExistence(timeout: 5))
        XCTAssertEqual(quotaBookmark.frame.minY, bookmarkedY, accuracy: 8)
        XCTAssertFalse(app.descendants(matching: .any)["overview.serverMonitoring"].exists)
        app.descendants(matching: .any)["overview.tab.summary"].firstMatch.click()
        app.buttons["overview.quotaShortcut"].click()
        XCTAssertTrue(app.descendants(matching: .any)["overview.quotaRank"].firstMatch.waitForExistence(timeout: 5))
        app.descendants(matching: .any)["overview.tab.quality"].firstMatch.click()
        XCTAssertTrue(app.descendants(matching: .any)["overview.history.quality"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["流量来源"].exists)
        let monitor = app.buttons["overview.node.node-1"].firstMatch
        XCTAssertTrue(monitor.waitForExistence(timeout: 10))
        monitor.click()
        XCTAssertTrue(app.staticTexts["公网入口与转发：成功"].firstMatch.waitForExistence(timeout: 10))
        app.typeKey(.escape, modifierFlags: [])
        XCTAssertFalse(app.buttons["管理此节点"].firstMatch.exists)
        app.buttons["overview.notifications"].click()
        XCTAssertTrue(app.staticTexts["监控事项 1"].firstMatch.waitForExistence(timeout: 5))
        app.typeKey(.escape, modifierFlags: [])
        XCTAssertFalse(app.staticTexts["监控事项 1"].firstMatch.exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Overview fixture layout"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }
}
