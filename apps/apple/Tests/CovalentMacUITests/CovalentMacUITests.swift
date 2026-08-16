import XCTest

@MainActor
final class CovalentMacUITests: XCTestCase {
    func testTierOneNavigationAndPrimaryWorkflowsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["overview.newBackup"].isEnabled)

        app.buttons["overview.newBackup"].click()
        XCTAssertTrue(app.staticTexts["New Backup"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.textFields["backup.name"].exists)
        XCTAssertFalse(app.buttons["backup.create"].isEnabled)
        app.buttons["Cancel"].click()

        app.staticTexts["Devices"].click()
        XCTAssertTrue(app.staticTexts["Your backup network"].waitForExistence(timeout: 3))
        app.buttons["devices.pair"].click()
        XCTAssertTrue(app.staticTexts["Secure Pairing"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Nearby advertisements remain untrusted until finalization."].exists)
        app.buttons["Close"].click()

        app.staticTexts["Settings"].click()
        XCTAssertTrue(app.staticTexts["Settings transfer"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Private identity keys and folder permissions never leave this device."].exists)
    }

    func testOverviewPassesSystemAccessibilityAudit() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))
        try app.performAccessibilityAudit()
    }

    func testNativeMenuBarQuickActionsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))

        let statusItem = app.statusItems.firstMatch
        XCTAssertTrue(statusItem.waitForExistence(timeout: 5))
        statusItem.click()

        XCTAssertTrue(app.menuItems["Open Covalent"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.menuItems["New Backup…"].exists)
        XCTAssertTrue(app.menuItems["Restore Latest Backup…"].exists)
        XCTAssertTrue(app.menuItems["Refresh Status"].exists)
        XCTAssertTrue(app.menuItems["Settings…"].exists)
        XCTAssertTrue(app.menuItems["Quit Covalent"].exists)
    }

    private func launchApp() throws -> XCUIApplication {
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        let testBundle = Bundle(for: CovalentMacUITests.self)
        let port = environment["COVALENT_UI_TEST_PORT"]
            ?? testBundle.object(forInfoDictionaryKey: "CovalentUITestPort") as? String
        let token = environment["COVALENT_UI_TEST_TOKEN"]
            ?? testBundle.object(forInfoDictionaryKey: "CovalentUITestToken") as? String
        app.launchEnvironment["COVALENT_UI_TEST_BASE_URL"] = "http://127.0.0.1:\(try XCTUnwrap(port))"
        app.launchEnvironment["COVALENT_UI_TEST_TOKEN"] = try XCTUnwrap(token)
        app.launch()
        return app
    }
}
