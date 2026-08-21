import AppKit
import SwiftUI

@main
struct CovalentMacApp: App {
    @StateObject private var model: CovalentAppModel
    private let localNodeManager: LocalNodeManager?

    init() {
        let isUITest = ProcessInfo.processInfo.environment["COVALENT_UI_TEST_BASE_URL"] != nil
        let manager = isUITest ? nil : LocalNodeManager()
        localNodeManager = manager
        _model = StateObject(wrappedValue: CovalentAppModel(localNodeBootstrapper: manager))
    }

    var body: some Scene {
        Window("Covalent", id: "main") {
            MacRootView(model: model)
                .frame(minWidth: 900, minHeight: 640)
                // The window's root container carried no description at all,
                // which the system accessibility audit reports as "Element has
                // no description". Name it, and keep every child individually
                // reachable underneath.
                .accessibilityElement(children: .contain)
                .accessibilityLabel("Covalent")
        }
        .defaultSize(width: 1_080, height: 720)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Backup…") {
                    model.requestNewBackup()
                }
                .keyboardShortcut("n")
                .disabled(!model.isAuthorized || model.activeTask != nil)

                Button("Pair Device…") {
                    model.selectedSection = .devices
                    Task { await model.refreshDiscovery() }
                }
                .keyboardShortcut("p", modifiers: [.command, .shift])
                .disabled(!model.isAuthorized)
            }
            CommandGroup(after: .sidebar) {
                Divider()
                Button("Overview") { model.selectedSection = .overview }
                    .keyboardShortcut("1")
                Button("Backups") { model.selectedSection = .backups }
                    .keyboardShortcut("2")
                Button("Devices") { model.selectedSection = .devices }
                    .keyboardShortcut("3")
            }
            CommandMenu("Service") {
                Button("Refresh") {
                    Task { await model.refresh() }
                }
                .keyboardShortcut("r")
                Button("Connect…") {
                    model.presentation = .connection
                }
            }
        }

        Settings {
            MacSettingsView(model: model, compact: true)
                .frame(width: 620, height: 540)
        }

        // The `systemImage:` convenience publishes the raw SF Symbol name as
        // the status item's accessibility title, so VoiceOver announced
        // "externaldrive.badge.checkmark". Supply the label explicitly so the
        // menu bar item is named for what it is, and says what it means.
        MenuBarExtra {
            MacMenuBarMenu(model: model)
        } label: {
            Image(systemName: menuBarSymbol)
                .accessibilityLabel("Covalent")
                .accessibilityValue(model.serviceStatusLabel)
                .accessibilityIdentifier("Covalent")
        }
        .menuBarExtraStyle(.menu)
    }

    private var menuBarSymbol: String {
        switch model.phase {
        case .starting: "arrow.trianglehead.2.clockwise.rotate.90"
        case .ready: "externaldrive.badge.checkmark"
        case .needsAuthorization: "externaldrive.badge.questionmark"
        case .offline: "externaldrive.badge.xmark"
        }
    }
}

private struct MacMenuBarMenu: View {
    @ObservedObject var model: CovalentAppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Open Covalent") {
            showMainWindow()
        }

        Divider()

        Text(statusSummary)
        if let task = model.activeTask {
            Text("\(task.kind.label): \(task.title)")
            if task.jobId != nil {
                Button(task.state == .paused ? "Resume Active Job" : "Pause Active Job") {
                    Task { await model.controlActiveTask(task.state == .paused ? .resume : .pause) }
                }
                Button("Cancel Active Job…", role: .destructive) {
                    Task { await model.controlActiveTask(.cancel) }
                }
            }
        }

        Divider()

        Button("New Backup…") {
            model.requestNewBackup()
            showMainWindow()
        }
        .disabled(!model.isAuthorized || model.activeTask != nil)

        Button("Restore Latest Backup…") {
            model.requestRestoreLatest()
            showMainWindow()
        }
        .disabled(!model.isAuthorized || model.snapshots.isEmpty || model.activeTask != nil)

        Button("Refresh Status") {
            Task { await model.refresh() }
        }

        Divider()

        SettingsLink {
            Text("Settings…")
        }

        Button("Quit Covalent") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q")
    }

    private var statusSummary: String {
        if let status = model.status {
            return "\(model.serviceStatusLabel) · \(status.deviceName)"
        }
        return model.serviceStatusLabel
    }

    private func showMainWindow() {
        openWindow(id: "main")
        NSApplication.shared.activate()
    }
}
