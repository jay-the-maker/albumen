// SPDX-License-Identifier: Apache-2.0

import Photos
import SwiftUI
import UIKit
import UserNotifications

import AlbumenBackup
import AlbumenUpload

struct StatusScreen: View {
    @State private var endpoint = ""
    @State private var user = ""
    @State private var password = ""
    @State private var status = BackupReport().text
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Albumen")
                    .font(.title)
                Text("Copies originals to your receiver. Photos stay in the library.")
                TextField("Receiver", text: $endpoint)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                TextField("User", text: $user)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                SecureField("Receiver password", text: $password)
                Button("Save and back up") {
                    saveAndBackup()
                }
                Text(status)
                    .font(.body.monospaced())
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding()
        }
        .onAppear(perform: reload)
        .onChange(of: scenePhase) { phase in
            if phase == .active {
                reload()
            }
        }
    }

    private func reload() {
        let directory = BackupContainer.directory()
        status = BackupReportStore.readStatus(directory: directory)
        if let config = try? ReceiverEndpointStore.load(directory: directory) {
            if endpoint.isEmpty {
                endpoint = config.endpoint
            }
            if user.isEmpty {
                user = config.user
            }
        }
    }

    private func saveAndBackup() {
        let directory = BackupContainer.directory()
        do {
            try ReceiverEndpointStore.save(
                ReceiverEndpoint(endpoint: endpoint, user: user),
                directory: directory
            )
            if !password.isEmpty {
                try ReceiverPassword.save(password, accessGroup: KeychainAccess.group(bundle: .main))
                password = ""
            }
        } catch {
            status = "The receiver settings were not saved: \(error)\n\(BackupReport().text)"
            FailureNotice.post(status)
            return
        }
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
        PHPhotoLibrary.requestAuthorization(for: .readWrite) { _ in
            Task { @MainActor in
                status = await PhotoLibraryBackup.run(
                    directory: directory,
                    accessGroup: KeychainAccess.group(bundle: .main),
                    includeExisting: true
                )
                if !status.hasSuffix("Failures 0.") {
                    FailureNotice.post(status)
                }
                (UIApplication.shared.delegate as? AppDelegate)?.observer.start()
                (UIApplication.shared.delegate as? AppDelegate)?.scheduleRefresh()
            }
        }
    }
}
