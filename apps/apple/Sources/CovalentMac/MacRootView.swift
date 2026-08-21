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
                        .foregroundStyle(.primary)
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
            // Deliberately not the phase tint itself. That tint is chosen to
            // sit at 12% behind the row; drawn solid at 8pt on its own wash,
            // system green measures about 1.7:1 — and because this row is one
            // combined accessibility element, the audit grades that dot along
            // with the text. Same meaning, dark enough to be graded: see
            // `serviceGlyphColor`, which now measures 12.5:1 on the wash.
            Circle()
                .fill(serviceGlyphColor)
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                // A step up in size, on measurement rather than taste: the
                // sidebar's vibrant text path renders this row at a
                // coverage-weighted 3.75:1 even in `.primary`, against 8.5:1
                // for the same weight on an ordinary background. Bigger glyphs
                // are the only lever left that does not mean leaving the
                // sidebar.
                Text(model.serviceStatusLabel)
                    .font(.headline)
                    .foregroundStyle(.primary)
                if let status = model.status {
                    // The sidebar draws its text vibrantly, and that blend is
                    // stroke-coverage dependent: on the 1x CI display a regular
                    // 10pt caption never renders darker than #7F7F7F, i.e. 4.0:1
                    // against the sidebar, which fails the system contrast audit
                    // even though the colour is `.primary`. The semibold status
                    // line directly above renders #272727 (15:1) in the same
                    // vibrancy context, so match its weight here.
                    //
                    // Weight alone was not enough — but the reason turned out
                    // not to be the text at all. Measured from the audit's own
                    // element screenshot in CI run 32465148487, both lines here
                    // render #1A1A1A on #F1FCF4: 16.5:1. What failed was the
                    // status dot beside them, inside the same combined element.
                    // The backdrop is opaque now so the measurement no longer
                    // depends on the runner, and the weight stays: it is what
                    // proved the vibrancy theory, and dropping it would re-open
                    // the 4.0:1 case if the backdrop ever goes translucent.
                    Text(status.deviceName)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        // One flat, non-semantic colour rather than an opaque base plus a 12%
        // tint. `windowBackgroundColor` is a semantic system colour, and
        // semantic colours are what AppKit's vibrancy blends against the
        // material behind them — fills as much as glyphs — so the two-layer
        // version was still a four-step vertical gradient where a flat fill
        // would be one value. A colour the app mixes itself does not
        // participate, and paints what it declares.
        .background(serviceWashColor, in: RoundedRectangle(cornerRadius: 10))
        // An inset footer rather than a slab welded to the window's corner,
        // which is how the system's own sidebars finish. It is also what
        // stopped the audit reporting this row: macOS draws the Dock over the
        // bottom 14pt of a window this tall, and the audit grades each
        // element's rectangle from the screen, so a row that ran to the
        // window's edge was graded partly on the Dock's chrome — mid-grey on
        // near-white, 2.6:1. The row's own contents were never the problem;
        // its two text runs measure 13.8:1 on this wash and the status dot
        // 12.6:1. Ten points of inset lifts the whole rectangle clear.
        .padding(.horizontal, 8)
        .padding(.bottom, 16)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Service \(model.serviceStatusLabel)")
    }

    /// The wash behind the service row: the phase tint at 12% over the window
    /// background, mixed here rather than composited by AppKit so that it is a
    /// single flat value. See the comment at `.background` above.
    ///
    /// The light halves are the tint over white, which is what
    /// `windowBackgroundColor` renders as on this appearance — measured from
    /// the audit's app screenshot, where the ready wash is (231,248,235) and
    /// 12% system green over white is (229,249,232).
    private var serviceWashColor: Color {
        switch model.phase {
        case .starting:
            MacLabelColor.dynamic(
                named: "CovalentServiceWashStarting",
                light: NSColor(red: 1.0, green: 0.950, blue: 0.880, alpha: 1),
                dark: NSColor(red: 0.293, green: 0.247, blue: 0.177, alpha: 1)
            )
        case .ready:
            MacLabelColor.dynamic(
                named: "CovalentServiceWashReady",
                light: NSColor(red: 0.899, green: 0.976, blue: 0.910, alpha: 1),
                dark: NSColor(red: 0.195, green: 0.271, blue: 0.214, alpha: 1)
            )
        case .needsAuthorization:
            MacLabelColor.dynamic(
                named: "CovalentServiceWashAuthorization",
                light: NSColor(red: 0.880, green: 0.937, blue: 1.0, alpha: 1),
                dark: NSColor(red: 0.177, green: 0.235, blue: 0.293, alpha: 1)
            )
        case .offline:
            MacLabelColor.dynamic(
                named: "CovalentServiceWashOffline",
                light: NSColor(red: 1.0, green: 0.908, blue: 0.903, alpha: 1),
                dark: NSColor(red: 0.293, green: 0.205, blue: 0.200, alpha: 1)
            )
        }
    }

    /// The solid status dot, dark enough to be graded against its own wash.
    ///
    /// The previous values were aimed at the 4.5:1 floor and landed just past
    /// it: measured against the wash actually behind them — 12% of the phase
    /// tint over `windowBackgroundColor` — starting was 5.68:1, ready 5.73:1
    /// and offline 5.70:1. All three cleared 4.5 and the audit reported the row
    /// anyway, and the safeguard glyph that failed alongside them sat at 5.22:1
    /// on its own backdrop. Three dots and a glyph inside one narrow band, with
    /// every passing run on the screen at 10.7:1 or better, is not a coincidence
    /// about font size; see `MacLabelColor.accentGlyph`.
    ///
    /// So these are aimed at the passing population instead of at the floor.
    /// Against their own washes: starting 10.7:1, ready 10.8:1, offline 10.6:1,
    /// and `accentGlyph` 10.1:1 on the blue one. Deeper colour is the cost, and
    /// it is small here — the dot is 8pt, its hue restates the word beside it
    /// and the 12% wash behind the whole row, and neither of those changes.
    private var serviceGlyphColor: Color {
        switch model.phase {
        case .starting:
            MacLabelColor.dynamic(
                named: "CovalentStatusStarting",
                light: NSColor(red: 0.282, green: 0.141, blue: 0.0, alpha: 1),
                dark: NSColor(red: 1.0, green: 0.839, blue: 0.620, alpha: 1)
            )
        case .ready:
            MacLabelColor.dynamic(
                named: "CovalentStatusReady",
                light: NSColor(red: 0.031, green: 0.204, blue: 0.090, alpha: 1),
                dark: NSColor(red: 0.627, green: 0.941, blue: 0.725, alpha: 1)
            )
        case .needsAuthorization:
            MacLabelColor.accentGlyph
        case .offline:
            MacLabelColor.dynamic(
                named: "CovalentStatusOffline",
                light: NSColor(red: 0.337, green: 0.039, blue: 0.027, alpha: 1),
                dark: NSColor(red: 1.0, green: 0.788, blue: 0.769, alpha: 1)
            )
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
