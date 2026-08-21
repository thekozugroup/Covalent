import SwiftUI

struct IOSRootView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var detailedAlert: AppAlert?
    @State private var pendingRecovery: (@MainActor () async -> Void)?

    var body: some View {
        TabView(selection: $model.selectedSection) {
            NavigationStack {
                IOSHomeView(model: model)
            }
            .tabItem { Label("Home", systemImage: "house") }
            .tag(AppSection.overview)

            NavigationStack {
                IOSBackupsView(model: model)
            }
            .tabItem { Label("Backups", systemImage: "externaldrive") }
            .tag(AppSection.backups)

            NavigationStack {
                IOSDevicesView(model: model)
            }
            .tabItem { Label("Devices", systemImage: "laptopcomputer.and.iphone") }
            .tag(AppSection.devices)

            NavigationStack {
                IOSSettingsView(model: model)
            }
            .tabItem { Label("Settings", systemImage: "gearshape") }
            .tag(AppSection.settings)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let task = model.activeTask {
                IOSActiveTaskBar(task: task, model: model)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: model.activeTask)
        .task { await model.start() }
        .task { await model.pollNetworkPairings() }
        .sheet(item: $model.presentation) { presentation in
            switch presentation {
            case .connection:
                IOSConnectionView(model: model)
            case .newBackup:
                IOSNewBackupView(model: model)
            case .pairDevice:
                IOSPairingView(model: model)
            case .networkPairing:
                if let pairing = model.activeNetworkPairing {
                    IOSNetworkPairingView(model: model, pairing: pairing)
                }
            case .importSettings:
                IOSSettingsImportView(model: model)
            }
        }
        .sheet(item: $model.restoreSetupRequest) { request in
            if let snapshot = model.snapshots.first(where: { $0.id == request.snapshotId }) {
                IOSRestoreSetupView(model: model, snapshot: snapshot)
            } else {
                ContentUnavailableView("Backup unavailable", systemImage: "externaldrive.badge.xmark")
                    .presentationDetents([.medium])
            }
        }
        .sheet(
            isPresented: Binding(
                get: { model.restorePreview != nil },
                set: { if !$0 { model.dismissRestorePreview() } }
            )
        ) {
            if let context = model.restorePreview {
                IOSRestorePreviewView(model: model, context: context)
            }
        }
        .sheet(
            isPresented: Binding(
                get: { model.lastRestoreResult != nil },
                set: { if !$0 { model.clearRestoreResult() } }
            )
        ) {
            if let result = model.lastRestoreResult {
                IOSRestoreResultView(result: result) { model.clearRestoreResult() }
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
                        openSystemSettings()
                        model.clearAlert()
                    } else if let recovery = model.takeAlertRecovery() {
                        // Taken synchronously: SwiftUI clears the alert as soon
                        // as this button is tapped, so reading it inside the
                        // Task below would find it already gone.
                        Task { await recovery() }
                    }
                }
            }
            if alert.detail != nil {
                Button("Details") {
                    // Carry the recovery across, so reading the technical text
                    // does not cost the user their way out.
                    pendingRecovery = model.takeAlertRecovery()
                    detailedAlert = alert
                }
            }
            Button("OK", role: .cancel) { model.clearAlert() }
        } message: { alert in
            Text(alert.message)
        }
        // The technical text never leads. It is one deliberate tap away, for
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
            }
            Button("Done", role: .cancel) { dismissDetails() }
        } message: { alert in
            Text(alert.detail ?? "")
        }
    }

    private func dismissDetails() {
        detailedAlert = nil
        pendingRecovery = nil
    }

    private func openSystemSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
}

private struct IOSActiveTaskBar: View {
    let task: ActiveTask
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        HStack(spacing: 12) {
            if task.state != .running {
                Image(systemName: "pause.circle.fill")
                    .foregroundStyle(.orange)
            } else if fractionCompleted == nil {
                ProgressView()
            }
            VStack(alignment: .leading, spacing: 4) {
                Text("\(task.kind.label) \(task.title)")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                // A real fraction whenever the bytes are known; the spinner
                // above stands in only while they genuinely are not.
                if let fractionCompleted {
                    ProgressView(value: fractionCompleted)
                        .progressViewStyle(.linear)
                }
                Text(statusDetail)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if task.jobId != nil {
                Button(task.state == .paused ? "Resume" : "Pause") {
                    Task { await model.controlActiveTask(task.state == .paused ? .resume : .pause) }
                }
                .buttonStyle(.bordered)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(Color(uiColor: .secondarySystemBackground))
        .overlay(alignment: .top) { Divider() }
        .accessibilityElement(children: .contain)
    }

    private var fractionCompleted: Double? {
        task.state == .running ? task.progress?.fractionCompleted : nil
    }

    private var statusDetail: String {
        task.statusDetail(
            pausedText: "Paused on the node",
            checkpointText: "Resumable checkpoint active"
        )
    }
}
