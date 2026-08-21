import Foundation
import Testing

@testable import CovalentShared

/// Covers the contract the alert surfaces rely on.
///
/// The first version of this feature read the alert from inside a deferred
/// `Task`. SwiftUI clears the alert's `isPresented` binding the moment any
/// button is tapped, so by the time that Task ran the alert — and the retry
/// closure — were already `nil`, and every recovery button silently did
/// nothing. Nothing caught it, because the only assertion was that the button
/// had a title. These tests assert the recovery is actually *taken*.
@Suite @MainActor struct AlertRecoveryTests {
    @Test func takingTheRecoverySurvivesTheAlertBeingClearedFirst() async {
        let model = CovalentAppModel(configuration: .localDefault)
        model.alert = AppAlert(
            title: "Secure pairing couldn't start",
            message: "That device runs an incompatible version.",
            recovery: .chooseAnotherDevice
        )

        // What the button body does: take the work synchronously...
        let recovery = model.takeAlertRecovery()
        #expect(recovery != nil, "an actionable hint must hand back work to run")
        // ...and only then does SwiftUI's binding clear the alert.
        model.clearAlert()

        await recovery?()
        #expect(model.selectedSection == .devices)
        #expect(model.alert == nil)
    }

    @Test func takingTheRecoveryAfterTheAlertIsClearedYieldsNothing() {
        let model = CovalentAppModel(configuration: .localDefault)
        model.alert = AppAlert(title: "t", message: "m", recovery: .retry)
        model.clearAlert()
        #expect(
            model.takeAlertRecovery() == nil,
            "reading the recovery after dismissal must not resurrect a stale action"
        )
    }

    @Test func hintsWithNoInAppActionHandBackNothing() {
        for hint in [RecoveryHint.none, .freeUpSpace, .checkNetworkSettings] {
            let model = CovalentAppModel(configuration: .localDefault)
            model.alert = AppAlert(title: "t", message: "m", recovery: hint)
            #expect(
                model.takeAlertRecovery() == nil,
                "\(hint) has no model-side action; the view owns it or there is none"
            )
        }
    }

    /// "Choose Folder" must land somewhere a folder can actually be chosen.
    /// Settings only lists and revokes existing grants.
    @Test func chooseFolderLandsOnASurfaceThatCanGrantFolders() async {
        let model = CovalentAppModel(configuration: .localDefault)
        model.alert = AppAlert(title: "t", message: "m", recovery: .chooseFolderAgain)
        let recovery = model.takeAlertRecovery()
        await recovery?()
        #expect(
            model.selectedSection == .backups,
            "Settings has no folder picker; sending the user there is a dead end"
        )
    }

    @Test func previewAgainClearsTheStalePreviewItIsRecoveringFrom() async {
        let model = CovalentAppModel(configuration: .localDefault)
        model.alert = AppAlert(title: "t", message: "m", recovery: .previewRestoreAgain)
        let recovery = model.takeAlertRecovery()
        await recovery?()
        #expect(model.restorePreview == nil)
        #expect(model.selectedSection == .backups)
    }

    @Test func reconnectOpensTheConnectionSheet() async {
        let model = CovalentAppModel(configuration: .localDefault)
        model.alert = AppAlert(title: "t", message: "m", recovery: .reconnect)
        let recovery = model.takeAlertRecovery()
        await recovery?()
        #expect(model.presentation == .connection)
    }
}
