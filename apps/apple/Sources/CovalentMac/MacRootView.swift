import SwiftUI

struct MacRootView: View {
    @State private var status: NodeStatus?
    @State private var errorMessage: String?
    private let client = NodeClient()

    var body: some View {
        NavigationSplitView {
            List {
                Label("Overview", systemImage: "square.grid.2x2")
                Label("Backups", systemImage: "externaldrive")
                Label("Devices", systemImage: "laptopcomputer.and.iphone")
                Label("Settings", systemImage: "gearshape")
            }
            .navigationTitle("Covalent")
        } detail: {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    Text("Private backup, clearly placed.")
                        .font(.largeTitle.weight(.semibold))
                        .accessibilityAddTraits(.isHeader)
                    statusCard
                    platformCard
                }
                .frame(maxWidth: 760, alignment: .leading)
                .padding(32)
            }
            .background(Color(nsColor: .windowBackgroundColor))
            .task { await loadStatus() }
        }
    }

    @ViewBuilder
    private var statusCard: some View {
        GroupBox("This Mac") {
            VStack(alignment: .leading, spacing: 10) {
                if let status {
                    Text(status.deviceName)
                        .font(.title2.weight(.semibold))
                    Label(
                        status.lanDiscovery ? "LAN discovery on" : "LAN discovery off",
                        systemImage: status.lanDiscovery ? "network" : "network.slash"
                    )
                    Text("Protocol \(status.protocolVersion) · \(status.platformTier.label)")
                        .foregroundStyle(.secondary)
                } else if let errorMessage {
                    Label(errorMessage, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.secondary)
                } else {
                    ProgressView("Connecting to the local service…")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
        }
    }

    private var platformCard: some View {
        GroupBox("Tier 1 safeguards") {
            VStack(alignment: .leading, spacing: 12) {
                Label("You choose every device that stores an extra copy.", systemImage: "checkmark.shield")
                Label("Restores stay beneath a folder you authorize.", systemImage: "folder.badge.gearshape")
                Label("No Covalent account or hosted coordinator required.", systemImage: "person.crop.circle.badge.xmark")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
        }
    }

    @MainActor
    private func loadStatus() async {
        do {
            status = try await client.status()
            errorMessage = nil
        } catch {
            errorMessage = "Local service unavailable. Start covalent-node to continue."
        }
    }
}
