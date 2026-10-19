// SPDX-License-Identifier: Apache-2.0

import BackgroundTasks
import UIKit

import AlbumenBackup
import AlbumenUpload

final class AppDelegate: NSObject, UIApplicationDelegate {
    let observer = LibraryObserver()
    private let finished = NSLock()
    private var refreshFinished = false

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: BackgroundUpload.refreshTaskIdentifier,
            using: nil
        ) { task in
            guard let processing = task as? BGProcessingTask else {
                task.setTaskCompleted(success: false)
                return
            }
            self.handle(processing)
        }
        return true
    }

    func application(
        _ application: UIApplication,
        handleEventsForBackgroundURLSession identifier: String,
        completionHandler: @escaping () -> Void
    ) {
        guard identifier == BackgroundUpload.sessionIdentifier else {
            completionHandler()
            return
        }
        BackgroundUploadController.shared.resumeLaunch(completionHandler)
    }

    func scheduleRefresh() {
        let request = BGProcessingTaskRequest(identifier: BackgroundUpload.refreshTaskIdentifier)
        request.requiresNetworkConnectivity = true
        request.requiresExternalPower = false
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            let directory = BackupContainer.directory()
            _ = try? BackupReportStore.update(directory: directory) { report in
                report.failures.append("Background refresh was not scheduled: \(error)")
            }
            FailureNotice.post(BackupReportStore.readStatus(directory: directory))
        }
    }

    private func handle(_ task: BGProcessingTask) {
        finished.lock()
        refreshFinished = false
        finished.unlock()
        let work = Task { @MainActor in
            let directory = BackupContainer.directory()
            let text = await PhotoLibraryBackup.run(
                directory: directory,
                accessGroup: KeychainAccess.group(bundle: .main),
                includeExisting: false
            )
            if !text.hasSuffix("Failures 0.") {
                FailureNotice.post(text)
            }
            self.complete(task)
            self.scheduleRefresh()
        }
        task.expirationHandler = {
            work.cancel()
            let directory = BackupContainer.directory()
            _ = try? BackupReportStore.update(directory: directory) { report in
                report.failures.append("The background pass stopped early. Queued originals stay queued.")
            }
            FailureNotice.post(BackupReportStore.readStatus(directory: directory))
            self.complete(task)
        }
    }

    private func complete(_ task: BGProcessingTask) {
        finished.lock()
        let already = refreshFinished
        refreshFinished = true
        finished.unlock()
        if !already {
            task.setTaskCompleted(success: true)
        }
    }
}
