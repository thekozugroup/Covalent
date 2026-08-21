import SwiftUI

struct IOSRootView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

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
        ) { _ in
            Button("OK") { model.clearAlert() }
        } message: { alert in
            Text(alert.message)
        }
    }
}

private struct IOSActiveTaskBar: View {
    let task: ActiveTask
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        HStack(spacing: 12) {
            if task.state == .running {
                ProgressView()
            } else {
                Image(systemName: "pause.circle.fill")
                    .foregroundStyle(.orange)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("\(task.kind.label) \(task.title)")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Text(task.state == .paused ? "Paused on the node" : "Resumable checkpoint active")
                    .font(.caption)
                    .foregroundStyle(.secondary)
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
}
