import SwiftUI

struct MacOverviewView: View {
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                header
                if model.phase != .ready {
                    connectionCallout
                }
                statusGrid
                recentBackups
                safeguards
            }
            .frame(maxWidth: 920, alignment: .leading)
            .padding(32)
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .navigationTitle("Overview")
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 7) {
                Text(greeting)
                    .font(.largeTitle.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("Private backups, placed only where you choose.")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("New Backup") { model.presentation = .newBackup }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(!model.isAuthorized || model.activeTask != nil)
                .accessibilityIdentifier("overview.newBackup")
        }
    }

    private var greeting: String {
        if let name = model.status?.deviceName {
            return "\(name) is protected here"
        }
        return "Welcome to Covalent"
    }

    private var connectionCallout: some View {
        MacCallout(
            title: calloutTitle,
            message: calloutMessage,
            systemImage: model.phase == .offline ? "bolt.horizontal.circle" : "key.horizontal",
            tint: model.phase == .offline ? .orange : .blue
        ) {
            Button(model.phase == .offline ? "Try Again" : "Connect") {
                if model.phase == .offline {
                    Task { await model.refresh() }
                } else {
                    model.presentation = .connection
                }
            }
            .buttonStyle(.borderedProminent)
        }
    }

    private var calloutTitle: String {
        switch model.phase {
        case .starting: "Connecting to the local service"
        case .needsAuthorization: "Finish local setup"
        case .offline: "The local service is offline"
        case .ready: "Ready"
        }
    }

    private var calloutMessage: String {
        switch model.phase {
        case .starting: "Covalent is checking protocol compatibility."
        case .needsAuthorization: "Add the node's local API token. It stays in this Mac's Keychain."
        case .offline: "Start covalent-node, then reconnect. Existing resumable jobs remain in the node's durable state."
        case .ready: "The service is ready."
        }
    }

    private var statusGrid: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), spacing: 16)], spacing: 16) {
            MacMetric(
                title: "Backups",
                value: "\(model.backups.count)",
                detail: model.snapshots.isEmpty ? "No snapshots created in this app" : "\(model.snapshots.count) recent snapshots",
                systemImage: "externaldrive"
            )
            MacMetric(
                title: "Replica devices",
                value: "\(model.providers.count)",
                detail: model.providers.isEmpty ? "Local-only is supported" : "Chosen per backup",
                systemImage: "square.3.layers.3d"
            )
            MacMetric(
                title: "LAN discovery",
                value: model.status?.lanDiscovery == true ? "On" : "Off",
                detail: model.status?.lanDiscovery == true ? "Nearby hints enabled" : "Manual and Tailscale paths still work",
                systemImage: model.status?.lanDiscovery == true ? "network" : "network.slash"
            )
        }
    }

    @ViewBuilder
    private var recentBackups: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Recent backups")
                    .font(.title2.weight(.semibold))
                Spacer()
                if !model.snapshots.isEmpty {
                    Button("See All") { model.selectedSection = .backups }
                }
            }
            if model.snapshots.isEmpty {
                ContentUnavailableView {
                    Label("No backups yet", systemImage: "externaldrive.badge.plus")
                } description: {
                    Text("Choose a folder and keep the first encrypted snapshot locally or on devices you select.")
                } actions: {
                    Button("Create Backup") { model.presentation = .newBackup }
                        .disabled(!model.isAuthorized)
                }
                .frame(maxWidth: .infinity, minHeight: 190)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            } else {
                VStack(spacing: 0) {
                    ForEach(model.snapshots.prefix(3)) { snapshot in
                        Button {
                            model.selectedSection = .backups
                        } label: {
                            MacSnapshotRow(snapshot: snapshot)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        if snapshot.id != model.snapshots.prefix(3).last?.id {
                            Divider().padding(.leading, 44)
                        }
                    }
                }
                .padding(.horizontal, 14)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            }
        }
    }

    private var safeguards: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Built around your choices")
                .font(.title2.weight(.semibold))
            HStack(alignment: .top, spacing: 28) {
                MacSafeguard(systemImage: "checkmark.shield", title: "Explicit replicas", text: "Covalent never picks another storage device for you.")
                MacSafeguard(systemImage: "folder.badge.gearshape", title: "Confined restores", text: "A signed preview stays beneath the folder you authorize.")
                MacSafeguard(systemImage: "person.crop.circle.badge.xmark", title: "Local control", text: "Core workflows need no Covalent account or hosted coordinator.")
            }
        }
    }
}

struct MacMetric: View {
    let title: String
    let value: String
    let detail: String
    let systemImage: String

    var body: some View {
        VStack(alignment: .leading, spacing: 13) {
            HStack {
                Label(title, systemImage: systemImage)
                    .foregroundStyle(.secondary)
                Spacer()
            }
            Text(value)
                .font(.system(.largeTitle, design: .rounded).weight(.semibold))
            Text(detail)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .padding(18)
        .frame(maxWidth: .infinity, minHeight: 136, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
        .accessibilityElement(children: .combine)
    }
}

struct MacSafeguard: View {
    let systemImage: String
    let title: String
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: systemImage)
                .font(.title2)
                .foregroundStyle(.blue)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(text).font(.subheadline).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

struct MacCallout<Actions: View>: View {
    let title: String
    let message: String
    let systemImage: String
    let tint: Color
    @ViewBuilder let actions: () -> Actions

    var body: some View {
        HStack(spacing: 16) {
            Image(systemName: systemImage)
                .font(.title2)
                .foregroundStyle(tint)
                .frame(width: 32)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(message).font(.subheadline).foregroundStyle(.secondary)
            }
            Spacer()
            actions()
        }
        .padding(16)
        .background(tint.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
        .overlay { RoundedRectangle(cornerRadius: 14).stroke(tint.opacity(0.22)) }
    }
}

struct MacSnapshotRow: View {
    let snapshot: SnapshotRecord

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "externaldrive.fill")
                .font(.title3)
                .foregroundStyle(.blue)
                .frame(width: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(snapshot.displayName).font(.headline)
                Text(snapshot.createdAt.formatted(.relative(presentation: .named)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 3) {
                Text(snapshot.bytesRead.formatted(.byteCount(style: .file)))
                    .font(.subheadline.monospacedDigit())
                Label(snapshot.integrity.label, systemImage: integritySymbol)
                    .font(.caption)
                    .foregroundStyle(integrityColor)
            }
        }
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }

    private var integritySymbol: String {
        switch snapshot.integrity {
        case .unknown: "questionmark.circle"
        case .checking: "arrow.triangle.2.circlepath"
        case .intact: "checkmark.seal.fill"
        case .degraded: "exclamationmark.triangle.fill"
        case .corrupt: "xmark.octagon.fill"
        }
    }

    private var integrityColor: Color {
        switch snapshot.integrity {
        case .intact: .green
        case .degraded, .checking: .orange
        case .corrupt: .red
        case .unknown: .secondary
        }
    }
}
