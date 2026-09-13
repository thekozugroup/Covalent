import AppKit
import Foundation
import XCTest

@MainActor
final class RealFolderLinkUITests: XCTestCase {
    private let transitionTimeout: TimeInterval = 10
    private let transferTimeout: TimeInterval = 40

    override func record(_ issue: XCTIssue) {
        let location = issue.sourceCodeContext.location
        let details = "UI test failure: \(issue.compactDescription)\n"
            + "\(location?.fileURL.path ?? "unknown file"):\(location?.lineNumber ?? 0)\n"
        let attachment = XCTAttachment(string: details)
        attachment.name = "Real folder-link UI test failure location"
        attachment.lifetime = .keepAlways
        add(attachment)
        super.record(issue)
    }

    func testNativeManualFolderLinkTransfersOneWayAndUpdatesMenuBar() async throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let sourcePort = try XCTUnwrap(environment["COVALENT_REAL_UI_SOURCE_PORT"])
        let responderPort = try XCTUnwrap(environment["COVALENT_REAL_UI_RESPONDER_PORT"])
        let responderToken = try privateToken(
            at: XCTUnwrap(environment["COVALENT_REAL_UI_RESPONDER_TOKEN_FILE"])
        )
        let sourceRoot = try XCTUnwrap(environment["COVALENT_REAL_UI_SOURCE_ROOT"])
        let destinationRoot = try XCTUnwrap(environment["COVALENT_REAL_UI_DESTINATION_ROOT"])
        let app = try launchApp(port: sourcePort)

        app.typeKey("2", modifierFlags: .command)
        XCTAssertTrue(app.staticTexts["Your devices"].waitForExistence(timeout: transitionTimeout))
        let address = app.textFields["Tailscale hostname or IP"]
        XCTAssertTrue(address.waitForExistence(timeout: transitionTimeout))
        address.click()
        address.typeText("127.0.0.1:\(responderPort)")
        let pair = app.scrollViews["devices.view"].buttons["Pair Device"]
        XCTAssertTrue(pair.isHittable)
        pair.click()

        let pending = try await waitForIncomingPairing(port: responderPort, token: responderToken)
        let pairingID = try string(pending, "pairingId")
        let code = try string(pending, "authenticationString")
        let comparisonCode = app.staticTexts[
            "Comparison code \(code.replacingOccurrences(of: "-", with: " "))"
        ]
        XCTAssertTrue(comparisonCode.waitForExistence(timeout: transitionTimeout))
        let confirm = app.buttons["networkPairing.confirm"]
        XCTAssertTrue(confirm.isHittable)
        confirm.click()
        XCTAssertTrue(app.staticTexts["Waiting for Other Device"].waitForExistence(timeout: transitionTimeout))
        _ = try await requestJSON(
            port: responderPort,
            token: responderToken,
            path: "/api/v1/pair/network/\(pairingID)/confirm",
            method: "POST",
            body: ["displayedCode": code]
        )
        XCTAssertTrue(app.staticTexts["Pairing Complete"].waitForExistence(timeout: transferTimeout))
        XCTAssertTrue(app.staticTexts["Device ready for links"].waitForExistence(timeout: transitionTimeout))
        app.buttons["Done"].click()
        XCTAssertTrue(waitForDisappearance(of: app.staticTexts["Pairing Complete"], timeout: transitionTimeout))

        app.typeKey("1", modifierFlags: .command)
        let linksScrollView = try XCTUnwrap(
            app.scrollViews.allElementsBoundByIndex.max { $0.frame.width < $1.frame.width }
        )
        let peerPicker = app.popUpButtons["Paired Device"]
        scrollTo(peerPicker, in: linksScrollView)
        XCTAssertTrue(peerPicker.waitForExistence(timeout: transitionTimeout))
        peerPicker.click()
        XCTAssertTrue(app.menuItems["Responder UI Peer"].waitForExistence(timeout: transitionTimeout))
        app.menuItems["Responder UI Peer"].click()
        let name = app.textFields["Folder Name"]
        name.click()
        name.typeText("Mac UI Link")
        let sourceDeletion = app.staticTexts["Deleting a source file leaves destination copies untouched."]
        scrollTo(sourceDeletion, in: linksScrollView)
        XCTAssertTrue(sourceDeletion.waitForExistence(timeout: transitionTimeout))
        let destinationDeletion = app.staticTexts["Files deleted at a destination stay deleted there, even if the source changes. The source and other destinations stay untouched."]
        scrollTo(destinationDeletion, in: linksScrollView)
        XCTAssertTrue(destinationDeletion.waitForExistence(timeout: transitionTimeout))
        let cadence = app.popUpButtons["Transfers"]
        scrollTo(cadence, in: linksScrollView)
        XCTAssertTrue(cadence.waitForExistence(timeout: transitionTimeout))
        cadence.click()
        app.menuItems["Manual"].click()

