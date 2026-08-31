import XCTest

@MainActor
final class CovalentIOSUITests: XCTestCase {
    /// How long a screen transition may take before the test gives up.
    ///
    /// This is a tolerance for runner contention, not an assertion. The three
    /// seconds it replaces encoded an assumption about machine speed: on a
    /// loaded CI runner the iOS test phase took 284s against a normal ~55s,
    /// and a tab switch missed its window while the app was working fine.
    /// What each assertion requires is unchanged — only the patience is.
    private let uiTransitionTimeout: TimeInterval = 30

    func testTierTwoPrimaryWorkflowsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.navigationBars["Covalent"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.descendants(matching: .any)["home.serviceHeader"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Apple UI Test Node"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Not a full-device or continuous background backup"].exists)

        app.tabBars.buttons["Backups"].tap()
        XCTAssertTrue(app.navigationBars["Backups"].waitForExistence(timeout: uiTransitionTimeout))
        app.buttons["backups.new"].tap()
        XCTAssertTrue(app.navigationBars["New Backup"].waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(app.textFields["backup.name"].exists)
        XCTAssertFalse(app.buttons["backup.create"].isEnabled)
        app.buttons["Cancel"].tap()

        app.tabBars.buttons["Devices"].tap()
        XCTAssertTrue(app.navigationBars["Devices"].waitForExistence(timeout: uiTransitionTimeout))
        let advanced = app.buttons["devices.advancedRecovery"]
        scrollTo(advanced, in: app)
        XCTAssertTrue(advanced.exists)
        advanced.tap()
        let offlinePairing = app.buttons["devices.offlinePairing"]
        scrollTo(offlinePairing, in: app)
        XCTAssertTrue(offlinePairing.exists)
        offlinePairing.tap()
        XCTAssertTrue(app.navigationBars["Secure Pairing"].waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(
            app.staticTexts
                .containing(NSPredicate(format: "label CONTAINS %@", "Seeing a device nearby is not enough to trust it"))
                .firstMatch
                .exists
        )
        app.buttons["Close"].tap()

        app.tabBars.buttons["Settings"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: uiTransitionTimeout))
        let privateTransferNotice = app.staticTexts["Private identity keys, API tokens, backup keys, and folder permissions never leave this device."]
        scrollTo(privateTransferNotice, in: app)
        XCTAssertTrue(privateTransferNotice.exists)
        let tierNotice = app.staticTexts["Selected-folder backups"]
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
        let tokenFile = environment["COVALENT_UI_TEST_TOKEN_FILE"]
            ?? testBundle.object(forInfoDictionaryKey: "CovalentUITestTokenFile") as? String
        app.launchEnvironment["COVALENT_UI_TEST_BASE_URL"] = "http://127.0.0.1:\(try XCTUnwrap(port))"
        app.launchEnvironment["COVALENT_UI_TEST_TOKEN_FILE"] = try XCTUnwrap(tokenFile)
        app.launch()
        return app
    }
}
