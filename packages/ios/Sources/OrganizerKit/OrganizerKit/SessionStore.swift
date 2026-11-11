import Foundation
import Observation
import Security

/// The signed-in device: the server and its token (an API key of kind
/// `device`), with what Settings shows about it.
public struct StoredSession: Codable, Sendable, Equatable {
  public var server: URL
  public var token: String
  /// The key's id and prefix, as the server lists the device.
  public var keyID: String
  public var keyPrefix: String
  public var deviceName: String
  public var signedInAt: Date

  public init(
    server: URL, token: String, keyID: String, keyPrefix: String, deviceName: String,
    signedInAt: Date = .now
  ) {
    self.server = server
    self.token = token
    self.keyID = keyID
    self.keyPrefix = keyPrefix
    self.deviceName = deviceName
    self.signedInAt = signedInAt
  }
}

/// Where a secret is kept. The Keychain in the apps, memory in tests.
public protocol SecretStore: Sendable {
  func read() throws -> Data?
  func write(_ data: Data) throws
  func delete() throws
}

public struct KeychainError: Error, Equatable, LocalizedError {
  public let status: OSStatus
  public var errorDescription: String? {
    "The Keychain refused (\(status)): \(SecCopyErrorMessageString(status, nil) as String? ?? "")"
  }
}

/// One generic password in the Keychain, shared by the app and its share
/// extension through the access group (the app group). Readable after the
/// first unlock, so that uploads go on with the phone locked.
public struct KeychainStore: SecretStore {
  public let service: String
  public let account: String
  public let accessGroup: String?

  public init(
    service: String = "eu.vardalabs.organizer", account: String = "session",
    accessGroup: String? = Organizer.appGroup
  ) {
    self.service = service
    self.account = account
    self.accessGroup = accessGroup
  }

  private var query: [String: Any] {
    var query: [String: Any] = [
      kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: service,
      kSecAttrAccount as String: account,
      kSecUseDataProtectionKeychain as String: true,
    ]
    if let accessGroup { query[kSecAttrAccessGroup as String] = accessGroup }
    return query
  }

  public func read() throws -> Data? {
    var query = self.query
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var result: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    if status == errSecItemNotFound { return nil }
    guard status == errSecSuccess else { throw KeychainError(status: status) }
    return result as? Data
  }

  public func write(_ data: Data) throws {
    let attributes: [String: Any] = [
      kSecValueData as String: data,
      kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
    ]
    var status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
    if status == errSecItemNotFound {
      status = SecItemAdd(query.merging(attributes) { $1 } as CFDictionary, nil)
    }
    guard status == errSecSuccess else { throw KeychainError(status: status) }
  }

  public func delete() throws {
    let status = SecItemDelete(query as CFDictionary)
    guard status == errSecSuccess || status == errSecItemNotFound else {
      throw KeychainError(status: status)
    }
  }
}

/// A secret in memory, for tests and previews.
public final class MemoryStore: SecretStore, @unchecked Sendable {
  private let lock = NSLock()
  private var data: Data?

  public init(_ data: Data? = nil) { self.data = data }
  public func read() throws -> Data? { lock.withLock { data } }
  public func write(_ data: Data) throws { lock.withLock { self.data = data } }
  public func delete() throws { lock.withLock { data = nil } }
}

/// The session of the app: read from its store at start, written on sign
/// in, removed on sign out (or a 401).
@MainActor
@Observable
public final class SessionStore {
  public private(set) var session: StoredSession?
  @ObservationIgnored private let store: SecretStore

  public init(store: SecretStore = KeychainStore()) {
    self.store = store
    self.session = Self.load(from: store)
  }

  /// The session without an instance, for the share extension and the
  /// background uploads.
  public nonisolated static func load(from store: SecretStore = KeychainStore()) -> StoredSession? {
    guard let data = try? store.read() else { return nil }
    return try? JSONDecoder().decode(StoredSession.self, from: data)
  }

  public func save(_ session: StoredSession) throws {
    try store.write(JSONEncoder().encode(session))
    self.session = session
  }

  public func clear() {
    try? store.delete()
    session = nil
  }
}
