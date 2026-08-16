import BackgroundTasks
import UIKit

@MainActor
enum IOSBackgroundExecution {
    static let processingIdentifier = "life.michaelwong.covalent.ios.transfer-continuation"
    private static weak var model: CovalentAppModel?
    private static var isRegistered = false

    static func register(model: CovalentAppModel) {
        self.model = model
        guard !isRegistered else { return }
        isRegistered = BGTaskScheduler.shared.register(
            forTaskWithIdentifier: processingIdentifier,
            using: nil
        ) { task in
            guard let processingTask = task as? BGProcessingTask else {
                task.setTaskCompleted(success: false)
                return
            }
            Task { @MainActor in await handle(processingTask) }
        }
    }

    static func scheduleContinuation() {
        let request = BGProcessingTaskRequest(identifier: processingIdentifier)
        request.requiresNetworkConnectivity = true
        request.requiresExternalPower = false
        try? BGTaskScheduler.shared.submit(request)
    }

    static func perform<Value: Sendable>(
        named name: String,
        onExpiration: @escaping @MainActor () async -> Void = {},
        _ operation: @escaping @MainActor () async -> Value
    ) async -> Value {
        let operationTask = Task { await operation() }
        let allowance = BackgroundAllowance(name: name) {
            scheduleContinuation()
            Task { @MainActor in
                await onExpiration()
                operationTask.cancel()
            }
        }
        defer { allowance.end() }
        return await operationTask.value
    }

    private static func handle(_ task: BGProcessingTask) async {
        scheduleContinuation()
        guard let model else {
            task.setTaskCompleted(success: false)
            return
        }
        let refreshTask = Task { @MainActor in
            await model.refresh()
            return model.phase == .ready || model.phase == .needsAuthorization
        }
        task.expirationHandler = { refreshTask.cancel() }
        let success = await refreshTask.value
        task.setTaskCompleted(success: success && !refreshTask.isCancelled)
    }
}

@MainActor
private final class BackgroundAllowance {
    private var identifier: UIBackgroundTaskIdentifier = .invalid

    init(name: String, expirationHandler: @escaping @MainActor () -> Void) {
        identifier = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
            Task { @MainActor [weak self] in
                expirationHandler()
                self?.end()
            }
        }
    }

    func end() {
        guard identifier != .invalid else { return }
        UIApplication.shared.endBackgroundTask(identifier)
        identifier = .invalid
    }
}
