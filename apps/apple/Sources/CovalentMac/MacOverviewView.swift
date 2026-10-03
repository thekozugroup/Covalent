import SwiftUI

struct MacOverviewView: View {
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        Form {
            Section {
                Text("Send each source folder to one or more paired devices. Destination files remain ordinary files.")
                    .secondaryLabelStyle()
                HStack {
                    Button("Create Link", systemImage: "folder.badge.plus") {
                        openLinks()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.isAuthorized)
                    .accessibilityIdentifier("status.createLink")

                    Button("Pair Device", systemImage: "laptopcomputer.and.iphone") {
                        openDevices()
                    }
                    .disabled(!model.isAuthorized)
                    .accessibilityIdentifier("status.pairDevice")
                }
            } header: {
                Text("One-Way Folder Links")
            }

            Section("This Mac") {
                if let status = model.status {
                    LabeledContent("Device") {
                        Text(status.deviceName)
                            .foregroundStyle(.primary)
                    }
                    .accessibilityIdentifier("status.device")
                }
                LabeledContent("Local Service") {
                    Text(model.serviceStatusLabel)
                        .foregroundStyle(.primary)
                }
                if model.phase != .ready {
                    Label(serviceGuidance, systemImage: serviceSymbol)
                        .foregroundStyle(.secondary)
                    if model.phase == .offline {
                        Button("Try Again") { Task { await model.refresh() } }
                    } else if model.phase == .needsAuthorization {
                        Button("Connect…") { model.presentation = .connection }
                    }
                }
            }

            Section("Links") {
                linkStatus
                Button("Open Links", systemImage: "link") { openLinks() }
            }

            Section("Paired Devices") {
                pairedDevices
                Button("Open Devices", systemImage: "laptopcomputer.and.iphone") { openDevices() }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Status")
        .accessibilityIdentifier("status.view")
        .task { await model.refreshFolders() }
    }

    @ViewBuilder
    private var linkStatus: some View {
        if model.folderSyncLoading && model.folderSyncStatus == nil {
            ProgressView("Checking links…")
        } else if let error = model.folderSyncError {
            Label(error, systemImage: "exclamationmark.triangle")
                .foregroundStyle(.secondary)
        } else if let status = model.folderSyncStatus, status.availability != "available" {
            Label("Link status is unavailable", systemImage: "exclamationmark.triangle")
            Text("Open Links for details and recovery actions.")
                .secondaryLabelStyle()
        } else if let status = model.folderSyncStatus, !activeLinkRows.isEmpty {
            ForEach(activeLinkRows) { share in
                HStack {
                    Label(share.label, systemImage: linkSymbol(for: share))
                    Spacer()
                    Text(status.displayLabel(for: share))
                        .secondaryLabelStyle()
                }
                .accessibilityElement(children: .combine)
            }
        } else {
            Label("No links yet", systemImage: "folder.badge.plus")
            Text("Pair a device, then choose a source folder.")
                .secondaryLabelStyle()
        }
    }

    @ViewBuilder
    private var pairedDevices: some View {
        let devices = SavedPeerDevice.merge(
            peers: model.folderSyncStatus?.peers ?? [],
            providers: model.providers
        )
        if !devices.isEmpty {
            ForEach(devices) { device in
                Label(
                    device.displayName ?? device.address,
                    systemImage: "laptopcomputer.and.iphone"
                )
            }
        } else {
            Label("No paired devices", systemImage: "laptopcomputer.and.iphone")
            Text("Pair a device before creating a link.")
                .secondaryLabelStyle()
        }
    }

    private var activeLinkRows: [FolderShare] {
        guard let shares = model.folderSyncStatus?.shares else { return [] }
        var seen = Set<UUID>()
        return shares.filter {
            $0.phase != .removed && seen.insert($0.folderId).inserted
        }
    }

    private var serviceGuidance: String {
        switch model.phase {
        case .starting: "Covalent is starting the local service."
        case .ready: "The local service is ready."
        case .needsAuthorization: "Connect to finish local setup."
        case .offline: "Start the local service, then try again."
        }
    }

    private var serviceSymbol: String {
        switch model.phase {
        case .starting: "arrow.triangle.2.circlepath"
        case .ready: "checkmark.circle"
        case .needsAuthorization: "questionmark.circle"
        case .offline: "exclamationmark.triangle"
        }
    }

    private func linkSymbol(for share: FolderShare) -> String {
        if share.phase == .paused { return "pause.circle" }
        switch share.linkRun?.phase {
        case .preparing, .running: return "arrow.triangle.2.circlepath"
        case .incomplete, .interrupted: return "exclamationmark.triangle"
        default: return "folder"
        }
    }

    private func openLinks() {
        model.selectedSection = .folders
        Task { await model.refreshFolders() }
    }

    private func openDevices() {
        model.selectedSection = .devices
        Task { await model.refreshDiscovery() }
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
