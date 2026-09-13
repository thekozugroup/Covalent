import AppKit
import XCTest

/// Hosted capability probe only. This does not replace populated-link keyboard,
/// VoiceOver-order, menu-bar, responsive-layout, or packaged Release acceptance.
@MainActor
final class HostedVoiceOverCapabilityProbe: XCTestCase {
    private let transitionTimeout: TimeInterval = 10
    private let probeDuration: TimeInterval = 45
    private let speechTarget = "Set Up This Mac"

    func testHostedRunnerCanExposeActualVoiceOverSpeech() throws {
        let softDeadline = Date().addingTimeInterval(probeDuration)
        let pasteboard = NSPasteboard.general
        let originalPasteboard = try snapshot(pasteboard)
        let initiallyEnabled = NSWorkspace.shared.isVoiceOverEnabled
        let app = try launchIsolatedFixture()
        var changedVoiceOver = false

        defer {
            let voiceOverRestored = restoreVoiceOver(
                initiallyEnabled: initiallyEnabled,
                changedVoiceOver: changedVoiceOver,
                using: app
            )
            let pasteboardRestored = restore(originalPasteboard, to: pasteboard)
            attachAuditResult(
                voiceOverRestored && pasteboardRestored
                    ? "Accessibility audit: VoiceOver state and pasteboard restored"
                    : "Accessibility audit: VoiceOver or pasteboard restoration failed"
            )
            XCTAssertTrue(voiceOverRestored, "VoiceOver probe must restore the initial VoiceOver state.")
            XCTAssertTrue(pasteboardRestored, "VoiceOver probe must restore every saved pasteboard item.")
            app.terminate()
        }

        let fixedLabel = app.buttons[speechTarget]
        guard fixedLabel.waitForExistence(timeout: remaining(upTo: transitionTimeout, before: softDeadline)) else {
            throw ProbeError.missingFixtureLabel
        }
        guard !initiallyEnabled else { throw ProbeError.voiceOverInitiallyEnabled }

        app.typeKey(.F5, modifierFlags: .command)
        changedVoiceOver = true
        try acceptObservedWelcomeIfPresent(before: softDeadline)
        guard waitForVoiceOver(enabled: true, before: softDeadline) else {
            throw ProbeError.voiceOverDidNotEnable
        }
        guard NSWorkspace.shared.isVoiceOverEnabled else {
            throw ProbeError.voiceOverDidNotEnable
        }

        app.activate()
        let copiedExpectedSpeech = try navigateAndCopySpeech(
            containing: speechTarget,
            app: app,
            pasteboard: pasteboard,
            before: softDeadline
        )
        attachAuditResult(
            copiedExpectedSpeech
                ? "Accessibility audit: VoiceOver speech matched fixed Covalent label"
                : "Accessibility audit: VoiceOver speech did not match fixed Covalent label"
        )
        guard copiedExpectedSpeech else { throw ProbeError.expectedSpeechNotCopied }

        let setupTitle = app.staticTexts["Set up Covalent"]
        guard setupTitle.exists else { throw ProbeError.voiceOverSetupActionFailed }
        app.typeKey(.space, modifierFlags: [.control, .option])
        guard waitForDisappearance(of: setupTitle, before: softDeadline),
              app.state == .runningForeground,
              app.windows.firstMatch.exists else {
            throw ProbeError.voiceOverSetupActionFailed
        }
        attachAuditResult("Accessibility audit: VoiceOver activated native setup action")
    }

    private func launchIsolatedFixture() throws -> XCUIApplication {
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        let testBundle = Bundle(for: HostedVoiceOverCapabilityProbe.self)
        let port = environment["COVALENT_UI_TEST_PORT"]
            ?? testBundle.object(forInfoDictionaryKey: "CovalentUITestPort") as? String
        let tokenFile = environment["COVALENT_UI_TEST_TOKEN_FILE"]
            ?? testBundle.object(forInfoDictionaryKey: "CovalentUITestTokenFile") as? String
        guard let port, let tokenFile else { throw ProbeError.missingFixtureConfiguration }
        app.launchEnvironment["COVALENT_UI_TEST_BASE_URL"] = "http://127.0.0.1:\(port)"
        app.launchEnvironment["COVALENT_UI_TEST_TOKEN_FILE"] = tokenFile
        app.launchEnvironment["COVALENT_UI_TEST_FIRST_LAUNCH"] = "1"
        app.launch()
        return app
    }

    private func acceptObservedWelcomeIfPresent(before deadline: Date) throws {
        let voiceOver = XCUIApplication(bundleIdentifier: "com.apple.VoiceOver")
        let welcome = voiceOver.staticTexts["Welcome to VoiceOver"]
        guard welcome.waitForExistence(timeout: remaining(upTo: 2, before: deadline)) else { return }
        let dialog = voiceOver.dialogs.firstMatch
        guard dialog.exists else { throw ProbeError.unexpectedWelcome }
        voiceOver.typeKey(.return, modifierFlags: [])
        guard waitForDisappearance(of: welcome, before: deadline),
              NSWorkspace.shared.isVoiceOverEnabled else {
            throw ProbeError.unexpectedWelcome
        }
    }

