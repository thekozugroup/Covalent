import UIKit

@MainActor
enum IOSBackgroundExecution {
    static func perform<Value>(
        named name: String,
        _ operation: () async -> Value
    ) async -> Value {
        let allowance = BackgroundAllowance(name: name)
        defer { allowance.end() }
        return await operation()
    }
}

@MainActor
private final class BackgroundAllowance {
    private var identifier: UIBackgroundTaskIdentifier = .invalid

    init(name: String) {
        identifier = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
            Task { @MainActor [weak self] in self?.end() }
        }
    }

    func end() {
        guard identifier != .invalid else { return }
        UIApplication.shared.endBackgroundTask(identifier)
        identifier = .invalid
    }
}
