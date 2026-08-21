import SwiftUI

struct MacRootView: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var selectedSection: AppSection = .overview
    @State private var detailedAlert: AppAlert?
    @State private var pendingRecovery: (@MainActor () async -> Void)?

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                List(AppSection.allCases, selection: $selectedSection) { section in
                    Label(section.label, systemImage: section.systemImage)
                        .tag(section)
                        .primaryLabelStyle()
                        .accessibilityIdentifier("sidebar.\(section.rawValue)")
                }
                .listStyle(.sidebar)
                // The list vends its own accessibility element (an outline), so
                // a bare label lands on it. That is separate from the column
                // container named below: the audit tree shows the outline as a
                // grandchild of the column group, and both need a description.
                .accessibilityLabel("Covalent sidebar")

                serviceState
            }
            .navigationTitle("Covalent")
            .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 260)
            // Outermost point in the column the app can reach. The audit
            // reports an unnamed group at {{8, 39}, {220, 676}} that holds the
            // list and the service row, so name a container at that level and
            // keep both children individually reachable.
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
            // The matching unnamed group on the detail side is at
            // {{0, 31}, {1024, 692}} with the content scroll view as its only
            // child. A bare label is tried here rather than an explicit
            // container: the sidebar's `List` took `.accessibilityLabel` on its
            // own, so a container that SwiftUI has already collapsed may take
            // one too, and this reads better than nesting another group inside
            // one that is already a single-child wrapper.
            .accessibilityLabel(model.selectedSection.label)
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
                    .primaryLabelStyle()
                if let status = model.status {
                    // The sidebar draws its text vibrantly, and that blend is
                    // stroke-coverage dependent: on the 1x CI display a regular
                    // 10pt caption never renders darker than #7F7F7F, i.e. 4.0:1
                    // against the sidebar, which fails the system contrast audit
                    // even though the colour is `.primary`. The semibold status
                    // line directly above renders #272727 (15:1) in the same
                    // vibrancy context, so match its weight here.
                    //
                    // Weight alone was not enough: CI run 32461742319, on the
                    // commit that introduced it, still reported "Contrast
                    // failed for Service Ready". Weight changes stroke
                    // coverage, but the *inputs* to the blend were still a
                    // translucent tint over a vibrant material and a semantic
                    // colour AppKit is free to reinterpret. Both are now
                    // pinned: opaque text (`MacLabelColor`) over an opaque
                    // backdrop, so what the audit samples is what the app
                    // declared. The weight stays — it is what proved the
                    // vibrancy theory, and dropping it would re-open the
                    // 4.0:1 case if the backdrop ever goes translucent again.
                    Text(status.deviceName)
                        .font(.caption.weight(.semibold))
                        .primaryLabelStyle()
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background {
            // Opaque base first, tint second. `serviceColor.opacity(0.12)`
            // alone let the sidebar material show through, which is what made
            // the rendered contrast depend on the runner rather than on this
            // file.
            Color(nsColor: .windowBackgroundColor)
            serviceColor.opacity(0.12)
        }
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
            pausedText: "Paused. This resumable job stays on the local node.",
            checkpointText: "The local node checkpoints resumable progress."
        )
    }
}
