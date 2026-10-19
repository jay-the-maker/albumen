// SPDX-License-Identifier: Apache-2.0

import Foundation

/// One background `URLSession` for this process. The delegate answers the
/// receiver's Basic challenge from the Keychain and records every result.
public final class BackgroundUploadSession: NSObject, URLSessionDelegate, URLSessionTaskDelegate, URLSessionDataDelegate {
    public let user: String
    public let accessGroup: String?
    public let directory: URL
    private var session: URLSession!
    private var launchCompletion: (() -> Void)?
    private let lock = NSLock()

    public init(user: String, accessGroup: String?, directory: URL) {
        self.user = user
        self.accessGroup = accessGroup
        self.directory = directory
        super.init()
        session = URLSession(
            configuration: BackgroundUpload.makeConfiguration(),
            delegate: self,
            delegateQueue: nil
        )
    }

    public func resumeLaunch(_ completion: @escaping () -> Void) {
        lock.lock()
        launchCompletion = completion
        lock.unlock()
    }

    public func enqueue(
        file: URL,
        filename: String,
        clientSha256: String,
        localIdentifier: String,
        mtimeSeconds: Int,
        endpoint: URL,
        memberId: String? = nil
    ) throws {
        let request = try BackgroundPut.request(
            endpoint: endpoint,
            filename: filename,
            mtimeSeconds: mtimeSeconds,
            memberId: memberId
        )
        let id = UUID().uuidString
        try PendingUploadStore.update(directory: directory) { pending in
            pending.append(
                PendingUpload(
                    id: id,
                    localIdentifier: localIdentifier,
                    filename: filename,
                    clientSha256: clientSha256,
                    scratchPath: file.path
                )
            )
        }
        let task = session.uploadTask(with: request, fromFile: file)
        task.taskDescription = id
        task.resume()
    }

    public func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didReceive challenge: URLAuthenticationChallenge,
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

    public func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let id = task.taskDescription ?? ""
        let http = task.response as? HTTPURLResponse
        finish(id: id, status: http?.statusCode, serverSha256: http?.value(forHTTPHeaderField: "X-Albumen-Sha256"), error: error)
    }

    public func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        lock.lock()
        let done = launchCompletion
        launchCompletion = nil
        lock.unlock()
        done?()
    }

    private func finish(id: String, status: Int?, serverSha256: String?, error: Error?) {
        do {
            let removed: PendingUpload? = try PendingUploadStore.update(directory: directory) { pending in
                guard let index = pending.firstIndex(where: { $0.id == id }) else {
                    return nil
                }
                return pending.remove(at: index)
            }
            guard let item = removed else {
                _ = try BackupReportStore.update(directory: directory) { report in
                    report.failures.append("An upload finished without a matching original (\(id)).")
                }
                return
            }
            let scratch = directory.appendingPathComponent("scratch", isDirectory: true)
            try? BackgroundScratch.remove(scratch: scratch, file: URL(fileURLWithPath: item.scratchPath))
            let outcome = BackgroundOutcome.decide(
                localIdentifier: item.localIdentifier,
                clientSha256: item.clientSha256,
                status: status,
                serverSha256: serverSha256,
                transport: error.map { String(describing: $0) }
            )
            let stillRunning = try PendingUploadStore.load(directory: directory)
                .contains { $0.localIdentifier == item.localIdentifier }
            _ = try BackupReportStore.update(directory: directory) { report in
                switch outcome {
                case .verified(_, let alreadyStored):
                    if alreadyStored {
                        report.alreadyVerified += 1
                    } else {
                        report.verifiedThisRun += 1
                    }
                    if !stillRunning {
                        try? UploadQueueStore.update(directory: directory) { queue in
                            queue.pending.removeAll { $0 == item.localIdentifier }
                            queue.derivativeMessages[item.localIdentifier] = nil
                        }
                    }
                case .failure(let message):
                    report.failures.append(message)
                }
            }
        } catch {
            _ = try? BackupReportStore.update(directory: directory) { report in
                report.failures.append("The upload result was not recorded: \(error)")
            }
        }
    }
}

public final class BackgroundUploadController {
    public static let shared = BackgroundUploadController()
    private let lock = NSLock()
    private var session: BackgroundUploadSession?

    public func open(user: String, accessGroup: String?, directory: URL) -> BackgroundUploadSession {
        lock.lock()
        defer { lock.unlock() }
        if let session {
            return session
        }
        let created = BackgroundUploadSession(user: user, accessGroup: accessGroup, directory: directory)
        session = created
        return created
    }

    public func resumeLaunch(_ completion: @escaping () -> Void) {
        lock.lock()
        let session = session
        lock.unlock()
        guard let session else {
            completion()
            return
        }
        session.resumeLaunch(completion)
    }
}
