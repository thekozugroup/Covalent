import SwiftUI

struct IOSHomeView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

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
        // Side by side normally; stacked at accessibility text sizes. Keeping
        // the row horizontal there squeezes the device name into a column one
        // character wide, because the two glyphs grow with the text and leave
        // it nothing to wrap into.
        let layout = dynamicTypeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 12))
            : AnyLayout(HStackLayout(alignment: .center, spacing: 14))
        return layout {
            HStack(spacing: 14) {
                Image(systemName: serviceSymbol)
                    .scaledSymbolFont(size: 25, weight: .semibold, relativeTo: .title2)
                    .foregroundStyle(serviceColor)
                    .scaledSymbolFrame(48, relativeTo: .title2)
                    .background(serviceColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
                    .accessibilityHidden(true)
                if dynamicTypeSize.isAccessibilitySize {
                    Spacer(minLength: 0)
                    protectedBadge
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(model.status?.deviceName ?? "This iPhone or iPad")
                    .font(.title2.weight(.semibold))
                    .fixedSize(horizontal: false, vertical: true)
                Text(model.serviceStatusLabel)
                    .font(.subheadline)
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if !dynamicTypeSize.isAccessibilitySize {
                protectedBadge
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("home.serviceHeader")
    }

    @ViewBuilder
    private var protectedBadge: some View {
        if model.phase == .ready {
            Label("Protected", systemImage: "checkmark.shield.fill")
                .labelStyle(.iconOnly)
                .font(.title2)
                .foregroundStyle(.primary)
                .accessibilityLabel("Authenticated service connection")
        }
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
            Text("Covalent protects only the folders you choose. iOS may suspend the app; your backup server saves its progress, so supported work continues when the app is active again.")
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
                        .scaledSymbolFont(size: 42)
                        .foregroundStyle(.primary)
                        .accessibilityHidden(true)
                    Text("No backups yet")
                        .font(.title3.weight(.semibold))
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                    Text("Choose a folder to make the first encrypted backup.")
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
            Label("Only the devices you choose", systemImage: "checkmark.shield")
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
            ? "Enter your backup server's address and access token. The token stays in this device's Keychain."
            : "Covalent could not reach your backup server. Check that it is switched on and that this device can reach its address."
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
