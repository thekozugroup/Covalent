import SwiftUI
import UniformTypeIdentifiers

struct IOSBackupsView: View {
    @ObservedObject var model: CovalentAppModel

    var body: some View {
        Group {
            if model.snapshots.isEmpty {
                ContentUnavailableView {
                    Label("No snapshots yet", systemImage: "externaldrive.badge.plus")
                } description: {
                    Text("A backup appears here after the node commits its encrypted snapshot.")
                } actions: {
                    Button("New Backup") { model.requestNewBackup() }
                        .buttonStyle(.borderedProminent)
                        .disabled(!model.isAuthorized)
                }
            } else {
                List {
                    ForEach(model.snapshots) { snapshot in
                        NavigationLink {
                            IOSBackupDetailView(model: model, snapshotId: snapshot.id)
                        } label: {
                            IOSSnapshotRow(snapshot: snapshot)
                        }
                        .accessibilityIdentifier("snapshot.\(snapshot.id.uuidString)")
                    }
                }
            }
        }
        .navigationTitle("Backups")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    model.requestNewBackup()
                } label: {
                    Label("New Backup", systemImage: "plus")
                }
                .disabled(!model.isAuthorized || model.activeTask != nil)
                .accessibilityIdentifier("backups.new")
            }
        }
    }
}

private struct IOSSnapshotRow: View {
    let snapshot: SnapshotRecord

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: integritySymbol)
                .foregroundStyle(integrityColor)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(snapshot.displayName).font(.headline)
                Text(snapshot.createdAt.formatted(date: .abbreviated, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 4) {
                Text(snapshot.bytesRead.formatted(.byteCount(style: .file)))
                    .font(.subheadline.monospacedDigit())
                Text(snapshot.integrity.label)
                    .font(.caption)
                    .foregroundStyle(integrityColor)
            }
        }
        .padding(.vertical, 5)
        .accessibilityElement(children: .combine)
    }

    private var integritySymbol: String {
        switch snapshot.integrity {
        case .unknown: "externaldrive"
        case .checking: "arrow.trianglehead.2.clockwise.rotate.90"
        case .intact: "checkmark.shield.fill"
        case .degraded: "exclamationmark.triangle.fill"
        case .corrupt: "xmark.octagon.fill"
        }
    }

    private var integrityColor: Color {
        switch snapshot.integrity {
        case .unknown: .secondary
        case .checking: .blue
        case .intact: .green
        case .degraded: .orange
        case .corrupt: .red
        }
    }
}

private struct IOSBackupDetailView: View {
    @ObservedObject var model: CovalentAppModel
    let snapshotId: UUID
    @State private var showingRepairConfirmation = false

