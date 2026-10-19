// SPDX-License-Identifier: Apache-2.0

import Foundation

import AlbumenBackup
import AlbumenUpload

/// Principal class for the embedded background upload extension.
/// It reconnects the background URLSession and copies PHAsset originals.
/// It does not delete Photos assets.
final class BackgroundUploadProcessor: NSObject, NSExtensionRequestHandling {
    override init() {
        super.init()
        reconnect()
    }

    func beginRequest(with context: NSExtensionContext) {
        let directory = BackupContainer.directory()
        Task { @MainActor in
            let text = await PhotoLibraryBackup.run(
                directory: directory,
                accessGroup: KeychainAccess.group(bundle: .main),
                includeExisting: false
            )
            if !text.hasSuffix("Failures 0.") {
                FailureNotice.post(text)
            }
            context.completeRequest(returningItems: nil)
        }
    }

    private func reconnect() {
        let directory = BackupContainer.directory()
        guard let config = try? ReceiverEndpointStore.load(directory: directory), !config.user.isEmpty else {
            return
        }
        _ = BackgroundUploadController.shared.open(
            user: config.user,
            accessGroup: KeychainAccess.group(bundle: .main),
            directory: directory
        )
    }
}
