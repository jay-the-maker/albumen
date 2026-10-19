// SPDX-License-Identifier: Apache-2.0

import Foundation

import AlbumenUpload

enum FailureNotice {
    static func post(_ message: String) {
        BackupNotice.post(message)
    }
}
