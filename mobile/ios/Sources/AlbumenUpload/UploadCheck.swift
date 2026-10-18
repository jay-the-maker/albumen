// SPDX-License-Identifier: Apache-2.0

import Foundation

public struct VerifiedUpload: Equatable, Sendable {
    public let filename: String
    public let sha256: String

    public init(filename: String, sha256: String) {
        self.filename = filename
        self.sha256 = sha256
    }
}

public enum UploadVerdict: Equatable, Sendable {
    /// 201 with the client's digest, including a collision-safe path for a
    /// different file that shared the camera filename. 409 with the same digest
    /// means those bytes were already stored and the retry was deduped.
    case verified(sha256: String)
    case conflict(client: String, server: String)
    case missingServerHash
    case rejected(status: Int)
}

public enum UploadFailure: Error, Equatable, Sendable {
    case conflict(client: String, server: String)
    case missingServerHash
    case rejected(status: Int)
    case transport(String)
    case badFilename(String)
    case noOriginal
}

public enum UploadCheck {
    /// Accept the upload only when `X-Albumen-Sha256` equals the digest of the
    /// bytes the client sent. 201 is a new object, even when the camera filename
    /// collided and the server stored it under another name. 409 is success only
    /// when that header is the digest of the identical bytes already stored.
    public static func verdict(
        clientSha256: String,
        status: Int,
        serverSha256: String?
    ) -> UploadVerdict {
        let client = clientSha256.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let server = serverSha256?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased() ?? ""
        if server.isEmpty {
            return .missingServerHash
        }
        if status == 201 || status == 409 {
            if client == server {
                return .verified(sha256: client)
            }
            return .conflict(client: client, server: server)
        }
        return .rejected(status: status)
    }

    public static func requireVerified(_ verdict: UploadVerdict) throws -> String {
        switch verdict {
        case .verified(let sha256):
            return sha256
        case .conflict(let client, let server):
            throw UploadFailure.conflict(client: client, server: server)
        case .missingServerHash:
            throw UploadFailure.missingServerHash
        case .rejected(let status):
            throw UploadFailure.rejected(status: status)
        }
    }
}

public enum StoredName {
    /// Keep the filename. Directory pieces are dropped. `..` is refused.
    public static func require(_ raw: String) throws -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty
            || trimmed.contains("\0")
            || trimmed.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
        {
            throw UploadFailure.badFilename(raw)
        }
        let parts = trimmed.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "/" || $0 == "\\" })
            .map(String.init)
        if parts.contains("..") {
            throw UploadFailure.badFilename(raw)
        }
        guard let name = parts.last(where: { !$0.isEmpty && $0 != "." }) else {
            throw UploadFailure.badFilename(raw)
        }
        return name
    }
}