    var body: some View {
        Group {
            if let snapshot {
                List {
                    Section {
                        LabeledContent("Created", value: snapshot.createdAt.formatted(date: .long, time: .shortened))
                        LabeledContent("Size", value: snapshot.bytesRead.formatted(.byteCount(style: .file)))
                        LabeledContent("Items", value: snapshot.entries.formatted())
                        LabeledContent("New chunks", value: snapshot.chunksStored.formatted())
                        LabeledContent("Deduplicated", value: snapshot.chunksDeduplicated.formatted())
                    }

                    Section("Replica placement") {
                        if snapshot.selectedProviderIds.isEmpty {
                            Label("Local only", systemImage: "iphone")
                            Text("No extra storage provider was selected for this snapshot.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        } else {
                            ForEach(snapshot.selectedProviderIds, id: \.self) { providerId in
                                Label(providerName(providerId), systemImage: "server.rack")
                            }
                        }
                    }

                    if snapshot.degradedFailures > 0 || snapshot.integrity == .corrupt {
                        Section {
                            Label(
                                snapshot.integrity == .corrupt ? "Snapshot needs repair" : "A selected provider was unavailable",
                                systemImage: "exclamationmark.triangle.fill"
                            )
                            .foregroundStyle(.orange)
                            Text("Repair uses only intact copies from explicitly selected, authorized providers.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }

                    Section("Recovery") {
                        Button {
                            model.restoreSetupRequest = RestoreSetupRequest(snapshotId: snapshot.id)
                        } label: {
                            Label("Preview Restore", systemImage: "doc.text.magnifyingglass")
                        }
                        .disabled(model.activeTask != nil)

                        Button {
                            verify(snapshot)
                        } label: {
                            Label("Verify Snapshot", systemImage: "checkmark.shield")
                        }
                        .disabled(model.activeTask != nil)

                        Button("Verify and Repair…") { showingRepairConfirmation = true }
                            .disabled(model.activeTask != nil)
                    }

                    Section("Technical details") {
                        LabeledContent("Backup ID", value: snapshot.backupId.uuidString)
                        LabeledContent("Snapshot ID", value: snapshot.snapshotId)
                    }
                    .font(.caption)
                }
                .navigationTitle(snapshot.displayName)
                .navigationBarTitleDisplayMode(.inline)
                .confirmationDialog("Repair this snapshot?", isPresented: $showingRepairConfirmation) {
                    Button("Verify and Repair") { verify(snapshot, repair: true) }
                    Button("Cancel", role: .cancel) {}
                } message: {
                    Text("Covalent will use only intact copies from providers selected for this snapshot.")
                }
            } else {
                ContentUnavailableView("Backup unavailable", systemImage: "externaldrive.badge.xmark")
            }
        }
    }

    private var snapshot: SnapshotRecord? {
        model.snapshots.first { $0.id == snapshotId }
    }

    private func providerName(_ id: UUID) -> String {
        model.providers.first(where: { $0.peerId == id })?.address ?? "Offline selected provider"
    }

    private func verify(_ snapshot: SnapshotRecord, repair: Bool = false) {
        Task {
            _ = await IOSBackgroundExecution.perform(named: repair ? "Covalent repair" : "Covalent verify") {
                await model.verify(snapshot, repair: repair)
            }
        }
    }
}

struct IOSRestoreSetupView: View {
    @ObservedObject var model: CovalentAppModel
    let snapshot: SnapshotRecord
    @Environment(\.dismiss) private var dismiss
    @State private var destinationGrantId: UUID?
    @State private var isChoosingFolder = false
    @State private var isPreparing = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Picker("Destination", selection: $destinationGrantId) {
                        Text("Choose a folder").tag(nil as UUID?)
                        ForEach(model.restoreGrants) { grant in
                            Text(grant.displayName).tag(grant.id as UUID?)
                        }
                    }
                    Button("Choose New Folder…") { isChoosingFolder = true }
                } header: {
                    Text("Authorized destination")
                } footer: {
                    Text("The node creates a signed, no-write preview before any restore begins.")
                }

                Section {
                    Label("Choose an empty folder", systemImage: "folder.badge.plus")
                    Text("An empty destination keeps every streamed write identical to the node's signed preview.")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Restore \(snapshot.displayName)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Preview") { preparePreview() }
                        .disabled(destinationGrantId == nil || isPreparing)
                }
            }
            .overlay {
                if isPreparing { ProgressView("Signing no-write preview…").padding().background(.background, in: RoundedRectangle(cornerRadius: 12)) }
            }
            .fileImporter(isPresented: $isChoosingFolder, allowedContentTypes: [.folder]) { result in
                do {
                    let url = try result.get()
                    Task {
                        if let grant = await model.addDirectoryGrant(url: url, purpose: .restoreDestination) {
                            destinationGrantId = grant.id
                        }
                    }
                } catch {
                    model.alert = AppAlert(
                        title: "Folder could not be opened",
                        message: (error as? LocalizedError)?.errorDescription ?? String(describing: error)
                    )
                }
            }
        }
        .onAppear { destinationGrantId = model.restoreGrants.first?.id }
    }

    private func preparePreview() {
        guard let destinationGrantId else { return }
        isPreparing = true
        Task {
            let plan = await model.previewRestore(
                record: snapshot,
                destinationGrantId: destinationGrantId,
                conflictPolicy: .fail
            )
            if plan != nil { dismiss() }
            isPreparing = false
        }
    }
}

