import SwiftUI

struct MacRootView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var selectedSection: AppSection = .folders
    @State private var adoptedInitialSection = false
    @State private var detailedAlert: AppAlert?
    @State private var pendingRecovery: (@MainActor () async -> Void)?

    var body: some View {
        NavigationSplitView {
            List(selection: $selectedSection) {
                Section {
                    ForEach([AppSection.folders, .devices, .overview]) { section in
                        sidebarLabel(for: section)
                    }
                }
                Text("Legacy and Advanced")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(MacLabelColor.secondary)
                    .accessibilityAddTraits(.isHeader)
                ForEach([AppSection.backups, .settings]) { section in
                    sidebarLabel(for: section)
                }
                Text("Local Service")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(MacLabelColor.secondary)
                    .accessibilityAddTraits(.isHeader)
                serviceState
            }
            .listStyle(.sidebar)
            .accessibilityLabel("Covalent sidebar")
            .navigationTitle("Covalent")
            .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 260)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Sections and service status")
        } detail: {
            ZStack(alignment: .bottom) {
                detail
                if let activeTask = model.activeTask {
                    MacActiveTaskBar(task: activeTask, model: model)
                        .padding(20)
                        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: model.activeTask)
            .accessibilityLabel(macLabel(for: model.selectedSection))
        }
        .toolbar(removing: .title)
        .toolbar {
            if #available(macOS 26.0, *) {
                sectionTitle.sharedBackgroundVisibility(.hidden)
            } else {
                sectionTitle
            }

            ToolbarItemGroup {
                Button {
                    model.selectedSection = .folders
                    Task { await model.refreshFolders() }
                } label: {
                    Label("Create Link", systemImage: "folder.badge.plus")
                }
                .disabled(!model.isAuthorized)
                .accessibilityIdentifier("toolbar.createLink")

                Button {
                    model.selectedSection = .devices
                    Task { await model.refreshDiscovery() }
                } label: {
                    Label("Pair Device", systemImage: "laptopcomputer.and.iphone")
                }
                .disabled(!model.isAuthorized)

                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .help("Refresh local service status")

            }
        }
        .onAppear {
            if !adoptedInitialSection, model.selectedSection == .overview {
                model.selectedSection = .folders
            }
            adoptedInitialSection = true
            selectedSection = model.selectedSection
        }
        .onChange(of: selectedSection) { _, section in
            if model.selectedSection != section {
                model.selectedSection = section
            }
        }
        .onChange(of: model.selectedSection) { _, section in
            if selectedSection != section {
                selectedSection = section
            }
        }
        .task { await model.start() }
        .task { await model.pollNetworkPairings() }
        .sheet(item: $model.presentation) { presentation in
            switch presentation {
            case .connection:
                MacConnectionView(model: model)
            case .newBackup:
                MacLegacyBackupsView(model: model)
            case .pairDevice:
                MacPairingView(model: model)
            case .networkPairing:
                if let pairing = model.activeNetworkPairing {
                    MacNetworkPairingView(model: model, pairing: pairing)
                }
            case .importSettings:
                MacSettingsImportView(model: model)
            case .firstLaunchSetup:
                MacFirstLaunchRecoveryView(model: model)
            }
        }
        .sheet(
            isPresented: Binding(
                get: { model.restorePreview != nil },
                set: { if !$0 { model.dismissRestorePreview() } }
            )
        ) {
            if let context = model.restorePreview {
                MacRestorePreviewView(model: model, context: context)
            }
        }
        .sheet(
            isPresented: Binding(
                get: { model.lastRestoreResult != nil },
                set: { if !$0 { model.clearRestoreResult() } }
            )
        ) {
            if let result = model.lastRestoreResult {
                MacRestoreResultView(result: result) {
                    model.clearRestoreResult()
                }
            }
        }
        .alert(
            model.alert?.title ?? "Covalent",
            isPresented: Binding(
                get: { model.alert != nil },
                set: { if !$0 { model.clearAlert() } }
            ),
            presenting: model.alert
        ) { alert in
            if let recoveryTitle = alert.recoveryActionTitle {
                Button(recoveryTitle) {
                    if alert.recoveryOpensSystemSettings {
                        openNetworkSettings()
                        model.clearAlert()
                    } else if let recovery = model.takeAlertRecovery() {
                        // Taken synchronously: SwiftUI clears the alert as soon
                        // as this button is clicked, so reading it inside the
                        // Task below would find it already gone.
                        Task { await recovery() }
                    }
                }
                .keyboardShortcut(.defaultAction)
            }
            if alert.detail != nil {
                Button("Details…") {
                    // Carry the recovery across, so reading the technical text
                    // does not cost the user their way out.
                    pendingRecovery = model.takeAlertRecovery()
                    detailedAlert = alert
                }
            }
            Button("OK", role: .cancel) { model.clearAlert() }
                .keyboardShortcut(.cancelAction)
        } message: { alert in
            Text(alert.message)
        }
        // The technical text never leads. It is one deliberate click away, for
        // the person who is going to paste it into a bug report.
        .alert(
            "Technical details",
            isPresented: Binding(
                get: { detailedAlert != nil },
                set: { if !$0 { dismissDetails() } }
            ),
            presenting: detailedAlert
        ) { alert in
            if let recoveryTitle = alert.recoveryActionTitle, pendingRecovery != nil {
                Button(recoveryTitle) {
                    let recovery = pendingRecovery
                    dismissDetails()
                    if let recovery { Task { await recovery() } }
                }
                .keyboardShortcut(.defaultAction)
            }
            Button("Done", role: .cancel) { dismissDetails() }
                .keyboardShortcut(.cancelAction)
        } message: { alert in
            Text(alert.detail ?? "")
        }
    }

    private var sectionTitle: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            Text(macLabel(for: model.selectedSection))
                .font(.title2.weight(.semibold))
                .foregroundStyle(MacLabelColor.sidebarUnselected)
        }
    }

    private func sidebarLabel(for section: AppSection) -> some View {
        HStack(spacing: 8) {
            Image(systemName: section.systemImage)
                .symbolRenderingMode(.monochrome)
                .font(.body.weight(.semibold))
                .foregroundStyle(
                    section == selectedSection ? Color.white : MacLabelColor.accentGlyph
                )
                .frame(width: 20)
                .accessibilityHidden(true)
            Text(macLabel(for: section))
                .font(.body.weight(.semibold))
                .foregroundStyle(
                    section == selectedSection ? Color.white : MacLabelColor.sidebarUnselected
                )
                .accessibilityIdentifier("sidebar.\(section.rawValue)")
        }
        .tag(section)
    }

    private func macLabel(for section: AppSection) -> String {
        switch section {
        case .overview: "Status"
        case .backups: "Legacy Backups"
        case .folders: "Links"
        case .devices: "Devices"
        case .settings: "Advanced Settings"
        }
    }

    private func dismissDetails() {
        detailedAlert = nil
        pendingRecovery = nil
    }

    private func openNetworkSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.Network-Settings.extension") else { return }
        NSWorkspace.shared.open(url)
    }

    @ViewBuilder
    private var detail: some View {
        switch model.selectedSection {
        case .overview:
            MacOverviewView(model: model)
        case .backups:
            MacLegacyBackupsView(model: model)
        case .folders:
            MacFoldersView(model: model)
        case .devices:
            MacDevicesView(model: model)
        case .settings:
            MacSettingsView(model: model)
        }
    }

    private var serviceState: some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(model.serviceStatusLabel)
                if let status = model.status {
                    Text(status.deviceName)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        } icon: {
            Image(systemName: serviceSymbol)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Local service, \(model.serviceStatusLabel)")
    }

    private var serviceSymbol: String {
        switch model.phase {
        case .starting: "arrow.triangle.2.circlepath"
        case .ready: "checkmark.circle"
        case .needsAuthorization: "questionmark.circle"
        case .offline: "exclamationmark.triangle"
        }
    }
}