    private func navigateAndCopySpeech(
        containing expected: String,
        app: XCUIApplication,
        pasteboard: NSPasteboard,
        before deadline: Date
    ) throws -> Bool {
        for traversal in 0..<12 where Date() < deadline {
            pasteboard.clearContents()
            guard pasteboard.setString("Covalent VoiceOver probe marker", forType: .string) else {
                throw ProbeError.pasteboardWriteFailed
            }
            let markerChange = pasteboard.changeCount
            app.typeKey("c", modifierFlags: [.control, .option, .shift])
            let changed = waitForPasteboardChange(
                after: markerChange,
                pasteboard: pasteboard,
                before: deadline
            )
            let spoken = changed ? pasteboard.string(forType: .string) : nil
            if spoken?.range(of: expected, options: .caseInsensitive) != nil {
                return true
            }
            guard traversal < 11, Date() < deadline else { continue }
            let normalized = spoken?.lowercased()
            if normalized?.contains(" group") == true || normalized?.contains("scroll area") == true {
                app.typeKey(.downArrow, modifierFlags: [.control, .option, .shift])
            } else {
                app.typeKey(.rightArrow, modifierFlags: [.control, .option])
            }
        }
        return false
    }

    private func restoreVoiceOver(
        initiallyEnabled: Bool,
        changedVoiceOver: Bool,
        using app: XCUIApplication
    ) -> Bool {
        let voiceOver = XCUIApplication(bundleIdentifier: "com.apple.VoiceOver")
        let welcome = voiceOver.staticTexts["Welcome to VoiceOver"]
        if !initiallyEnabled, welcome.exists {
            let cleanupDeadline = Date().addingTimeInterval(transitionTimeout)
            voiceOver.typeKey(.escape, modifierFlags: [])
            return waitForDisappearance(of: welcome, before: cleanupDeadline)
                && waitForVoiceOver(enabled: false, before: cleanupDeadline)
        }
        let current = NSWorkspace.shared.isVoiceOverEnabled
        guard current != initiallyEnabled else { return true }
        guard changedVoiceOver else { return false }
        app.activate()
        app.typeKey(.F5, modifierFlags: .command)
        return waitForVoiceOver(
            enabled: initiallyEnabled,
            before: Date().addingTimeInterval(transitionTimeout)
        )
    }

    private func waitForVoiceOver(enabled: Bool, before deadline: Date) -> Bool {
        wait(before: deadline) { NSWorkspace.shared.isVoiceOverEnabled == enabled }
    }

    private func waitForPasteboardChange(
        after changeCount: Int,
        pasteboard: NSPasteboard,
        before deadline: Date
    ) -> Bool {
        wait(before: min(deadline, Date().addingTimeInterval(1))) {
            pasteboard.changeCount != changeCount
        }
    }

    private func waitForDisappearance(of element: XCUIElement, before deadline: Date) -> Bool {
        wait(before: deadline) { !element.exists }
    }

    private func wait(before deadline: Date, condition: () -> Bool) -> Bool {
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        return condition()
    }

    private func remaining(upTo maximum: TimeInterval, before deadline: Date) -> TimeInterval {
        max(0, min(maximum, deadline.timeIntervalSinceNow))
    }

    private func attachAuditResult(_ fixedResult: String) {
        let attachment = XCTAttachment(string: fixedResult + "\n")
        attachment.name = "Hosted VoiceOver capability"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private struct PasteboardSnapshot: Equatable {
        let items: [[String: Data]]
    }

    private func snapshot(_ pasteboard: NSPasteboard) throws -> PasteboardSnapshot {
        let items = try (pasteboard.pasteboardItems ?? []).map { item in
            try Dictionary(uniqueKeysWithValues: item.types.map { type in
                guard let data = item.data(forType: type) else {
                    throw ProbeError.pasteboardSnapshotFailed
                }
                return (type.rawValue, data)
            })
        }
        return PasteboardSnapshot(items: items)
    }

    private func restore(_ saved: PasteboardSnapshot, to pasteboard: NSPasteboard) -> Bool {
        pasteboard.clearContents()
        let items: [NSPasteboardItem] = saved.items.map { savedItem in
            let item = NSPasteboardItem()
            for (rawType, data) in savedItem {
                item.setData(data, forType: NSPasteboard.PasteboardType(rawType))
            }
            return item
        }
        guard items.isEmpty || pasteboard.writeObjects(items) else { return false }
        return (try? snapshot(pasteboard)) == saved
    }

    private enum ProbeError: Error {
        case missingFixtureConfiguration
        case missingFixtureLabel
        case voiceOverInitiallyEnabled
        case voiceOverDidNotEnable
        case expectedSpeechNotCopied
        case voiceOverSetupActionFailed
        case unexpectedWelcome
        case pasteboardSnapshotFailed
        case pasteboardWriteFailed
    }
}
