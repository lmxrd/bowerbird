import Foundation

/// The desktop app's session, for its File Provider extension on macOS
/// (docs/M5B_DESIGN.md §2). The app (Rust) keeps its token in the login
/// keychain, which a sandboxed extension cannot read; its helper
/// (`organizer-fileprovider session`) keeps a copy here, in the Keychain's
/// default access group, which the helper and the extension share (their
/// `keychain-access-groups` entitlement: the app's team), as the iOS app
/// shares its own with its share extension.
public struct DesktopSession: Codable, Sendable, Equatable {
  public var server: URL
  public var token: String

  public init(server: URL, token: String) {
    self.server = server
    self.token = token
  }

  public static let store = KeychainStore(
    service: "eu.vardalabs.organizer.desktop", account: "file-provider", accessGroup: nil)

  public static func load(from store: any SecretStore = store) -> DesktopSession? {
    guard let data = try? store.read() else { return nil }
    return try? JSONDecoder().decode(DesktopSession.self, from: data)
  }

  public func save(to store: any SecretStore = DesktopSession.store) throws {
    try store.write(JSONEncoder().encode(self))
  }

  public static func clear(from store: any SecretStore = store) throws {
    try store.delete()
  }
}
