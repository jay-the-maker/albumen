// SPDX-License-Identifier: Apache-2.0

import Foundation
import Security

public struct KeychainError: Error, Equatable, Sendable {
    public let status: OSStatus

    public init(status: OSStatus) {
        self.status = status
    }
}

/// Receiver password in the Keychain. It is not written to UserDefaults or to a file.
public enum ReceiverPassword {
    public static let service = "photos.albumen.receiver"
    public static let account = "password"

    public static func save(_ password: String) throws {
        guard !password.isEmpty else {
            throw UploadFailure.missingPassword
        }
        let query = lookupQuery()
        SecItemDelete(query as CFDictionary)
        var insert = query
        insert[kSecValueData as String] = Data(password.utf8)
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        let status = SecItemAdd(insert as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw KeychainError(status: status)
        }
    }

    public static func read() throws -> String? {
        var query = lookupQuery()
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess else {
            throw KeychainError(status: status)
        }
        guard let data = item as? Data, let password = String(data: data, encoding: .utf8), !password.isEmpty else {
            throw UploadFailure.missingPassword
        }
        return password
    }

    /// Attributes used to find the item. The password bytes are not in this dictionary.
    public static func lookupQuery() -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }
}
