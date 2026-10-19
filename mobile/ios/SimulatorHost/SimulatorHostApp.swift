// SPDX-License-Identifier: Apache-2.0

import UIKit

/// Process the simulator job launches. It allows cleartext HTTP to the
/// loopback receiver and gives the local notification a bundle to grant.
/// It does not read Photos, and it does not embed the upload extension.
@main
final class SimulatorHostApp: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = UIViewController()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}
