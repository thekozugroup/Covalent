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
    #expect(throws: NodeClientError.invalidTrustedCertificate) {
        _ = try NodeConnectionConfiguration(
            baseURL: URL(string: "http://127.0.0.1:8787")!,
            apiToken: nil,
            trustedCertificateDER: Data("not a certificate".utf8)
        )
    }
}

@Test func exactTLSCertificateEnrollmentAcceptsPEMAndRequiresHTTPS() throws {
    let encoded = "MIIDETCCAfmgAwIBAgIUYsPev+VNpSHCYn0nYC3w+2SsWUEwDQYJKoZIhvcNAQELBQAwGDEWMBQGA1UEAwwNY292YWxlbnQudGVzdDAeFw0yNjA4MTYxMDE3MzBaFw0zNjA4MTMxMDE3MzBaMBgxFjAUBgNVBAMMDWNvdmFsZW50LnRlc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDEFxGkt0SWXJZYg07a+KyODIKOqdVWME/an7aRhjvsWQqgnymGxiX/SP3UkeGJmniv1GyLtMMs6HHIyQYm5fNTy79BXKhHbzXm9jXOcGZsg9cUYFv4Diw5jjk/m/UBwcST+YVNJ6lSuS3wbL1N9Lf1WF23Jo0GljMlCL3vrits0PzoM7BwkjUAtmEif0qj5NtwFQbkPie7q52ncv5BdAOLwEt5lw7TywHa0txlazf2YYFKywpTNs4zVZEGBXAJv596IDpHSCge/pyzzvPN3aPWBOKPa43jnzm6ns0X/pXUImGXulWWVsimfvZ8yB+bfyPVA0qsIwgwNksfBEOZQy8DAgMBAAGjUzBRMB0GA1UdDgQWBBRK/E13+jjcE1TYyefJvelp37GwVDAfBgNVHSMEGDAWgBRK/E13+jjcE1TYyefJvelp37GwVDAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQBSAqEDBH+zydx+MHkonv3T2HeTpXqnxl2nAgdbmiszaiiHUo5NO/OQPwWLLf3k1JnBh8G0g5jcOyKCKRmMIT4/9l085EeI9et3gyf7paKwo5zcO4+1i4S3ysdUKNx/6EuvnVrtlpR8YOWTmYK/zGbsvF+lJ5ppLwNDwRXwW82q6Zr21tgVqtrMqtdcflPVguvSs04J5TDwBlD3QVyCDIK3EZphjXZvSpKU5LpRTgKZlOVGon1itsTWHNzEgPtpGYPcyfL5U9Vc1PRwoctcd5bbDlG1W/7z5kT0f95zetjzZdAx1N2e72rGHbCD+W9jDETg5s3KVMUnBnzQovGWRXZx"
    let pem = Data("-----BEGIN CERTIFICATE-----\n\(encoded)\n-----END CERTIFICATE-----\n".utf8)
    let certificateDER = try SecureNodeConnectionStore.parseCertificateFile(pem)
    #expect(!certificateDER.isEmpty)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "https://covalent.test:8443")!,
        apiToken: String(repeating: "t", count: 32),
        trustedCertificateDER: certificateDER
    )
    #expect(configuration.trustedCertificateDER == certificateDER)
    #expect(throws: NodeClientError.invalidTrustedCertificate) {
        _ = try NodeConnectionConfiguration(
            baseURL: URL(string: "http://covalent.test:8443")!,
            apiToken: nil,
            trustedCertificateDER: certificateDER
        )
    }
    #expect(throws: NodeClientError.invalidTrustedCertificate) {
        _ = try SecureNodeConnectionStore.parseCertificateFile(Data("garbage".utf8))
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
