import SwiftUI

@main
struct CovalentMacApp: App {
    var body: some Scene {
        WindowGroup {
            MacRootView()
                .frame(minWidth: 720, minHeight: 520)
        }
        .commands {
            CommandGroup(after: .newItem) {
                Button("Pair Device…") {}
                    .keyboardShortcut("p", modifiers: [.command, .shift])
                    .disabled(true)
                    .help("Pairing is unavailable until the engine service is configured.")
            }
        }
    }
}