        let chooseSource = app.buttons["Choose Source Folder…"]
        scrollTo(chooseSource, in: linksScrollView)
        XCTAssertTrue(chooseSource.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(chooseSource.isHittable)
        chooseSource.click()
        try chooseFolder(at: sourceRoot, in: app)

        let offer = try await waitForIncomingOffer(
            port: responderPort,
            token: responderToken,
            label: "Mac UI Link"
        )
        _ = try await requestJSON(
            port: responderPort,
            token: responderToken,
            path: "/api/v1/sync/accept",
            method: "POST",
            body: [
                "offerId": try string(offer, "offerId"),
                "selectedRoot": destinationRoot,
            ]
        )

        let idle = app.staticTexts["Idle. Run this link when you want to transfer changes."]
        XCTAssertTrue(idle.waitForExistence(timeout: transferTimeout))
        let run = app.buttons["Run Mac UI Link now"]
        XCTAssertTrue(run.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(run.isHittable)
        run.click()
        let copiedForward = await waitForFile(
            destinationRoot + "/forward.txt",
            bytes: Data("packaged-rclone-forward-content".utf8)
        )
        XCTAssertTrue(
            copiedForward,
            "rclone test fixture did not copy the source file to the responder."
        )
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: sourceRoot + "/destination-only.txt"),
            "Destination-only content must never write back to the source."
        )
        XCTAssertEqual(
            try Data(contentsOf: URL(fileURLWithPath: destinationRoot + "/destination-only.txt")),
            Data("destination-must-not-write-back".utf8)
        )
        XCTAssertTrue(app.staticTexts["Last run completed"].waitForExistence(timeout: transferTimeout))

