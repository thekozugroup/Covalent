import SwiftUI
import UIKit

@main
struct CovalentIOSApp: App {
    @StateObject private var model: CovalentAppModel

    init() {
        let model = CovalentAppModel()
        _model = StateObject(wrappedValue: model)
        IOSBackgroundExecution.register(model: model)
        let appearance = UITabBarAppearance()
        appearance.configureWithOpaqueBackground()
        appearance.backgroundColor = .systemBackground
        // Tab captions are small text. The default light-mode system blue is
        // below 4.5:1 against this opaque white background; use an adaptive
        // selected color with ample contrast in both appearances.
        let selectedColor = UIColor { traits in
            if traits.userInterfaceStyle == .dark {
                UIColor(red: 0.45, green: 0.72, blue: 1, alpha: 1)
            } else {
                UIColor(red: 0, green: 0.27, blue: 0.58, alpha: 1)
            }
        }

        for itemAppearance in [
            appearance.stackedLayoutAppearance,
            appearance.inlineLayoutAppearance,
            appearance.compactInlineLayoutAppearance,
        ] {
            itemAppearance.normal.iconColor = .label
            itemAppearance.normal.titleTextAttributes = [.foregroundColor: UIColor.label]
            itemAppearance.selected.iconColor = selectedColor
            itemAppearance.selected.titleTextAttributes = [.foregroundColor: selectedColor]
        }

        UITabBar.appearance().standardAppearance = appearance
        UITabBar.appearance().scrollEdgeAppearance = appearance
    }

    var body: some Scene {
        WindowGroup {
            IOSRootView(model: model)
        }
    }
}
