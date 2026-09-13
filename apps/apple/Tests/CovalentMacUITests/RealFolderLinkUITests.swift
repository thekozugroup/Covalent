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
        let responderPort = try XCTUnwrap(environment["COVALENT_REAL_UI_RESPONDER_PORT"])
        let responderToken = try privateToken(
            at: XCTUnwrap(environment["COVALENT_REAL_UI_RESPONDER_TOKEN_FILE"])
        )
        let sourceRoot = try XCTUnwrap(environment["COVALENT_REAL_UI_SOURCE_ROOT"])
        let destinationRoot = try XCTUnwrap(environment["COVALENT_REAL_UI_DESTINATION_ROOT"])
        recordPhase("Covalent phase: setup entered")
        let app = launchManagedApp()
        defer { app.terminate() }

        let setup = app.buttons["firstLaunch.setup"]
        XCTAssertTrue(setup.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(setup.isHittable)
        setup.click()
        XCTAssertTrue(
            waitForDisappearance(of: app.staticTexts["Set up Covalent"], timeout: transferTimeout)
        )
        recordPhase("Covalent phase: setup completed")

        recordPhase("Covalent phase: pairing entered")
        app.typeKey("2", modifierFlags: .command)
        XCTAssertTrue(app.staticTexts["Your devices"].waitForExistence(timeout: transitionTimeout))
        let devicesScrollView = app.scrollViews["devices.view"]
        XCTAssertTrue(devicesScrollView.waitForExistence(timeout: transitionTimeout))
        let address = devicesScrollView.textFields["devices.address"]
        scrollTo(address, in: devicesScrollView)
        XCTAssertTrue(
            address.waitForExistence(timeout: transitionTimeout),
            textFieldDiagnostics(in: devicesScrollView)
        )
        address.click()
        let responderAddress = "127.0.0.1:\(responderPort)"
        address.typeText(responderAddress)
        XCTAssertEqual(address.value as? String, responderAddress)
        let pair = devicesScrollView.buttons["Pair Device"]
        scrollTo(pair, in: devicesScrollView)
        let pairEnabled = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "enabled == true"),
            object: pair
        )
        XCTAssertEqual(
            XCTWaiter.wait(for: [pairEnabled], timeout: transitionTimeout),
            .completed,
            "Pair Device did not become enabled after the node was ready and the address was entered"
        )
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
        let sourcePeerID = try await waitForSoleProviderIdentity(
            port: responderPort,
            token: responderToken
        )
        recordPhase("Covalent phase: pairing completed")

        recordPhase("Covalent phase: manual link entered")
        recordPhase("Covalent phase: manual peer selection entered")
        app.typeKey("1", modifierFlags: .command)
        let linksScrollView = app.scrollViews["links.view"]
        XCTAssertTrue(linksScrollView.waitForExistence(timeout: transitionTimeout))
        let peerPicker = linksScrollView.popUpButtons["links.new.peer"]
        scrollTo(peerPicker, in: linksScrollView)
        XCTAssertTrue(peerPicker.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(peerPicker.isHittable)
        peerPicker.click()
        XCTAssertTrue(
            app.menuItems["Responder UI Peer"].waitForExistence(timeout: transitionTimeout),
            peerPickerDiagnostic(app: app, picker: peerPicker)
        )
        app.menuItems["Responder UI Peer"].click()
        recordPhase("Covalent phase: manual peer selection completed")

        recordPhase("Covalent phase: manual label entered")
        let name = linksScrollView.textFields["links.new.name"]
        scrollTo(name, in: linksScrollView)
        XCTAssertTrue(name.waitForExistence(timeout: transitionTimeout))
        name.click()
        name.typeText("Mac UI Link")
        recordPhase("Covalent phase: manual label completed")

        recordPhase("Covalent phase: manual source guidance entered")
        let sourceDeletion = app.staticTexts["Deleting a source file leaves destination copies untouched."]
        scrollTo(sourceDeletion, in: linksScrollView)
        XCTAssertTrue(sourceDeletion.waitForExistence(timeout: transitionTimeout))
        recordPhase("Covalent phase: manual source guidance completed")

        recordPhase("Covalent phase: manual destination guidance entered")
        let destinationDeletion = app.staticTexts["Files deleted at a destination stay deleted there, even if the source changes. The source and other destinations stay untouched."]
        scrollTo(destinationDeletion, in: linksScrollView)
        XCTAssertTrue(destinationDeletion.waitForExistence(timeout: transitionTimeout))
        recordPhase("Covalent phase: manual destination guidance completed")

        recordPhase("Covalent phase: manual cadence selection entered")
        let cadence = linksScrollView.popUpButtons["folder-link-cadence"]
        scrollTo(cadence, in: linksScrollView)
        guard cadence.waitForExistence(timeout: transitionTimeout) else {
            throw FixtureError.missing(cadencePickerDiagnostic(app: app))
        }
        guard cadence.isHittable else {
            throw FixtureError.missing("Transfers picker is not hittable; "
                + cadencePickerDiagnostic(app: app))
        }
        cadence.click()
        app.menuItems["Manual"].click()
        recordPhase("Covalent phase: manual cadence selection completed")

        recordPhase("Covalent phase: manual folder picker entered")
        let chooseSource = app.buttons["Choose Source Folder…"]
        recordPhase("Covalent phase: manual folder picker scroll entered")
        scrollTo(chooseSource, in: linksScrollView)
        XCTAssertTrue(chooseSource.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(chooseSource.isHittable)
        recordPhase("Covalent phase: manual folder picker scroll completed")
        recordPhase("Covalent phase: manual folder picker opening entered")
        chooseSource.click()
        try chooseFolder(at: sourceRoot, in: app)
        recordPhase("Covalent phase: manual folder picker completed")

        recordPhase("Covalent phase: manual offer and accept entered")
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
        recordPhase("Covalent phase: manual offer and accept completed")

        recordPhase("Covalent phase: manual idle wait entered")
        let idle = app.staticTexts["Idle. Run this link when you want to transfer changes."]
        scrollTo(idle, in: linksScrollView)
        XCTAssertTrue(idle.waitForExistence(timeout: transferTimeout))
        recordPhase("Covalent phase: manual idle wait completed")
        recordPhase("Covalent phase: manual link completed")

        recordPhase("Covalent phase: first run entered")
        let run = app.buttons["Run Mac UI Link now"]
        scrollTo(run, in: linksScrollView)
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
        let lastRunCompleted = app.staticTexts["Last run completed"]
        scrollTo(lastRunCompleted, in: linksScrollView)
        XCTAssertTrue(lastRunCompleted.waitForExistence(timeout: transferTimeout))
        recordPhase("Covalent phase: first run completed")

        recordPhase("Covalent phase: relaunch entered")
        let statusItemBeforeRelaunch = app.statusItems["Covalent"]
        XCTAssertTrue(statusItemBeforeRelaunch.waitForExistence(timeout: transitionTimeout))
        statusItemBeforeRelaunch.coordinate(
            withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)
        ).click()
        let quit = app.menuItems["Quit Covalent"]
        XCTAssertTrue(quit.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(quit.isHittable)
        quit.click()
        XCTAssertTrue(app.wait(for: .notRunning, timeout: transitionTimeout))
        try Data("packaged-rclone-after-relaunch".utf8).write(
            to: URL(fileURLWithPath: sourceRoot + "/after-relaunch.txt"),
            options: .atomic
        )
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: destinationRoot + "/after-relaunch.txt")
        )
        app.launch()
        XCTAssertFalse(app.staticTexts["Set up Covalent"].waitForExistence(timeout: 1))
        app.typeKey("1", modifierFlags: .command)
        let relaunchedLinks = app.scrollViews["links.view"]
        XCTAssertTrue(relaunchedLinks.waitForExistence(timeout: transferTimeout))
        recordPhase("Covalent phase: relaunch completed")

        recordPhase("Covalent phase: second run entered")
        let runAfterRelaunch = app.buttons["Run Mac UI Link now"]
        scrollTo(runAfterRelaunch, in: relaunchedLinks)
        XCTAssertTrue(runAfterRelaunch.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(runAfterRelaunch.isHittable)
        runAfterRelaunch.click()
        let copiedAfterRelaunch = await waitForFile(
            destinationRoot + "/after-relaunch.txt",
            bytes: Data("packaged-rclone-after-relaunch".utf8)
        )
        XCTAssertTrue(
            copiedAfterRelaunch,
            "The relaunched sandboxed app did not reuse its saved folder grant."
        )
        let relaunchedSourcePeerID = try await waitForSoleProviderIdentity(
            port: responderPort,
            token: responderToken
        )
        XCTAssertEqual(
            relaunchedSourcePeerID,
            sourcePeerID,
            "The relaunched app must reuse the peer identity protected by Keychain."
        )
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: sourceRoot + "/destination-only.txt"),
            "Relaunching must not allow destination content to write back to the source."
        )
        let completedAfterRelaunch = app.staticTexts["Last run completed"]
        scrollTo(completedAfterRelaunch, in: relaunchedLinks)
        XCTAssertTrue(completedAfterRelaunch.waitForExistence(timeout: transferTimeout))
        recordPhase("Covalent phase: second run completed")

        recordPhase("Covalent phase: menu and status entered")
        let editSettings = app.buttons["Edit Link Settings…"]
        scrollTo(editSettings, in: relaunchedLinks)
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
        recordPhase("Covalent phase: menu and status completed")
    }

    private func recordPhase(_ name: String) {
        XCTContext.runActivity(named: name) { _ in }
    }

    private func peerPickerDiagnostic(app: XCUIApplication, picker: XCUIElement) -> String {
        let menuItems = app.menuItems
        let labels = menuItems.allElementsBoundByIndex.prefix(12).map(\.label)
        return "Paired Device value: \(String(describing: picker.value)); "
            + "menu items: \(menuItems.count); first labels: \(labels)"
    }

    private func cadencePickerDiagnostic(app: XCUIApplication) -> String {
        let popUpButtons = app.popUpButtons
        let entries = popUpButtons.allElementsBoundByIndex.prefix(8).map {
            "identifier=\($0.identifier.debugDescription) label=\($0.label.debugDescription)"
        }
        return "Transfers picker target: folder-link-cadence; pop-up buttons=\(popUpButtons.count); "
            + "first entries: \(entries)"
    }

    private func launchManagedApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment.removeValue(forKey: "COVALENT_UI_TEST_BASE_URL")
        app.launchEnvironment.removeValue(forKey: "COVALENT_UI_TEST_TOKEN_FILE")
        app.launchEnvironment.removeValue(forKey: "COVALENT_UI_TEST_FIRST_LAUNCH")
        app.launchEnvironment["COVALENT_ADVERTISED_PEER_ADDRESS"] = "127.0.0.1:8787"
        app.launch()
        return app
    }

    private func chooseFolder(at path: String, in app: XCUIApplication) throws {
        let panel = app.dialogs["open-panel"]
        XCTAssertTrue(panel.waitForExistence(timeout: transitionTimeout))
        recordPhase("Covalent phase: manual folder picker opening completed")
        recordPhase("Covalent phase: manual folder picker location entered")
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
        recordPhase("Covalent phase: manual folder picker location completed")
        recordPhase("Covalent phase: manual folder picker confirmation entered")
        let choose = panel.buttons["Choose"]
        XCTAssertTrue(choose.waitForExistence(timeout: transitionTimeout))
        XCTAssertTrue(choose.isHittable)
        choose.click()
        XCTAssertTrue(waitForDisappearance(of: panel, timeout: transitionTimeout))
        recordPhase("Covalent phase: manual folder picker confirmation completed")
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

    private func waitForSoleProviderIdentity(port: String, token: String) async throws -> String {
        for _ in 0..<40 {
            if let providers = try await requestJSON(
                port: port,
                token: token,
                path: "/api/v1/providers"
            ) as? [[String: Any]],
               providers.count == 1,
               let peerID = providers.first?["peerId"] as? String,
               UUID(uuidString: peerID) != nil {
                return peerID.lowercased()
            }
            try await Task.sleep(nanoseconds: 250_000_000)
        }
        throw FixtureError.timeout("one paired source identity on the responder")
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

    private func textFieldDiagnostics(in scrollView: XCUIElement) -> String {
        let fields = scrollView.textFields.allElementsBoundByIndex.prefix(4)
        guard !fields.isEmpty else { return "Devices text fields: none" }
        return "Devices text fields: " + fields.map { field in
            let frame = field.frame
            return "identifier=\(field.identifier.debugDescription) "
                + "label=\(field.label.debugDescription) "
                + "placeholder=\((field.placeholderValue ?? "").debugDescription) "
                + "frame=(\(frame.origin.x),\(frame.origin.y),\(frame.width),\(frame.height))"
        }.joined(separator: "; ")
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
