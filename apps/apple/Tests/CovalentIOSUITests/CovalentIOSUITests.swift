import XCTest

@MainActor
final class CovalentIOSUITests: XCTestCase {
    func testTierTwoPrimaryWorkflowsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.navigationBars["Covalent"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.descendants(matching: .any)["home.serviceHeader"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Apple UI Test Node"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Not a full-device or continuous background backup"].exists)

        app.tabBars.buttons["Backups"].tap()
        XCTAssertTrue(app.navigationBars["Backups"].waitForExistence(timeout: 3))
        app.buttons["backups.new"].tap()
        XCTAssertTrue(app.navigationBars["New Backup"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.textFields["backup.name"].exists)
        XCTAssertFalse(app.buttons["backup.create"].isEnabled)
        app.buttons["Cancel"].tap()

        app.tabBars.buttons["Devices"].tap()
        XCTAssertTrue(app.navigationBars["Devices"].waitForExistence(timeout: 3))
        let advanced = app.buttons["devices.advancedRecovery"]
        scrollTo(advanced, in: app)
        XCTAssertTrue(advanced.exists)
        advanced.tap()
        let offlinePairing = app.buttons["devices.offlinePairing"]
        scrollTo(offlinePairing, in: app)
        XCTAssertTrue(offlinePairing.exists)
        offlinePairing.tap()
        XCTAssertTrue(app.navigationBars["Secure Pairing"].waitForExistence(timeout: 3))
        XCTAssertTrue(
            app.staticTexts
                .containing(NSPredicate(format: "label CONTAINS %@", "Nearby advertisements alone remain untrusted"))
                .firstMatch
                .exists
        )
        app.buttons["Close"].tap()

        app.tabBars.buttons["Settings"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 3))
        let privateTransferNotice = app.staticTexts["Private identity keys, API tokens, backup keys, and folder permissions never leave this device."]
        scrollTo(privateTransferNotice, in: app)
        XCTAssertTrue(privateTransferNotice.exists)
        let tierNotice = app.staticTexts["Tier 2 selected-folder support"]
        scrollTo(tierNotice, in: app)
        XCTAssertTrue(tierNotice.exists)
    }

    func testHomePassesSystemAccessibilityAudit() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.descendants(matching: .any)["home.serviceHeader"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Apple UI Test Node"].waitForExistence(timeout: 20))
        try app.performAccessibilityAudit { issue in
            let elementDescription = issue.element?.debugDescription ?? "unknown element"
            print(
                "Accessibility audit: \(issue.auditType.rawValue) · "
                    + "\(issue.compactDescription) · \(issue.detailedDescription) · "
                    + elementDescription
            )
            return false
        }
    }

    private func scrollTo(_ element: XCUIElement, in app: XCUIApplication) {
        for _ in 0..<5 where !element.exists {
            app.swipeUp()
        }
    }

    private func launchApp() throws -> XCUIApplication {
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        let testBundle = Bundle(for: CovalentIOSUITests.self)
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
