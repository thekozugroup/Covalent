import AppKit
import SwiftUI

struct MacBackupsView: View {
    @ObservedObject var model: CovalentAppModel
    @State private var selectedSnapshotId: UUID?
    @State private var restoreSnapshot: SnapshotRecord?

    var body: some View {
        Group {
            if model.snapshots.isEmpty {
                MacEmptyState(
                    systemImage: "externaldrive.badge.plus",
                    title: "No backups yet",
                    message: "A backup appears here once your backup server finishes encrypting and saving it."
                ) {
                    Button("New Backup") { model.requestNewBackup() }
                        .buttonStyle(.borderedProminent)
                        .disabled(!model.isAuthorized)
                }
            } else {
                HSplitView {
                    snapshotList
                        .frame(minWidth: 280, idealWidth: 320, maxWidth: 390)
                    if let selectedSnapshot {
                        MacSnapshotDetail(model: model, snapshot: selectedSnapshot) {
                            restoreSnapshot = selectedSnapshot
                        }
                            .frame(minWidth: 480)
                    } else {
                        MacEmptyState(
                            systemImage: "externaldrive",
                            title: "Select a Backup",
                            message: "Choose a backup on the left to see what it holds."
                        )
                            .frame(minWidth: 480)
                    }
                }
            }
        }
        .navigationTitle("Backups")
        .background(Color(nsColor: .windowBackgroundColor))
        .onAppear {
            if selectedSnapshotId == nil {
                selectedSnapshotId = model.snapshots.first?.id
            }
        }
        .onChange(of: model.snapshots) { _, snapshots in
            if !snapshots.contains(where: { $0.id == selectedSnapshotId }) {
                selectedSnapshotId = snapshots.first?.id
            }
        }
        .onChange(of: model.restoreSetupRequest, initial: true) { _, request in
            guard let request,
                  let snapshot = model.snapshots.first(where: { $0.id == request.snapshotId })
            else { return }
            selectedSnapshotId = snapshot.id
            restoreSnapshot = snapshot
            model.restoreSetupRequest = nil
        }
        .sheet(item: $restoreSnapshot) { snapshot in
            MacRestoreSetupView(model: model, snapshot: snapshot)
        }
    }

    private var snapshotList: some View {
        List(selection: $selectedSnapshotId) {
            ForEach(groupedDates, id: \.0) { month, snapshots in
                Section(month) {
                    ForEach(snapshots) { snapshot in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(snapshot.displayName)
                                .font(.headline)
                                .lineLimit(1)
                            HStack {
                                Text(snapshot.createdAt.formatted(.dateTime.month(.abbreviated).day().hour().minute()))
                                Spacer()
                                Text(snapshot.bytesRead.formatted(.byteCount(style: .file)))
                            }
                            .font(.caption)
                            .secondaryLabelStyle()
                        }
                        .padding(.vertical, 5)
                        .tag(snapshot.id)
                        .accessibilityIdentifier("snapshot.\(snapshot.id.uuidString)")
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }

    private var selectedSnapshot: SnapshotRecord? {
        model.snapshots.first { $0.id == selectedSnapshotId }
    }

    private var groupedDates: [(String, [SnapshotRecord])] {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMM yyyy")
        let grouped = Dictionary(grouping: model.snapshots) { formatter.string(from: $0.createdAt) }
        return grouped.map { ($0.key, $0.value.sorted { $0.createdAt > $1.createdAt }) }
            .sorted { ($0.1.first?.createdAt ?? .distantPast) > ($1.1.first?.createdAt ?? .distantPast) }
    }
}

private struct MacSnapshotDetail: View {
    @ObservedObject var model: CovalentAppModel
    let snapshot: SnapshotRecord
    let onRestore: () -> Void
    @State private var showingRepairConfirmation = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 26) {
                header
                if snapshot.degradedFailures > 0 || snapshot.integrity == .corrupt {
                    attentionCallout
                }
                metrics
                replicaPlacement
                recoveryActions
                technicalDetails
            }
            .frame(maxWidth: 760, alignment: .leading)
            .padding(32)
        }
        .confirmationDialog(
            "Repair this backup?",
            isPresented: $showingRepairConfirmation,
            titleVisibility: .visible
        ) {
            Button("Verify and Repair") {
                Task { _ = await model.verify(snapshot, repair: true) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Covalent will use only intact copies on the storage devices you already selected and approved. It will not add another device.")
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 18) {
            Image(systemName: "externaldrive.fill")
                .scaledSymbolFont(size: 34)
                .foregroundStyle(MacLabelColor.accentGlyph)
                .scaledSymbolFrame(52)
                .background(Color.blue.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 5) {
                Text(snapshot.displayName)
                    .font(.largeTitle.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("Backup from \(snapshot.createdAt.formatted(date: .long, time: .shortened))")
                    .secondaryLabelStyle()
            }
            Spacer()
            Menu {
                Button("Verify") { Task { _ = await model.verify(snapshot) } }
                Button("Verify and Repair…") { showingRepairConfirmation = true }
            } label: {
                Label("More", systemImage: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .disabled(model.activeTask != nil)
        }
    }

    private var attentionCallout: some View {
        MacCallout(
            title: snapshot.integrity == .corrupt ? "Backup needs repair" : "Some selected devices were unavailable",
            message: snapshot.integrity == .corrupt
                ? "The check found missing or damaged data on this Mac. Repair can use intact copies from the storage devices you selected."
                : "The backup was saved on this Mac, but one or more devices you selected did not confirm receiving every part of it.",
            systemImage: "exclamationmark.triangle.fill",
            tint: .orange
        ) {
            Button("Verify") { Task { _ = await model.verify(snapshot) } }
                .disabled(model.activeTask != nil)
        }
    }

    private var metrics: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 145), spacing: 12)], spacing: 12) {
            detailMetric("Size", snapshot.bytesRead.formatted(.byteCount(style: .file)))
            detailMetric("Items", snapshot.entries.formatted())
            detailMetric("New chunks", snapshot.chunksStored.formatted())
            detailMetric("Deduplicated", snapshot.chunksDeduplicated.formatted())
        }
    }

