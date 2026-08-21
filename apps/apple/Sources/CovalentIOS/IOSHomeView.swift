import SwiftUI

struct IOSHomeView: View {
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                serviceHeader

                if model.phase != .ready {
                    connectionCallout
                }

                tierCallout
                recentBackup
                safeguards
            }
            .padding()
        }
        .background(Color(uiColor: .systemGroupedBackground))
        .navigationTitle("Covalent")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
            }
        }
    }

    private var serviceHeader: some View {
        HStack(spacing: 14) {
            Image(systemName: serviceSymbol)
                .font(.system(size: 25, weight: .semibold))
                .foregroundStyle(serviceColor)
                .frame(width: 48, height: 48)
                .background(serviceColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(model.status?.deviceName ?? "This iPhone or iPad")
                    .font(.title2.weight(.semibold))
                Text(model.serviceStatusLabel)
                    .font(.subheadline)
                    .foregroundStyle(.primary)
            }
            Spacer()
            if model.phase == .ready {
                Label("Protected", systemImage: "checkmark.shield.fill")
                    .labelStyle(.iconOnly)
                    .font(.title2)
                    .foregroundStyle(.primary)
                    .accessibilityLabel("Authenticated service connection")
            }
        }
        .padding(16)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("home.serviceHeader")
    }

    private var connectionCallout: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(connectionTitle, systemImage: "exclamationmark.triangle.fill")
                .font(.headline)
                .foregroundStyle(model.phase == .offline ? .orange : .blue)
            Text(connectionMessage)
                .font(.subheadline)
                .foregroundStyle(.primary)
            Button(model.phase == .needsAuthorization ? "Connect Securely" : "Check Connection") {
                model.presentation = .connection
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))
    }

    private var tierCallout: some View {
        VStack(alignment: .leading, spacing: 9) {
            Label("iPhone and iPad support", systemImage: "iphone")
                .font(.headline)
            Text("Tier 2 protects only folders you explicitly choose. iOS may suspend the app; the node keeps durable checkpoints so supported jobs can resume when the app is active again.")
                .font(.subheadline)
                .foregroundStyle(.primary)
            Label("Not a full-device or continuous background backup", systemImage: "info.circle")
                .font(.caption.weight(.medium))
                .foregroundStyle(.primary)
        }
        .padding(16)
        .background(Color.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 16))
    }

    @ViewBuilder
    private var recentBackup: some View {
        VStack(alignment: .leading, spacing: 12) {
            recentBackupHeader
            if let snapshot = model.snapshots.first {
                Button {
                    model.selectedSection = .backups
                } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "externaldrive.fill").foregroundStyle(.blue)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(snapshot.displayName).font(.headline).foregroundStyle(.primary)
                            Text(snapshot.createdAt.formatted(date: .abbreviated, time: .shortened))
                                .font(.caption)
                                .foregroundStyle(.primary)
                        }
                        Spacer()
                        Text(snapshot.bytesRead.formatted(.byteCount(style: .file)))
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.primary)
                    }
                }
                .buttonStyle(.plain)
            } else {
                VStack(spacing: 12) {
                    Image(systemName: "externaldrive.badge.plus")
                        .font(.system(size: 42))
                        .foregroundStyle(.primary)
                        .accessibilityHidden(true)
                    Text("No snapshots yet")
                        .font(.title3.weight(.semibold))
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                    Text("Choose a folder to make the first encrypted snapshot.")
                        .font(.subheadline)
                        .foregroundStyle(.primary)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                    Button("New Backup") { model.requestNewBackup() }
                        .buttonStyle(.borderedProminent)
                        .tint(Color(red: 0, green: 0.27, blue: 0.58))
                        .disabled(!model.isAuthorized)
                        .accessibilityIdentifier("home.newBackup")
                }
                .frame(maxWidth: .infinity, minHeight: 190)
            }
        }
        .padding(16)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))
    }

    private var recentBackupHeader: some View {
        recentBackupTitle
    }

    private var recentBackupTitle: some View {
        Text("Recent backup")
            .font(.headline)
            .fixedSize(horizontal: false, vertical: true)
    }

    private var safeguards: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Built-in safeguards")
                .font(.headline)
                .fixedSize(horizontal: false, vertical: true)
            Label("Exact replica devices only", systemImage: "checkmark.shield")
                .fixedSize(horizontal: false, vertical: true)
            Label("Signed no-write restore preview", systemImage: "doc.text.magnifyingglass")
                .fixedSize(horizontal: false, vertical: true)
            Label("Persistent folder access you can revoke", systemImage: "folder.badge.gearshape")
                .fixedSize(horizontal: false, vertical: true)
        }
        .font(.subheadline)
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))
    }

    private var connectionTitle: String {
        model.phase == .needsAuthorization ? "Secure setup required" : "Local service unavailable"
    }

    private var connectionMessage: String {
        model.phase == .needsAuthorization
            ? "Enter the node address and local API token. The token stays in this device's Keychain."
            : "The app could not reach the configured node. Check that the node is running and this device can reach its address."
    }

    private var serviceSymbol: String {
        switch model.phase {
        case .starting: "arrow.trianglehead.2.clockwise.rotate.90"
        case .ready: "externaldrive.badge.checkmark"
        case .needsAuthorization: "key"
        case .offline: "bolt.slash"
        }
    }

    private var serviceColor: Color {
        switch model.phase {
        case .starting: .orange
        case .ready: .green
        case .needsAuthorization: .blue
        case .offline: .red
        }
    }
}
