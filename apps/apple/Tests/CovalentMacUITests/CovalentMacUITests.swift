import XCTest

@MainActor
final class CovalentMacUITests: XCTestCase {
    /// How long a screen transition may take before the test gives up.
    ///
    /// This is a tolerance for runner contention, not an assertion. Three
    /// seconds was too tight — it encoded an assumption about machine speed —
    /// but thirty was set on a gate that had never once passed, without any
    /// measurement showing timing was the problem, and it is far too loose: a
    /// pane that took twenty-five seconds to appear would be a serious bug and
    /// would still pass.
    ///
    /// There is now a measurement. CI run 32461742319 reports
    /// `IDETestOperationsObserverDebug: 43.053 elapsed -- Testing started
    /// completed` for all three tests on a real runner, including three app
    /// launches, and the failure in that run was audit findings rather than
    /// any wait. Ten seconds is what the launch assertions already use, and it
    /// is several times the largest transition that has ever been observed.
    private let uiTransitionTimeout: TimeInterval = 10

    func testTierOneNavigationAndPrimaryWorkflowsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["overview.newBackup"].isEnabled)

        app.buttons["overview.newBackup"].click()
        XCTAssertTrue(app.staticTexts["New Backup"].waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(app.textFields["backup.name"].exists)
        XCTAssertFalse(app.buttons["backup.create"].isEnabled)
        app.buttons["Cancel"].click()

        app.staticTexts["Devices"].click()
        XCTAssertTrue(app.staticTexts["Your backup network"].waitForExistence(timeout: uiTransitionTimeout))
        let advanced = app.descendants(matching: .any)["devices.advancedRecovery"]
        XCTAssertTrue(advanced.waitForExistence(timeout: uiTransitionTimeout))
        scrollTo(advanced, in: app)
        XCTAssertTrue(advanced.isHittable, "Advanced recovery never became clickable.")
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
        XCTAssertTrue(offlinePairing.waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(offlinePairing.isHittable)
        offlinePairing.click()
        XCTAssertTrue(app.staticTexts["Secure Pairing"].waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(app.staticTexts["A device seen nearby is not trusted until you finish pairing with it."].exists)
        let close = app.buttons["pairing.close"]
        XCTAssertTrue(close.waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(close.isHittable)
        close.click()
        XCTAssertTrue(waitForDisappearance(of: app.staticTexts["Secure Pairing"], timeout: uiTransitionTimeout))

        app.staticTexts["Settings"].click()
        XCTAssertTrue(app.staticTexts["Settings transfer"].waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(app.staticTexts["Private identity keys and folder permissions never leave this device."].exists)
    }

    func testOverviewPassesSystemAccessibilityAudit() throws {
        let app = try launchApp()
        // Every audit finding still fails this test; stopping at the first one
        // only hid how many there were, which turned a single run into a
        // one-finding-at-a-time search. Report them all.
        continueAfterFailure = true
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))

        // ---------------------------------------------------------------
        // Why this audit is scoped to the main window.
        //
        // `performAccessibilityAudit()` walks everything the process vends
        // over the accessibility API, and two of those things are not
        // Covalent's user interface and cannot be described by it:
        //
        //  1. The **system TouchBar**, reported as
        //     `TouchBar, {{80, 0}, {685, 30}}, Disabled`, whose path is
        //     `Application -> TouchBar` — a sibling of the window, owned by
        //     AppKit. Covalent defines no `NSTouchBar`; this is the default
        //     bar the system synthesizes for any app, and there is no API by
        //     which an app names a bar it did not create.
        //
        //  2. A **Parent/Child mismatch on the window's full-screen button**,
        //     reported as `Group, {{65, 50}, {14, 14}}` whose path runs
        //     `Window -> Button "_XCUI:FullScreenWindow" -> Group`. The
        //     `_XCUI:` prefix is XCTest's own naming for the standard
        //     `NSWindow` title-bar buttons, so the mismatch is between two
        //     elements AppKit and the test harness produce between them. No
        //     Covalent view appears anywhere in that path.
        //
        // The scope is therefore the window. `performAccessibilityAudit` is
        // only available on `XCUIApplication`, not on `XCUIElement`, so the
        // window boundary is applied as a predicate on each finding's frame
        // rather than by auditing a subtree — same boundary, expressed the
        // only way the API allows. Finding (1) lies wholly above the window
        // (y 0-30 against a window starting at y 31) and is excluded by it.
        // Finding (2) is *inside* the window, so the frame test does not
        // reach it and it is ignored by name instead.
        //
        // Both exclusions are counted and asserted below, so neither can grow
        // silently into a place to hide a real finding.
        //
        //  3. The **two NavigationSplitView column containers**, reported as
        //     `Group, {{0, 31}, {1024, 692}}` and `Group, {{8, 39}, {220, 676}}`,
        //     each a direct child of `SplitGroup "main, SidebarNavigationSplitView"`.
        //     These are the one exclusion here that is *not* obviously
        //     framework-owned on sight, so it was tested rather than assumed.
        //     Two SwiftUI fixes were tried in CI run 32465148487 and the audit
        //     tree from that run shows exactly what each did:
        //
        //       - `.accessibilityElement(children: .contain)` plus a label, on
        //         the outermost view of the sidebar column, produced
        //         `Group {{8, 39}, {220, 676}}` (still unnamed)
        //           -> `Group {{8, 83}, {220, 632}}, label: 'Sections and service status'`.
        //         The name landed on a *new* group inside the container.
        //
        //       - A bare `.accessibilityLabel` on the outermost view of the
        //         detail column produced `Group {{0, 31}, {1024, 692}}` (still
        //         unnamed) -> `ScrollView, label: 'Overview'`. The name landed
        //         on the scroll view underneath it.
        //
        //     In both cases the label attached *below* the container, because
        //     the container is synthesized above every modifier the column
        //     closure can carry. Nothing in `MacRootView.swift` sits between
        //     the split view and these groups. The way to remove them is to
        //     stop using `NavigationSplitView`, which is a UI decision and not
        //     an accessibility fix; both labels above were kept, because naming
        //     the sidebar region and the content pane is worth having on its
        //     own.
        //
        // Read this as a narrowing of ownership, not of coverage. Every view
        // Covalent draws — sidebar, toolbar, detail pane, sheets — is inside
        // this window and is still audited, and the ten app-owned findings
        // this change was made alongside were *fixed*, not excluded: the
        // status grid was named, and seven contrast findings were traced to
        // their real causes (a 50%-alpha `.secondary`, then rendered stroke
        // coverage at small sizes, then two decorative glyphs inside combined
        // elements) and fixed at the source in `MacTextStyles.swift`,
        // `MacOverviewView.swift` and `MacRootView.swift`. If you are here
        // because you want a failing finding to go away, this is not the
        // precedent for it: exclude only elements that no Covalent source file
        // can reach, prove it the way entry 3 does, and add a counted
        // assertion so the exclusion cannot widen.
        // ---------------------------------------------------------------
        let window = app.windows["main"]
        XCTAssertTrue(window.waitForExistence(timeout: uiTransitionTimeout))
        let windowFrame = window.frame
        XCTAssertFalse(windowFrame.isEmpty, "Cannot scope the audit to a window with no frame.")

        var outsideWindow: [String] = []
        var harnessChrome: [String] = []
        var splitViewColumns: [CGRect] = []
        try app.performAccessibilityAudit { issue in
            let details = """
                Accessibility audit: \(issue.compactDescription)
                \(issue.detailedDescription)
                \(issue.element?.debugDescription ?? "Element unavailable")
                """
            let elementFrame = issue.element?.frame ?? .null
            let isOutsideWindow = !elementFrame.isNull
                && !elementFrame.isEmpty
                && !elementFrame.intersects(windowFrame)
            let isHarnessChrome = issue.auditType == .parentChild
                && details.contains("_XCUI:FullScreenWindow")
            // Deliberately narrow. "Descends from the split view" would match
            // nearly every element in the app, so the element must also be an
            // unnamed group with the shape of a column: either the whole
            // window (the detail column reports the window's own frame) or a
            // full-height strip no wider than a third of it. Height alone was
            // not enough — the detail pane *is* the window height, so any
            // full-height content group would have qualified.
            let looksLikeAColumn = elementFrame == windowFrame
                || (elementFrame.width <= windowFrame.width / 3
                    && elementFrame.height >= windowFrame.height * 0.9)
            let isSplitViewColumn = issue.auditType == .sufficientElementDescription
                && issue.element?.elementType == .group
                && issue.element?.label.isEmpty == true
                && details.contains("SidebarNavigationSplitView")
                && looksLikeAColumn

            let attachment = XCTAttachment(string: details)
            if isOutsideWindow {
                outsideWindow.append(issue.compactDescription)
                attachment.name = "Ignored: outside the main window"
            } else if isHarnessChrome {
                harnessChrome.append(issue.compactDescription)
                attachment.name = "Ignored: AppKit/XCTest window button"
            } else if isSplitViewColumn {
                splitViewColumns.append(elementFrame)
                attachment.name = "Ignored: NavigationSplitView column container"
            } else {
                attachment.name = "Accessibility audit element"
                // Also to stdout. The attachment only reaches the result
                // bundle, which on CI means a 100 MB artifact download before
                // anyone can read what failed, and `xcodebuild -quiet` prints
                // the failing test's name and nothing else. Every round trip
                // this gate has cost was spent finding that out again.
                print("COVALENT-AUDIT-FINDING\n\(details)")
            }
            attachment.lifetime = .keepAlways
            self.add(attachment)
            return isOutsideWindow || isHarnessChrome || isSplitViewColumn
        }

        // Negative controls. An exclusion nobody has watched fire is
        // indistinguishable from one that swallows real findings, so both are
        // pinned to the exact population they were written for. If AppKit or
        // XCTest stops producing either, these fail and the exclusion gets
        // deleted rather than quietly outliving its reason — and if a third
        // one appears, it fails too rather than being absorbed.
        XCTAssertEqual(
            outsideWindow.count,
            1,
            "Expected exactly the system TouchBar outside the window, got: \(outsideWindow)"
        )
        XCTAssertEqual(
            harnessChrome.count,
            1,
            "Expected exactly the window-button ancestry mismatch, got: \(harnessChrome)"
        )
        // Not just "there were two". A count alone would still hold if SwiftUI
        // started naming one column while the app grew an unnamed full-height
        // group of its own — the total would stay at two and a real finding
        // would disappear into the exclusion. Assert one of each shape.
        XCTAssertEqual(
            splitViewColumns.filter { $0 == windowFrame }.count,
            1,
            "Expected exactly one detail-column container spanning the window, got: \(splitViewColumns)"
        )
        XCTAssertEqual(
            splitViewColumns.filter { $0 != windowFrame }.count,
            1,
            "Expected exactly one sidebar-column container, got: \(splitViewColumns)"
        )
        XCTAssertEqual(
            splitViewColumns.count,
            2,
            """
            Expected exactly the two NavigationSplitView column containers, got: \(splitViewColumns)
            Three would mean the app grew a full-height unnamed group of its own; one or zero
            would mean SwiftUI stopped synthesizing them, and this exclusion should be deleted.
            """
        )
    }

    func testNativeMenuBarQuickActionsAreReachable() throws {
        let app = try launchApp()
        continueAfterFailure = false
        XCTAssertTrue(app.staticTexts["Apple UI Test Node is protected here"].waitForExistence(timeout: 10))

        let statusItem = app.statusItems["Covalent"].firstMatch
        XCTAssertTrue(statusItem.waitForExistence(timeout: 10))
        XCTAssertTrue(statusItem.isHittable)
        statusItem.click()

        XCTAssertTrue(app.menuItems["Open Covalent"].waitForExistence(timeout: uiTransitionTimeout))
        XCTAssertTrue(app.menuItems["New Backup…"].exists)
        XCTAssertTrue(app.menuItems["Restore Latest Backup…"].exists)
        XCTAssertTrue(app.menuItems["Refresh Status"].exists)
        XCTAssertTrue(app.menuItems["Settings…"].exists)
        XCTAssertTrue(app.menuItems["Quit Covalent"].exists)
    }

    /// Expands a macOS `DisclosureGroup`.
    ///
    /// Click the control itself first. A previous ordering reached outside the
    /// element's leading edge first, hunting for the triangle glyph: for this
    /// group that offset resolved to x=227 while the enclosing scroll view
    /// starts at x=228, so the click landed in the sidebar and the group was
    /// never touched. Anything the control offers has to be reachable within
    /// its own bounds, so that is where this looks; the leading-edge offsets
    /// remain as fallbacks and are kept small enough to stay inside the pane.
    private func expandDisclosure(_ element: XCUIElement) {
        let targets = [
            element.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)),
            element.coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.5)),
            element.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: -6, dy: 8)),
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
        // Scroll until the control can actually be clicked, not merely until
        // it exists. macOS keeps below-the-fold rows in the accessibility tree,
        // so `exists` goes true while the element is still off screen — and a
        // click at its coordinate then lands on nothing, silently doing
        // nothing at all.
        guard !element.isHittable else { return }
        let scrollView = app.scrollViews.firstMatch
        guard scrollView.waitForExistence(timeout: uiTransitionTimeout) else { return }
        for delta in [-60.0, -60.0, -60.0, -60.0, -60.0, 60.0, 60.0, 60.0, 60.0, 60.0]
        where !element.isHittable {
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
