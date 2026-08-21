import SwiftUI

struct MacRootView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var selectedSection: AppSection = .overview
    @State private var detailedAlert: AppAlert?

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                List(AppSection.allCases, selection: $selectedSection) { section in
                    Label(section.label, systemImage: section.systemImage)
                        .tag(section)
                        .foregroundStyle(.primary)
                        .accessibilityIdentifier("sidebar.\(section.rawValue)")
                }
                .listStyle(.sidebar)

                serviceState
            }
            .navigationTitle("Covalent")
            .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 260)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Covalent sidebar")
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
        }
        .toolbar {
            ToolbarItemGroup {
                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .help("Refresh local service status")

                Button {
                    model.requestNewBackup()
                } label: {
                    Label("New Backup", systemImage: "plus")
                }
                .buttonStyle(.borderedProminent)
                .disabled(!model.isAuthorized || model.activeTask != nil)
                .help(model.isAuthorized ? "Create a backup" : "Connect to the local service first")
                .accessibilityIdentifier("toolbar.newBackup")
            }
        }
        .onAppear { selectedSection = model.selectedSection }
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
                MacNewBackupView(model: model)
            case .pairDevice:
                MacPairingView(model: model)
            case .networkPairing:
                if let pairing = model.activeNetworkPairing {
                    MacNetworkPairingView(model: model, pairing: pairing)
                }
            case .importSettings:
                MacSettingsImportView(model: model)
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
                    } else {
                        Task { await model.performAlertRecovery() }
                    }
                }
                .keyboardShortcut(.defaultAction)
            }
            if alert.detail != nil {
                Button("Details…") { detailedAlert = alert }
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
                set: { if !$0 { detailedAlert = nil } }
            ),
            presenting: detailedAlert
        ) { _ in
            Button("Done", role: .cancel) { detailedAlert = nil }
                .keyboardShortcut(.cancelAction)
        } message: { alert in
            Text(alert.detail ?? "")
        }
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
            MacBackupsView(model: model)
        case .devices:
            MacDevicesView(model: model)
        case .settings:
            MacSettingsView(model: model)
        }
    }

    private var serviceState: some View {
        HStack(spacing: 9) {
            Circle()
                .fill(serviceColor)
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(model.serviceStatusLabel)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                if let status = model.status {
                    // The sidebar draws its text vibrantly, and that blend is
                    // stroke-coverage dependent: on the 1x CI display a regular
                    // 10pt caption never renders darker than #7F7F7F, i.e. 4.0:1
                    // against the sidebar, which fails the system contrast audit
                    // even though the colour is `.primary`. The semibold status
                    // line directly above renders #272727 (15:1) in the same
                    // vibrancy context, so match its weight here.
                    Text(status.deviceName)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(serviceColor.opacity(0.12))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Service \(model.serviceStatusLabel)")
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
                    .foregroundStyle(.secondary)
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
            pausedText: "Paused. This resumable job stays on the local node.",
            checkpointText: "The local node checkpoints resumable progress."
        )
    }
}
