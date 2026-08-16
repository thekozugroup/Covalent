import SwiftUI

struct MacRootView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        NavigationSplitView {
            List(AppSection.allCases, selection: $model.selectedSection) { section in
                Label(section.label, systemImage: section.systemImage)
                    .tag(section)
                    .accessibilityIdentifier("sidebar.\(section.rawValue)")
            }
            .navigationTitle("Covalent")
            .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 260)
            .safeAreaInset(edge: .bottom) {
                serviceState
            }
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
        .task { await model.start() }
        .sheet(item: $model.presentation) { presentation in
            switch presentation {
            case .connection:
                MacConnectionView(model: model)
            case .newBackup:
                MacNewBackupView(model: model)
            case .pairDevice:
                MacPairingView(model: model)
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
        ) { _ in
            Button("OK") { model.clearAlert() }
        } message: { alert in
            Text(alert.message)
        }
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
                    .font(.caption.weight(.medium))
                if let status = model.status {
                    Text(status.deviceName)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(Color(nsColor: .controlBackgroundColor))
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
            ProgressView()
                .controlSize(.small)
            VStack(alignment: .leading, spacing: 2) {
                Text("\(task.kind.label) \(task.title)")
                    .font(.subheadline.weight(.semibold))
                Text(task.state == .paused ? "Paused. This resumable job stays on the local node." : "The local node checkpoints resumable progress.")
                    .font(.caption)
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
}