struct IOSRestorePreviewView: View {
    @ObservedObject var model: CovalentAppModel
    let context: RestorePreviewContext
    @Environment(\.dismiss) private var dismiss
    @State private var confirmReplace = false
    @State private var isRestoring = false

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Label("No files have been written", systemImage: "checkmark.shield")
                        .foregroundStyle(.green)
                    LabeledContent("Items", value: context.plan.entries.count.formatted())
                    LabeledContent("Conflict policy", value: context.plan.conflictPolicy.label)
                    LabeledContent("Authorized folder", value: context.destinationDisplayName)
                } header: {
                    Text("Signed plan")
                } footer: {
                    Text("The node signature binds the exact content and actions. The local folder permission confines extraction to the authorized empty folder.")
                }

                Section("Planned changes") {
                    ForEach(context.plan.entries) { entry in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(entry.destinationPath)
                                .font(.subheadline)
                                .lineLimit(2)
                                .truncationMode(.middle)
                            HStack {
                                Text(actionLabel(entry.action))
                                    .font(.caption.weight(.medium))
                                    .foregroundStyle(entry.action == .replaceFile ? .orange : .blue)
                                Spacer()
                                Text(entry.kind == .directory ? "Folder" : "File")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(.vertical, 3)
                    }
                }
            }
            .navigationTitle("Restore Preview")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        model.dismissRestorePreview()
                        dismiss()
                    }
                    .disabled(isRestoring)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(context.plan.conflictPolicy.isDestructive ? "Replace & Restore" : "Restore") {
                        if context.plan.conflictPolicy.isDestructive { confirmReplace = true }
                        else { restore() }
                    }
                    .disabled(isRestoring || model.activeTask != nil)
                    .accessibilityIdentifier("restore.execute")
                }
            }
            .overlay {
                if isRestoring { ProgressView("Restoring files…").padding().background(.background, in: RoundedRectangle(cornerRadius: 12)) }
            }
            .alert("Replace existing files?", isPresented: $confirmReplace) {
                Button("Replace and Restore", role: .destructive) { restore() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Only items marked Replace in this signed plan can be overwritten under the authorized folder.")
            }
        }
    }

    private func restore() {
        isRestoring = true
        Task {
            let result = await IOSBackgroundExecution.perform(named: "Covalent restore") {
                await model.executeRestore()
            }
            if result != nil { dismiss() }
            isRestoring = false
        }
    }

    private func actionLabel(_ action: RestoreAction) -> String {
        switch action {
        case .createFile: "Create file"
        case .createDirectory: "Create folder"
        case .keepDirectory: "Keep folder"
        case .skipFile: "Skip file"
        case .replaceFile: "Replace file"
        case .renameFile: "Keep both"
        }
    }
}

struct IOSRestoreResultView: View {
    let result: RestoreResponse
    let done: () -> Void

    var body: some View {
        VStack(spacing: 22) {
            Spacer()
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 58))
                .foregroundStyle(.green)
                .accessibilityHidden(true)
            Text("Restore Complete")
                .font(.title.weight(.semibold))
            Text("Covalent restored \(result.filesRestored) files and wrote \(result.bytesWritten.formatted(.byteCount(style: .file))).")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            HStack(spacing: 24) {
                metric("Files", result.filesRestored)
                metric("Folders", result.directoriesCreated)
                metric("Skipped", result.filesSkipped)
            }
            if result.rejectedProviderCopies > 0 {
                Label("\(result.rejectedProviderCopies) invalid provider copies rejected", systemImage: "checkmark.shield")
                    .foregroundStyle(.orange)
            }
            Button("Done", action: done)
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            Spacer()
        }
        .padding(28)
        .presentationDetents([.medium, .large])
    }

    private func metric(_ label: String, _ value: Int) -> some View {
        VStack(spacing: 3) {
            Text(value.formatted()).font(.title2.weight(.semibold)).monospacedDigit()
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}
