import Foundation
import Testing
@testable import CovalentShared

@Test func nodeStatusDecodesRustContract() throws {
    let json = Data(#"{"deviceName":"Home Mac","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"foundation"}"#.utf8)
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
}
