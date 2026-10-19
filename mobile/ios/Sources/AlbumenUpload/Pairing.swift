// SPDX-License-Identifier: Apache-2.0

import CryptoKit
import Foundation

/// `albumen-pair://v1` from `albumen-receive --pair`.
///
/// The URI carries the WebDAV endpoint, the SHA-256 fingerprint of the
/// receiver certificate, a one-time token, and the family member public key
/// when the receiver was started for one member. The same vectors are parsed
/// in `receive/src/pairing.rs` and on Android.
public enum Pairing {
    public static let exampleWithMember =
        "albumen-pair://v1?endpoint=https%3A%2F%2F127.0.0.1%3A8443%2F&fingerprint=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&member=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

    public static let exampleWithoutMember =
        "albumen-pair://v1?endpoint=https%3A%2F%2Fphotos.example%3A8443%2F&fingerprint=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

    public struct Code: Equatable, Sendable {
        public let endpoint: String
        public let fingerprint: String
        public let token: String
        public let member: String?

        public init(endpoint: String, fingerprint: String, token: String, member: String?) {
            self.endpoint = endpoint
            self.fingerprint = fingerprint
            self.token = token
            self.member = member
        }
    }

    public struct Rejection: Error, Equatable, CustomStringConvertible {
        public let message: String

        public init(_ message: String = Pairing.rejected) {
            self.message = message
        }

        public var description: String { message }
    }

    public static let rejected =
        "The pairing code is not an albumen-pair v1 URI. The copy did not start."

    public static func parse(_ text: String) throws -> Code {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count <= 2048, trimmed.hasPrefix("albumen-pair://") else {
            throw Rejection()
        }
        let rest = String(trimmed.dropFirst("albumen-pair://".count))
        guard let split = rest.firstIndex(of: "?") else {
            throw Rejection()
        }
        let version = rest[..<split].trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard version == "v1" else {
            throw Rejection()
        }
        let pairs = try queryPairs(String(rest[rest.index(after: split)...]))
        let endpoint = try required(pairs, "endpoint")
        guard let fingerprint = normalizeFingerprint(try required(pairs, "fingerprint")),
            isHex64(try required(pairs, "token")),
            validEndpoint(endpoint)
        else {
            throw Rejection()
        }
        let token = try required(pairs, "token").lowercased()
        let member: String?
        if let value = pairs["member"] {
            guard isHex64(value) else {
                throw Rejection()
            }
            member = value.lowercased()
        } else {
            member = nil
        }
        return Code(endpoint: endpoint, fingerprint: fingerprint, token: token, member: member)
    }

    /// SHA-256 of the certificate encoding, lowercase hex.
    public static func fingerprint(encodedCertificate: Data) -> String {
        SHA256.hash(data: encodedCertificate).map { String(format: "%02x", $0) }.joined()
    }

    /// Nil when the certificate matches the pairing code.
    /// A mismatch is the sentence shown on the status screen. The receiver
    /// password is not part of that sentence, and the copy does not start.
    public static func pin(expected: String, presented: String) -> String? {
        let presentedClean = presented.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "\r", with: " ")
            .replacingOccurrences(of: "\n", with: " ")
        let expectedNorm = normalizeFingerprint(expected)
        let presentedNorm = normalizeFingerprint(presentedClean)
        if let expectedNorm, expectedNorm == presentedNorm {
            return nil
        }
        guard let expectedNorm else {
            return "Certificate fingerprint does not match the pairing code. The pairing code has no fingerprint. The copy did not start."
        }
        return "Certificate fingerprint does not match the pairing code. Expected \(expectedNorm). Received \(presentedClean). The copy did not start."
    }

    private static func queryPairs(_ query: String) throws -> [String: String] {
        if query.isEmpty {
            throw Rejection()
        }
        var pairs: [String: String] = [:]
        for part in query.split(separator: "&", omittingEmptySubsequences: false) {
            guard let eq = part.firstIndex(of: "="), eq != part.startIndex else {
                throw Rejection()
            }
            let key = try percentDecode(String(part[..<eq]))
            let value = try percentDecode(String(part[part.index(after: eq)...]))
            if pairs[key] != nil {
                throw Rejection()
            }
            pairs[key] = value
        }
        return pairs
    }

    private static func required(_ pairs: [String: String], _ key: String) throws -> String {
        guard let value = pairs[key] else {
            throw Rejection()
        }
        return value
    }

    private static func validEndpoint(_ endpoint: String) -> Bool {
        guard endpoint.hasPrefix("https://") else {
            return false
        }
        let rest = String(endpoint.dropFirst("https://".count))
        if rest.isEmpty || rest.contains(" ") || rest.contains("\n") || rest.contains("\r") || rest.contains("@") {
            return false
        }
        return !rest.split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false).first!.isEmpty
    }

    private static func normalizeFingerprint(_ value: String) -> String? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return isHex64(trimmed) ? trimmed.lowercased() : nil
    }

    private static func isHex64(_ value: String) -> Bool {
        value.count == 64 && value.allSatisfy(\.isHexDigit)
    }

    private static func percentDecode(_ text: String) throws -> String {
        var bytes = [UInt8]()
        bytes.reserveCapacity(text.utf8.count)
        var index = text.startIndex
        while index < text.endIndex {
            if text[index] == "%" {
                let first = text.index(after: index)
                guard first < text.endIndex else {
                    throw Rejection()
                }
                let second = text.index(after: first)
                guard second < text.endIndex else {
                    throw Rejection()
                }
                let hex = text[first...second]
                guard let value = UInt8(hex, radix: 16) else {
                    throw Rejection()
                }
                bytes.append(value)
                index = text.index(after: second)
            } else {
                let scalar = text[index].unicodeScalars
                for unit in String(scalar).utf8 {
                    bytes.append(unit)
                }
                index = text.index(after: index)
            }
        }
        guard let decoded = String(bytes: bytes, encoding: .utf8) else {
            throw Rejection()
        }
        return decoded
    }
}
