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
                    .secondaryLabelStyle()
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
        case .starting: "Covalent is checking that it can work with your backup server."
        case .needsAuthorization: "Add your backup server's access token. It stays in this Mac's Keychain."
        case .offline: "Start your backup server, then reconnect. Work already under way is saved and continues where it stopped."
        case .ready: "The service is ready."
        }
    }

    private var statusGrid: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), spacing: 16)], spacing: 16) {
            MacMetric(
                title: "Backups",
                value: "\(model.backups.count)",
                detail: model.snapshots.isEmpty ? "No backups created in this app" : "\(model.snapshots.count) recent backups",
                systemImage: "externaldrive"
            )
            MacMetric(
                title: "Extra copy devices",
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
        // The grid is vended to accessibility as a group holding the three
        // metric tiles, and a group with no name is exactly what the system
        // audit reports as "Element has no description". `children: .contain`
        // keeps each tile individually reachable and gives the group itself
        // the name a screen reader announces on entering it.
        .accessibilityElement(children: .contain)
        .accessibilityLabel("At a glance")
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
                // Deliberately not `ContentUnavailableView`. That view styles
                // its own title and message with the system secondary label
                // colour, which is 50% alpha and measured about 3.95:1 on the
                // white card behind it — the audit reported both its lines as
                // "Contrast failed". The copy and layout below are identical;
                // the difference is that the app, not the framework, decides
                // the colours, so they can be opaque and measurable.
                MacEmptyState(
                    systemImage: "externaldrive.badge.plus",
                    title: "No backups yet",
                    message: "Choose a folder and keep the first encrypted backup on this Mac, "
                        + "or on devices you select."
                ) {
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
                MacSafeguard(systemImage: "checkmark.shield", title: "Explicit extra copies", text: "Covalent never picks another storage device for you.")
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
                    .secondaryLabelStyle()
                Spacer()
            }
            Text(value)
                .font(.system(.largeTitle, design: .rounded).weight(.semibold))
            Text(detail)
                .font(.caption)
                .secondaryLabelStyle()
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
            // `accessibilityHidden` keeps this glyph out of the *tree*, not
            // out of the combined element's rectangle, and the audit grades
            // the rectangle. See `MacLabelColor.accentGlyph`.
            //
            // Filled, not outlined, and measured rather than assumed: the
            // outline variant declares its colour but its strokes are one
            // point wide, so sampling the audit's own element screenshot gives
            // a coverage-weighted #4786CE — 3.75:1, under the floor. A filled
            // glyph reaches nearly full coverage, so what it renders is what it
            // declared. Which then made the declared value the thing that
            // mattered, and it had been picked against the wrong backdrop:
            // these tiles sit on `windowBackgroundColor`, not on white. See
            // `MacLabelColor.accentGlyph`.
            Image(systemName: systemImage)
                .font(.title2.weight(.semibold))
                .symbolVariant(.fill)
                .foregroundStyle(MacLabelColor.accentGlyph)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                // Body size at medium weight, in the quieter colour. Three
                // sentences explaining what Covalent will and will not do on
                // its own deserve more than 11pt, which is the size this app
                // uses for timestamps; and this exact configuration —
                // `.body.weight(.medium)` in the secondary token — is the one
                // body-copy setting on this screen the audit has been observed
                // to accept, on the empty state's message. The two settings it
                // has refused here are 11pt medium and 13pt regular, so weight
                // and size are both at the accepted value rather than one step
                // below it.
                Text(text).font(.body.weight(.medium)).secondaryLabelStyle()
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
                Text(message).font(.subheadline).secondaryLabelStyle()
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
                .foregroundStyle(MacLabelColor.accentGlyph)
                .frame(width: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(snapshot.displayName).font(.headline)
                Text(snapshot.createdAt.formatted(.relative(presentation: .named)))
                    .font(.caption)
                    .secondaryLabelStyle()
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
