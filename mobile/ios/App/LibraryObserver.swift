// SPDX-License-Identifier: Apache-2.0

import Photos

import AlbumenBackup
import AlbumenUpload

final class LibraryObserver: NSObject, PHPhotoLibraryChangeObserver {
    func start() {
        Task { @MainActor in
            guard PhotoLibraryBackup.authorizationFailure() == nil else {
                return
            }
            PHPhotoLibrary.shared().register(self)
        }
    }

    func photoLibraryDidChange(_ changeInstance: PHChange) {
        Task { @MainActor in
            let directory = BackupContainer.directory()
            let text = await PhotoLibraryBackup.run(
                directory: directory,
                accessGroup: KeychainAccess.group(bundle: .main),
                includeExisting: false
            )
            if !text.hasSuffix("Failures 0.") {
                FailureNotice.post(text)
            }
        }
    }
}
