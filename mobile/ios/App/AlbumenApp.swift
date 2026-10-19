// SPDX-License-Identifier: Apache-2.0

import SwiftUI

@main
struct AlbumenApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup {
            StatusScreen()
        }
    }
}
