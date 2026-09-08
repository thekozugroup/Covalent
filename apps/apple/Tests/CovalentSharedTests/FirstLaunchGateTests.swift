import Foundation
import Testing
@testable import CovalentShared

@MainActor
private final class FirstLaunchGate: LocalNodeBootstrapping {
    private(set) var startModes: [LocalNodeStartupMode] = []
    var disposition: LocalNodeStartupDisposition
    var failStarts = false

    init(_ disposition: LocalNodeStartupDisposition) {
        self.disposition = disposition
    }

    func startupDisposition() throws -> LocalNodeStartupDisposition { disposition }

    func start(mode: LocalNodeStartupMode) async throws -> NodeConnectionConfiguration {
        startModes.append(mode)
        if failStarts { throw GateError.startFailed }
        return .localDefault
    }

    private enum GateError: Error { case startFailed }
}

@Test @MainActor func freshManagedMacShowsChoiceBeforeStartingANode() async {
    let gate = FirstLaunchGate(.needsFirstLaunchChoice)
    let model = CovalentAppModel(
        configuration: .localDefault,
        localNodeBootstrapper: gate
    )

    await model.start()

    #expect(model.needsFirstLaunchChoice)
    #expect(model.presentation == .firstLaunchSetup)
    #expect(gate.startModes.isEmpty, "A fresh Mac must not create an identity before the choice.")
}

@Test @MainActor func existingManagedIdentityBypassesFirstLaunchChoice() async {
    let gate = FirstLaunchGate(.existingIdentity)
    let model = CovalentAppModel(
        configuration: .localDefault,
        localNodeBootstrapper: gate
    )

    await model.start()

    #expect(!model.needsFirstLaunchChoice)
    #expect(gate.startModes.count == 1)
    guard case .normal? = gate.startModes.first else {
        Issue.record("An existing identity must start only the normal service path")
        return
    }
}

@Test @MainActor func recoveryFailureAfterPublishingIdentityReturnsToNormalStartup() async {
    let gate = FirstLaunchGate(.needsFirstLaunchChoice)
    let model = CovalentAppModel(
        configuration: .localDefault,
        localNodeBootstrapper: gate
    )
    await model.start()

    // Model the core helper publishing the recovered identity, then failing
    // while waiting for its ready-file. The next launch must be normal; it
    // must not leave the recovery gate over a durable identity.
    gate.disposition = .existingIdentity
    gate.failStarts = true
    await model.beginRecoveryFirstLaunch(
        recoveryKitFile: URL(fileURLWithPath: "/tmp/kit"),
        recoveryKeyFile: URL(fileURLWithPath: "/tmp/code")
    )

    #expect(!model.needsFirstLaunchChoice)
    #expect(gate.startModes.count == 2)
    guard case .recover? = gate.startModes.first,
          case .normal? = gate.startModes.last else {
        Issue.record("A published recovered identity must resume only through normal startup")
        return
    }
}