    private func detailMetric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.caption).secondaryLabelStyle()
            Text(value).font(.title3.weight(.semibold)).monospacedDigit()
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }

    private var replicaPlacement: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Where copies are kept")
                .font(.title2.weight(.semibold))
            if snapshot.selectedProviderIds.isEmpty {
                Label("Local only", systemImage: "desktopcomputer")
                    .secondaryLabelStyle()
                Text("No extra storage device was selected for this backup.")
                    .font(.subheadline)
                    .secondaryLabelStyle()
            } else {
                ForEach(snapshot.selectedProviderIds, id: \.self) { providerId in
                    HStack {
                        Image(systemName: "server.rack")
                            .foregroundStyle(MacLabelColor.accentGlyph)
                        VStack(alignment: .leading) {
                            Text(providerName(providerId))
                            Text(providerId.uuidString)
                                .font(.caption.monospaced())
                                .secondaryLabelStyle()
                                .lineLimit(1)
                        }
                        Spacer()
                        if model.providers.contains(where: { $0.peerId == providerId }) {
                            Label("Connected", systemImage: "checkmark.circle.fill")
                                .font(.caption)
                                .foregroundStyle(.green)
                        } else {
                            Label("Offline", systemImage: "bolt.slash")
                                .font(.caption)
                                .foregroundStyle(.orange)
                        }
                    }
                    .padding(12)
                    .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 10))
                }
            }
            Button("Change Extra Copies for Next Backup…") {
                model.requestNewBackup(existingBackupId: snapshot.backupId)
            }
            .disabled(model.activeTask != nil)
            Text("Adding or removing a device changes only the next backup. Existing backups keep their original encrypted copies.")
                .font(.caption)
                .secondaryLabelStyle()
        }
    }

    private var recoveryActions: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Recovery")
                .font(.title2.weight(.semibold))
            Text("Preview every destination and conflict before Covalent writes anything.")
                .secondaryLabelStyle()
            HStack {
                Button("Preview Restore…") { onRestore() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .disabled(model.activeTask != nil)
                    .accessibilityIdentifier("snapshot.previewRestore")
                Button("Check Backup") { Task { _ = await model.verify(snapshot) } }
                    .controlSize(.large)
                    .disabled(model.activeTask != nil)
            }
        }
    }

    private var technicalDetails: some View {
        DisclosureGroup("Technical details") {
            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 8) {
                detailRow("Backup ID", snapshot.backupId.uuidString)
                detailRow("Backup version ID", snapshot.snapshotId)
                detailRow("Selected devices", snapshot.selectedProviderIds.count.formatted())
            }
            .font(.caption)
            .textSelection(.enabled)
            .padding(.top, 10)
        }
    }

    private func detailRow(_ label: String, _ value: String) -> some View {
        GridRow {
            Text(label).secondaryLabelStyle()
            Text(value).monospaced().lineLimit(1).truncationMode(.middle)
        }
    }

    private func providerName(_ id: UUID) -> String {
        model.providers.first(where: { $0.peerId == id })?.address ?? "Selected device"
    }
}

