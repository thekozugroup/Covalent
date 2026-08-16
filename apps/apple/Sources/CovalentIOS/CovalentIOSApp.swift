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

        for itemAppearance in [
            appearance.stackedLayoutAppearance,
            appearance.inlineLayoutAppearance,
            appearance.compactInlineLayoutAppearance,
        ] {
            itemAppearance.normal.iconColor = .label
            itemAppearance.normal.titleTextAttributes = [.foregroundColor: UIColor.label]
            itemAppearance.selected.iconColor = .systemBlue
            itemAppearance.selected.titleTextAttributes = [.foregroundColor: UIColor.systemBlue]
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
