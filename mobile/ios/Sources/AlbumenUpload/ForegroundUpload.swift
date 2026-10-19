// SPDX-License-Identifier: Apache-2.0

import Foundation
import UserNotifications

/// Foreground WebDAV upload. The simulator does not finish a background
/// `URLSession`, so this session sends the same PUT, answers the same Basic
/// challenge from the Keychain, and records the same SHA-256 verdict.
public enum ForegroundUpload {
    public static func foregroundConfiguration() -> URLSessionConfiguration {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 60
        configuration.waitsForConnectivity = false
        return configuration
    }

    /// Upload one original and write `albumen-backup-status.txt`. The returned
    /// text is what the status screen shows. The caller posts it as a local
    /// notification when the failure count is not zero.
    public static func uploadOriginal(
        file: URL,
        filename: String,
        localIdentifier: String,
        mtimeSeconds: Int,
        endpoint: URL,
        user: String,
        accessGroup: String?,
        directory: URL,
        memberId: String? = nil
    ) async -> String {
        let sha: String
        do {
            sha = try Sha256File.hex(file)
        } catch {
            return record(
                directory: directory,
                outcome: .failure(
                    UploadFailure.transport(String(describing: error)).surfaced(identifier: localIdentifier)
                )
            )
        }
        let outcome = await ForegroundTransfer.send(
            file: file,
            filename: filename,
            clientSha256: sha,
            localIdentifier: localIdentifier,
            mtimeSeconds: mtimeSeconds,
            endpoint: endpoint,
            user: user,
            accessGroup: accessGroup,
            memberId: memberId
        )
        return record(directory: directory, outcome: outcome)
    }

    private static func record(directory: URL, outcome: BackgroundOutcome) -> String {
        let report = try? BackupReportStore.update(directory: directory) { report in
            switch outcome {
            case .verified(_, let alreadyStored):
                if alreadyStored {
                    report.alreadyVerified += 1
                } else {
                    report.verifiedThisRun += 1
                }
            case .failure(let message):
                report.failures.append(message)
            }
        }
        if let report {
            return report.text
        }
        let failure = "The backup status was not saved.\nVerified 0 this run. Already verified 0. Failures 1."
        return failure
    }
}

/// One local notification. The status screen posts this when a pass has a failure.
public enum BackupNotice {
    public static func post(_ message: String, completion: @escaping (Error?) -> Void = { _ in }) {
        let content = UNMutableNotificationContent()
        content.title = "Albumen backup"
        content.body = message
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request, withCompletionHandler: completion)
    }
}

private final class ForegroundTransfer: NSObject, URLSessionDelegate, URLSessionTaskDelegate {
    private let file: URL
    private let filename: String
    private let clientSha256: String
    private let localIdentifier: String
    private let mtimeSeconds: Int
    private let endpoint: URL
    private let user: String
    private let accessGroup: String?
    private let memberId: String?
    private let lock = NSLock()
    private var continuation: CheckedContinuation<BackgroundOutcome, Never>?
    private var session: URLSession?

    static func send(
        file: URL,
        filename: String,
        clientSha256: String,
        localIdentifier: String,
        mtimeSeconds: Int,
        endpoint: URL,
        user: String,
        accessGroup: String?,
        memberId: String? = nil
    ) async -> BackgroundOutcome {
        let transfer = ForegroundTransfer(
            file: file,
            filename: filename,
            clientSha256: clientSha256,
            localIdentifier: localIdentifier,
            mtimeSeconds: mtimeSeconds,
            endpoint: endpoint,
            user: user,
            accessGroup: accessGroup,
            memberId: memberId
        )
        return await transfer.run()
    }

    private init(
        file: URL,
        filename: String,
        clientSha256: String,
        localIdentifier: String,
        mtimeSeconds: Int,
        endpoint: URL,
        user: String,
        accessGroup: String?,
        memberId: String?
    ) {
        self.file = file
        self.filename = filename
        self.clientSha256 = clientSha256
        self.localIdentifier = localIdentifier
        self.mtimeSeconds = mtimeSeconds
        self.endpoint = endpoint
        self.user = user
        self.accessGroup = accessGroup
        self.memberId = memberId
    }

    private func run() async -> BackgroundOutcome {
        let request: URLRequest
        do {
            request = try BackgroundPut.request(
                endpoint: endpoint,
                filename: filename,
                mtimeSeconds: mtimeSeconds,
                memberId: memberId
            )
        } catch let error as MemberNamespaceError {
            let member = memberId ?? ""
            return .failure(MemberNamespace.refusal(filename: filename, memberId: member, detail: error.message))
        } catch {
            return .failure(UploadFailure.transport(String(describing: error)).surfaced(identifier: localIdentifier))
        }
        return await withCheckedContinuation { continuation in
            lock.lock()
            self.continuation = continuation
            lock.unlock()
            let session = URLSession(
                configuration: ForegroundUpload.foregroundConfiguration(),
                delegate: self,
                delegateQueue: nil
            )
            self.session = session
            let task = session.uploadTask(with: request, fromFile: file)
            task.resume()
        }
    }

    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        answer(challenge, completionHandler: completionHandler)
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        answer(challenge, completionHandler: completionHandler)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let http = task.response as? HTTPURLResponse
        let outcome = BackgroundOutcome.decide(
            localIdentifier: localIdentifier,
            clientSha256: clientSha256,
            status: http?.statusCode,
            serverSha256: http?.value(forHTTPHeaderField: "X-Albumen-Sha256"),
            transport: error.map { String(describing: $0) }
        )
        finish(outcome)
        session.finishTasksAndInvalidate()
    }

    private func answer(
        _ challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodHTTPBasic,
              let password = try? ReceiverPassword.read(accessGroup: accessGroup),
              !password.isEmpty
        else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let credential = URLCredential(user: user, password: password, persistence: .none)
        completionHandler(.useCredential, credential)
    }

    private func finish(_ outcome: BackgroundOutcome) {
        lock.lock()
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(returning: outcome)
    }
}