struct MacRestoreSetupView: View {
    @ObservedObject var model: CovalentAppModel
    let snapshot: SnapshotRecord
    @Environment(\.dismiss) private var dismiss
    @State private var destinationGrantId: UUID?
    @State private var isPreparing = false
    @State private var conflictPolicy: ConflictPolicy = .fail
    @State private var showReplacePreviewConfirmation = false

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Restore \(snapshot.displayName)")
                    .font(.title.weight(.semibold))
                Text("Choose an authorized destination. Covalent will create a signed, no-write preview first.")
                    .secondaryLabelStyle()
            }

            Form {
                Picker("Destination", selection: $destinationGrantId) {
                    Text("Choose a folder").tag(nil as UUID?)
                    ForEach(model.restoreGrants) { grant in
                        Text(grant.displayName).tag(grant.id as UUID?)
                    }
                }
                HStack {
                    Spacer()
                    Button("Choose New Folder…") { chooseFolder() }
                }
                Picker("If a file already exists", selection: $conflictPolicy) {
                    ForEach(ConflictPolicy.allCases) { policy in
                        Text(policy.label).tag(policy)
                    }
                }
                Text(conflictPolicy.safetyDetail)
                    .font(.caption)
                    .secondaryLabelStyle()
            }
            .formStyle(.grouped)

            Label(
                "Covalent lists this folder's contents before the preview and again right before writing. Any change stops the restore.",
                systemImage: "checkmark.shield"
            )
            .secondaryLabelStyle()
            .font(.subheadline)

            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Create Preview") { requestPreview() }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(destinationGrantId == nil || isPreparing)
            }
        }
        .padding(24)
        .frame(width: 520)
        .onAppear { destinationGrantId = model.restoreGrants.first?.id }
        .confirmationDialog(
            "Preview replacement of existing files?",
            isPresented: $showReplacePreviewConfirmation
        ) {
            Button("Preview Replacements", role: .destructive) { preparePreview() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The preview writes nothing. Continuing later can overwrite only destinations marked Replace in the signed plan.")
        }
    }

    private func requestPreview() {
        if conflictPolicy.isDestructive {
            showReplacePreviewConfirmation = true
        } else {
            preparePreview()
        }
    }

    private func preparePreview() {
        guard let destinationGrantId else { return }
        isPreparing = true
        Task {
            if await model.previewRestore(
                record: snapshot,
                destinationGrantId: destinationGrantId,
                conflictPolicy: conflictPolicy
            ) != nil {
                dismiss()
            }
            isPreparing = false
        }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.title = "Choose Restore Destination"
        panel.prompt = "Authorize Folder"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            if let grant = await model.addDirectoryGrant(url: url, purpose: .restoreDestination) {
                destinationGrantId = grant.id
            }
        }
    }
}

struct MacRestorePreviewView: View {
    @ObservedObject var model: CovalentAppModel
    let context: RestorePreviewContext
    @Environment(\.dismiss) private var dismiss
    @State private var isRestoring = false
    @State private var showReplaceExecutionConfirmation = false

    private var hasReplaceActions: Bool {
        context.plan.entries.contains { $0.action == .replaceFile }
    }

    private var hasSignedTargetInventory: Bool {
        context.plan.targetInventory != nil
    }

