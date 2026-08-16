import Foundation
import Testing
@testable import CovalentShared

@Test func nodeStatusDecodesRustContract() throws {
    let json = Data(#"{"deviceName":"Home Mac","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"}"#.utf8)
    let status = try JSONDecoder().decode(NodeStatus.self, from: json)
    #expect(status.deviceName == "Home Mac")
    #expect(status.protocolVersion == covalentProtocolVersion)
    #expect(status.platformTier == .tier1)
    #expect(status.lanDiscovery == false)
}

@Test func settingsEncodingHasNoPrivateIdentityField() throws {
    let settings = ExportedDeviceSettings(
        deviceName: "Home Mac",
        lanDiscoveryEnabled: false,
        rememberedBackups: []
    )
    let encoded = try JSONEncoder().encode(settings)
    let text = String(decoding: encoded, as: UTF8.self).lowercased()
    #expect(!text.contains("private"))
    #expect(!text.contains("identitykey"))
    #expect(!text.contains("bookmark"))
    #expect(!text.contains("token"))
}

@Test func committedSettingsFixtureDecodes() throws {
    let data = try Data(contentsOf: fixtureURL("settings-v1.json"))
    let settings = try JSONDecoder().decode(ExportedDeviceSettings.self, from: data)
    #expect(settings.schemaVersion == 1)
    #expect(settings.deviceName == "Living room Mac")
    #expect(settings.rememberedBackups.count == 1)
}

@Test func additivePairingFixtureDefaultsDecode() throws {
    let data = try Data(contentsOf: fixtureURL("pairing-invitation-v1.json"))
    let invitation = try JSONDecoder().decode(PairingInvitation.self, from: data)
    #expect(invitation.protocolVersion == covalentProtocolVersion)
    #expect(invitation.minimumProtocolVersion == 1)
    #expect(invitation.inviterDeviceName.isEmpty)
}

@Test func daemonAndClientGoldenContractsDecodeTogether() throws {
    let decoder = JSONDecoder()
    let backup = try decoder.decode(
        BackupSummary.self,
        from: Data(contentsOf: fixtureURL("backup-summary-v1.json"))
    )
    #expect(backup.snapshotCount == 3)
    #expect(backup.selectedProviderIds.count == 1)

    let error = try decoder.decode(
        APIErrorPayload.self,
        from: Data(contentsOf: fixtureURL("error-v1.json"))
    )
    #expect(error.protocolVersion == covalentProtocolVersion)
    #expect(error.code == "source_changed")
    #expect(error.retryable)

    let progress = try decoder.decode(
        TransferProgress.self,
        from: Data(contentsOf: fixtureURL("progress-v1.json"))
    )
    #expect(progress.protocolVersion == covalentProtocolVersion)
    #expect(progress.kind == .backup)
    #expect(progress.state == .running)

    let event = try decoder.decode(
        NodeEvent.self,
        from: Data(contentsOf: fixtureURL("event-v1.json"))
    )
    #expect(event.protocolVersion == covalentProtocolVersion)
    #expect(event.kind == .transferChanged)
    #expect(event.sequence == 17)
}

@Test func connectionConfigurationRejectsInvalidAndInsecureInputs() throws {
    #expect(throws: NodeClientError.invalidServiceURL) {
        _ = try NodeConnectionConfiguration(baseURL: URL(string: "file:///tmp/node")!, apiToken: nil)
    }
    #expect(throws: NodeClientError.invalidToken) {
        _ = try NodeConnectionConfiguration(baseURL: URL(string: "http://127.0.0.1:8787")!, apiToken: "short")
    }
}

@Test func directoryGrantRejectsNonFileURL() {
    #expect(throws: SelectedDirectoryError.notAFileURL) {
        _ = try SelectedDirectoryGrant.capture(
            url: URL(string: "https://example.com/folder")!,
            purpose: .backupSource
        )
    }
}

@Test @MainActor func appModelRoutesMenuAndPrimaryActionsWithoutDuplicatingState() {
    let model = CovalentAppModel(configuration: .localDefault)
    #expect(model.serviceStatusLabel == "Connecting")

    model.requestNewBackup()
    #expect(model.selectedSection == .backups)
    #expect(model.presentation == .newBackup)

    model.requestRestoreLatest()
    #expect(model.selectedSection == .backups)
    #expect(model.alert?.title == "No backup to restore")
}

private func fixtureURL(_ name: String) -> URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .appending(path: "fixtures/contracts/\(name)")
}
