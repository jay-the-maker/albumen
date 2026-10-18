// SPDX-License-Identifier: Apache-2.0

import Foundation

/// PUT an original at `albumen-receive`. The camera-roll asset is not this type's concern.
public struct WebDavUploader: Sendable {
    public let endpoint: URL
    public let user: String
    public let password: String

    public init(endpoint: URL, user: String, password: String) {
        self.endpoint = endpoint
        self.user = user
        self.password = password
    }

    public func putOriginal(filename: String, file: URL) async throws -> UploadVerdict {
        let name = try StoredName.require(filename)
        let client = try Sha256File.hex(file)
        var request = URLRequest(url: endpoint.appendingPathComponent(name))
        request.httpMethod = "PUT"
        let token = Data("\(user):\(password)".utf8).base64EncodedString()
        request.setValue("Basic \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        let delegate = IgnoreRedirects()
        let session = URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        let response: URLResponse
        do {
            (_, response) = try await session.upload(for: request, fromFile: file)
        } catch {
            throw UploadFailure.transport(String(describing: error))
        }
        guard let http = response as? HTTPURLResponse else {
            throw UploadFailure.transport("response was not HTTP")
        }
        return UploadCheck.verdict(
            clientSha256: client,
            status: http.statusCode,
            serverSha256: http.value(forHTTPHeaderField: "X-Albumen-Sha256")
        )
    }
}

private final class IgnoreRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest
    ) async -> URLRequest? {
        nil
    }
}
