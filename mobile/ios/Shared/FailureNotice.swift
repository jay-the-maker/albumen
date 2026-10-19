// SPDX-License-Identifier: Apache-2.0

import Foundation
import UserNotifications

enum FailureNotice {
    static func post(_ message: String) {
        let content = UNMutableNotificationContent()
        content.title = "Albumen backup"
        content.body = message
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}