struct MacActiveTaskBar: View {
    let task: ActiveTask
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        HStack(spacing: 14) {
            if fractionCompleted == nil {
                ProgressView()
                    .controlSize(.small)
            }
            VStack(alignment: .leading, spacing: 4) {
                Text("\(task.kind.label) \(task.title)")
                    .font(.subheadline.weight(.semibold))
                // A real fraction whenever the bytes are known; the spinner
                // beside it stands in only while they genuinely are not.
                if let fractionCompleted {
                    ProgressView(value: fractionCompleted)
                        .progressViewStyle(.linear)
                        .frame(width: 260)
                }
                Text(statusDetail)
                    .font(.caption.monospacedDigit())
                    .secondaryLabelStyle()
            }
            Spacer()
            if task.jobId != nil {
                if task.state == .paused {
                    Button("Resume") { Task { await model.controlActiveTask(.resume) } }
                } else {
                    Button("Pause") { Task { await model.controlActiveTask(.pause) } }
                }
                Button("Cancel", role: .destructive) {
                    Task { await model.controlActiveTask(.cancel) }
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .stroke(Color.primary.opacity(0.1))
        }
        .shadow(color: .black.opacity(0.08), radius: 12, y: 4)
        .frame(maxWidth: 720)
        .accessibilityElement(children: .contain)
    }

    private var fractionCompleted: Double? {
        task.state == .running ? task.progress?.fractionCompleted : nil
    }

    private var statusDetail: String {
        task.statusDetail(
            pausedText: "Paused. Your backup server is holding this job, so it can carry on later.",
            checkpointText: "Your backup server is saving progress as it goes."
        )
    }
}