    private var canRestore: Bool {
        hasSignedTargetInventory && !isRestoring && model.activeTask == nil
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Restore Preview")
                            .font(.title.weight(.semibold))
                        Text("No files have been written. Your backup server signed the exact contents and actions this restore will perform.")
                            .secondaryLabelStyle()
                    }
                    Spacer()
                    Text("\(context.plan.entries.count) items")
                        .font(.headline.monospacedDigit())
                }
                .padding(24)
            }
            Divider()
            Table(context.plan.entries) {
                TableColumn("Source") { entry in
                    Text(entry.sourcePath).lineLimit(1).truncationMode(.middle)
                }
                TableColumn("Destination") { entry in
                    Text(entry.destinationPath).lineLimit(1).truncationMode(.middle)
                }
                TableColumn("Action") { entry in
                    Text(actionLabel(entry.action))
                }
                .width(min: 110, ideal: 130)
            }
            .frame(minHeight: 340)
            Divider()
            HStack {
                if !hasSignedTargetInventory {
                    Label("This preview has no signed destination inventory. Create a new preview before restoring.", systemImage: "exclamationmark.triangle.fill")
                        .font(.subheadline)
                        .foregroundStyle(.orange)
                } else {
                    Label("Restore is confined to \(context.destinationDisplayName). Any folder change stops it.", systemImage: "checkmark.shield")
                        .font(.subheadline)
                        .secondaryLabelStyle()
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                Button("Cancel") {
                    model.dismissRestorePreview()
                    dismiss()
                }
                Button("Restore") { requestRestore() }
                .buttonStyle(.borderedProminent)
                .disabled(!canRestore)
                .accessibilityIdentifier("restore.execute")
            }
            .padding(18)
        }
        .frame(minWidth: 780, idealWidth: 900, minHeight: 520, idealHeight: 620)
        .confirmationDialog(
            "Replace existing files?",
            isPresented: $showReplaceExecutionConfirmation,
            titleVisibility: .visible
        ) {
            Button("Replace and Restore", role: .destructive) { runRestore() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Only files marked Replace in this signed plan can be overwritten. Covalent checks the signed destination inventory again and stops if it changed.")
        }
    }

    private func requestRestore() {
        if hasReplaceActions {
            showReplaceExecutionConfirmation = true
        } else {
            runRestore()
        }
    }

    private func runRestore() {
        isRestoring = true
        Task {
            defer { isRestoring = false }
            if await model.executeRestore() != nil {
                dismiss()
            }
        }
    }

    private func actionLabel(_ action: RestoreAction) -> String {
        switch action {
        case .createFile: "Create file"
        case .createDirectory: "Create folder"
        case .keepDirectory: "Keep folder"
        case .skipFile: "Skip existing file"
        case .replaceFile: "Replace existing file"
        case .renameFile: "Create renamed copy"
        }
    }
}

struct MacRestoreResultView: View {
    let result: RestoreResponse
    let done: () -> Void

    var body: some View {
        VStack(spacing: 22) {
            Image(systemName: "checkmark.circle.fill")
                .scaledSymbolFont(size: 54)
                .foregroundStyle(.green)
                .accessibilityHidden(true)
            VStack(spacing: 6) {
                Text("Restore Complete")
                    .font(.title.weight(.semibold))
                Text("Covalent restored \(result.filesRestored) files and wrote \(result.bytesWritten.formatted(.byteCount(style: .file))).")
                    .secondaryLabelStyle()
                    .multilineTextAlignment(.center)
            }
            HStack(spacing: 22) {
                resultMetric("Files", result.filesRestored)
                resultMetric("Folders", result.directoriesCreated)
                resultMetric("Skipped", result.filesSkipped)
            }
            if result.rejectedProviderCopies > 0 {
                Label("\(result.rejectedProviderCopies) invalid copies from storage devices were rejected.", systemImage: "checkmark.shield")
                    .foregroundStyle(.orange)
            }
            Button("Done", action: done)
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
        }
        .padding(32)
        .frame(width: 480)
    }

    private func resultMetric(_ label: String, _ value: Int) -> some View {
        VStack(spacing: 3) {
            Text(value.formatted()).font(.title2.weight(.semibold)).monospacedDigit()
            Text(label).font(.caption).secondaryLabelStyle()
        }
        .frame(minWidth: 80)
        .accessibilityElement(children: .combine)
    }
}