        let editSettings = app.buttons["Edit Link Settings…"]
        scrollTo(editSettings, in: linksScrollView)
        XCTAssertTrue(editSettings.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(editSettings.isHittable)
        editSettings.click()
        let settingsTitle = app.staticTexts["Mac UI Link Settings"]
        XCTAssertTrue(settingsTitle.waitForExistence(timeout: transitionTimeout))
        let cancelSettings = app.sheets.buttons["Cancel"]
        XCTAssertTrue(cancelSettings.isHittable)
        cancelSettings.click()
        XCTAssertTrue(waitForDisappearance(of: settingsTitle, timeout: transitionTimeout))
        try auditMainWindow(in: app, timeout: transitionTimeout)
        continueAfterFailure = false

        let statusItem = app.statusItems["Covalent"]
        XCTAssertTrue(statusItem.waitForExistence(timeout: transitionTimeout))
        statusItem.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).click()
        let linkStatus = app.menuItems["Mac UI Link — Last run completed"]
        XCTAssertTrue(linkStatus.waitForExistence(timeout: transitionTimeout))
        XCTAssertFalse(linkStatus.label.isEmpty)
        let completed = app.menuItems["Last run completed"]
        XCTAssertTrue(completed.waitForExistence(timeout: transitionTimeout))
        XCTAssertFalse(completed.label.isEmpty)
        let runAgain = app.menuItems["Run Mac UI Link Now"]
        XCTAssertTrue(runAgain.waitForExistence(timeout: transitionTimeout))
        XCTAssertFalse(runAgain.label.isEmpty)
    }

    private func launchApp(port: String) throws -> XCUIApplication {
        let environment = ProcessInfo.processInfo.environment
        let bundle = Bundle(for: RealFolderLinkUITests.self)
        let tokenFile = environment["COVALENT_UI_TEST_TOKEN_FILE"]
            ?? bundle.object(forInfoDictionaryKey: "CovalentUITestTokenFile") as? String
        let app = XCUIApplication()
        app.launchEnvironment["COVALENT_UI_TEST_BASE_URL"] = "http://127.0.0.1:\(port)"
        app.launchEnvironment["COVALENT_UI_TEST_TOKEN_FILE"] = try XCTUnwrap(tokenFile)
        app.launch()
        return app
    }

    private func chooseFolder(at path: String, in app: XCUIApplication) throws {
        let panel = app.dialogs["open-panel"]
        XCTAssertTrue(panel.waitForExistence(timeout: transitionTimeout))
        panel.typeKey("g", modifierFlags: [.command, .shift])
        let comboBox = app.sheets.comboBoxes.firstMatch
        let textField = app.sheets.textFields.firstMatch
        let location: XCUIElement
        if comboBox.waitForExistence(timeout: transitionTimeout / 2) {
            location = comboBox
        } else {
            location = textField
            XCTAssertTrue(location.waitForExistence(timeout: transitionTimeout / 2))
        }
        location.typeText(path)
        location.typeKey(.return, modifierFlags: [])
        XCTAssertTrue(waitForDisappearance(of: location, timeout: transitionTimeout))
        let choose = panel.buttons["Choose"]
        XCTAssertTrue(choose.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(choose.isHittable)
        choose.click()
        XCTAssertTrue(waitForDisappearance(of: panel, timeout: transitionTimeout))
    }

    private func privateToken(at path: String) throws -> String {
        try String(contentsOfFile: path, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func requestJSON(
        port: String,
        token: String,
        path: String,
        method: String = "GET",
        body: [String: Any]? = nil
    ) async throws -> Any {
        var request = URLRequest(url: try XCTUnwrap(URL(string: "http://127.0.0.1:\(port)\(path)")))
        request.httpMethod = method
        request.timeoutInterval = transitionTimeout
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let body {
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = try XCTUnwrap(response as? HTTPURLResponse).statusCode
        guard status == 200 else {
            throw FixtureError.http(status, String(decoding: data.prefix(2_048), as: UTF8.self))
        }
        return try JSONSerialization.jsonObject(with: data)
    }

    private func waitForIncomingPairing(port: String, token: String) async throws -> [String: Any] {
        for _ in 0..<80 {
            if let items = try await requestJSON(
                port: port,
                token: token,
                path: "/api/v1/pair/network/pending"
            ) as? [[String: Any]],
               let pending = items.first(where: {
                   $0["direction"] as? String == "incoming"
                       && $0["state"] as? String == "awaiting_local_confirmation"
               }) {
                return pending
            }
            try await Task.sleep(nanoseconds: 250_000_000)
        }
        throw FixtureError.timeout("incoming network pairing")
    }

    private func waitForIncomingOffer(
        port: String,
        token: String,
        label: String
    ) async throws -> [String: Any] {
        for _ in 0..<120 {
            do {
                if let status = try await requestJSON(
                    port: port,
                    token: token,
                    path: "/api/v1/sync/status"
                ) as? [String: Any],
                   let shares = status["shares"] as? [[String: Any]],
                   let offer = shares.first(where: {
                       $0["label"] as? String == label && $0["incoming"] as? Bool == true
                   }) {
                    return offer
                }
            } catch FixtureError.http(let status, let body)
                where status == 503 && body.contains("folder_sync_busy") {
                // Initial source indexing is bounded and retryable.
            }
            try await Task.sleep(nanoseconds: 250_000_000)
        }
        throw FixtureError.timeout("incoming folder offer")
    }

    private func waitForFile(_ path: String, bytes: Data) async -> Bool {
        for _ in 0..<160 {
            if let actual = try? Data(contentsOf: URL(fileURLWithPath: path)), actual == bytes {
                return true
            }
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
        return false
    }

    private func string(_ object: [String: Any], _ key: String) throws -> String {
        guard let value = object[key] as? String, !value.isEmpty else {
            throw FixtureError.missing(key)
        }
        return value
    }

    private func waitForDisappearance(of element: XCUIElement, timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: element
        )
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }

    private func scrollTo(_ element: XCUIElement, in scrollView: XCUIElement) {
        guard !element.isHittable else { return }
        let page = scrollView.frame.height * 0.75
        for delta in Array(repeating: -page, count: 8) + Array(repeating: page, count: 8)
        where !element.isHittable {
            scrollView.scroll(byDeltaX: 0, deltaY: delta)
        }
    }

    private enum FixtureError: Error {
        case http(Int, String)
        case missing(String)
        case timeout(String)
    }
}
