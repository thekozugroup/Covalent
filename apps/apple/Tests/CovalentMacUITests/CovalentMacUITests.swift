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
        let advanced = app.descendants(matching: .any)["devices.advancedRecovery"]
        XCTAssertTrue(advanced.waitForExistence(timeout: 3))
        scrollTo(advanced, in: app)
        expandDisclosure(advanced)
        XCTAssertEqual(
            String(describing: advanced.value ?? ""),
            "1",
            "Advanced recovery did not expand, so its contents cannot be reached."
        )
        let offlinePairing = app.buttons["devices.offlinePairing"]
        // Revealing the group pushes its contents below the fold, and macOS
        // keeps off-screen scroll content out of the accessibility tree, so
        // bring it into view before asserting — exactly as the iOS test does.
        scrollTo(offlinePairing, in: app)
        XCTAssertTrue(offlinePairing.waitForExistence(timeout: 3))
        XCTAssertTrue(offlinePairing.isHittable)
        offlinePairing.click()
        XCTAssertTrue(app.staticTexts["Secure Pairing"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Nearby advertisements remain untrusted until finalization."].exists)
        let close = app.buttons["pairing.close"]
        XCTAssertTrue(close.waitForExistence(timeout: 3))
        XCTAssertTrue(close.isHittable)
        close.click()
        XCTAssertTrue(waitForDisappearance(of: app.staticTexts["Secure Pairing"], timeout: 3))

        app.staticTexts["Settings"].click()
        XCTAssertTrue(app.staticTexts["Settings transfer"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["Private identity keys and folder permissions never leave this device."].exists)
    }

    func testOverviewPassesSystemAccessibilityAudit() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))
        try app.performAccessibilityAudit { issue in
            let details = "Accessibility audit: \(issue.compactDescription)\n\(issue.detailedDescription)\n\(issue.element?.debugDescription ?? "Element unavailable")"
            let attachment = XCTAttachment(string: details)
            attachment.name = "Accessibility audit element"
            attachment.lifetime = .keepAlways
            self.add(attachment)
            return false
        }
    }

    func testNativeMenuBarQuickActionsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))

        let statusItem = app.statusItems["Covalent"].firstMatch
        XCTAssertTrue(statusItem.waitForExistence(timeout: 10))
        XCTAssertTrue(statusItem.isHittable)
        statusItem.click()

        XCTAssertTrue(app.menuItems["Open Covalent"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.menuItems["New Backup…"].exists)
        XCTAssertTrue(app.menuItems["Restore Latest Backup…"].exists)
        XCTAssertTrue(app.menuItems["Refresh Status"].exists)
        XCTAssertTrue(app.menuItems["Settings…"].exists)
        XCTAssertTrue(app.menuItems["Quit Covalent"].exists)
    }

    /// Expands a macOS `DisclosureGroup`.
    ///
    /// The identifier resolves to an element covering the group's label, while
    /// the triangle that actually toggles it sits just outside that element's
    /// leading edge — so a plain `click()` lands on the label and leaves the
    /// group shut. Try the triangle first, then the element itself, stopping
    /// as soon as the control reports itself expanded.
    private func expandDisclosure(_ element: XCUIElement) {
        let targets = [
            element.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: -11, dy: 8)),
            element.coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.5)),
            element.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)),
        ]
        for target in targets {
            guard String(describing: element.value ?? "") != "1" else { return }
            target.click()
            _ = XCTWaiter.wait(
                for: [
                    XCTNSPredicateExpectation(
                        predicate: NSPredicate(format: "value == 1"),
                        object: element
                    )
                ],
                timeout: 2
            )
        }
    }

    /// Scrolls `element` into view, so an assertion measures whether the app
    /// offers the control — not whether it happened to be above the fold.
    ///
    /// macOS keeps off-screen scroll content out of the accessibility tree, so
    /// without this a perfectly good control reads as missing.
    private func scrollTo(_ element: XCUIElement, in app: XCUIApplication) {
        guard !element.exists else { return }
        let scrollView = app.scrollViews.firstMatch
        guard scrollView.waitForExistence(timeout: 3) else { return }
        for delta in [-60.0, -60.0, -60.0, -60.0, 60.0, 60.0, 60.0, 60.0] where !element.exists {
            scrollView.scroll(byDeltaX: 0, deltaY: CGFloat(delta))
        }
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

    private func waitForDisappearance(of element: XCUIElement, timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: element
        )
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }
}
