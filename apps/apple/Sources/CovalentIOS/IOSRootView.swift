import SwiftUI

struct IOSRootView: View {
    @State private var status: NodeStatus?
    @State private var isUnavailable = false
    private let client = NodeClient()

    var body: some View {
        NavigationStack {
            List {
                Section("This iPhone or iPad") {
                    if let status {
                        LabeledContent("Device", value: status.deviceName)
                        LabeledContent("LAN discovery", value: status.lanDiscovery ? "On" : "Off")
                    } else if isUnavailable {
                        Label("Local service unavailable", systemImage: "exclamationmark.triangle")
                    } else {
                        ProgressView("Connecting…")
                    }
                }
                Section("iOS support") {
                    Label("Tier 2", systemImage: "iphone")
                    Text("Covalent backs up only folders you select. iOS background limits may pause work; jobs resume when the system allows.")
                        .foregroundStyle(.secondary)
                }
                Section("Safety") {
                    Label("Explicit replica devices", systemImage: "checkmark.shield")
                    Label("Authorized restore folders", systemImage: "folder.badge.gearshape")
                }
            }
            .navigationTitle("Covalent")
            .task { await loadStatus() }
        }
    }

    @MainActor
    private func loadStatus() async {
        do {
            status = try await client.status()
            isUnavailable = false
        } catch {
            isUnavailable = true
        }
    }
}
