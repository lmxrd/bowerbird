import Foundation

public typealias News = Components.Schemas.News
public typealias Notice = Components.Schemas.Notice

/// The app's tabs; a notice opens one.
public enum AppTab: Hashable, Sendable {
  case files
  case dropBox
  case settings

  /// The tab of a page of the web app, as a notice names it: `/drop-box`,
  /// `/settings/backups`, `/` (what was shared, among the files).
  public init(path: String) {
    let first = path.split(separator: "/").first.map(String.init) ?? ""
    switch first {
    case "drop-box": self = .dropBox
    case "settings": self = .settings
    default: self = .files
    }
  }
}

/// An APNs device token as the server takes it: lowercase hex.
public enum PushToken {
  public static func hex(_ token: Data) -> String {
    token.map { String(format: "%02x", $0) }.joined()
  }
}

/// What the app shows of a push: the newest notice's words and the tab it
/// opens. The push itself says nothing ("Something new for you"); the
/// words come from `GET /api/notifications/news`.
public struct PushNotice: Equatable, Sendable {
  public var title: String
  public var body: String
  /// The page of the web app it is about, e.g. `/drop-box`.
  public var path: String

  public init(title: String, body: String, path: String) {
    self.title = title
    self.body = body
    self.path = path
  }

  public init(_ notice: Notice) {
    self.init(title: notice.title, body: notice.body, path: notice.path)
  }

  /// The tab it opens.
  public var tab: AppTab { AppTab(path: path) }
}

/// This device's subscription on the server, kept to remove it at sign-out
/// and to replace it when the token changes. `keyID`: the session it was
/// made with (another sign-in makes its own).
public struct PushRegistration: Codable, Sendable, Equatable {
  public var id: String
  public var token: String
  public var keyID: String

  public init(id: String, token: String, keyID: String) {
    self.id = id
    self.token = token
    self.keyID = keyID
  }

  public static func load(from store: SecretStore) -> PushRegistration? {
    guard let data = try? store.read() else { return nil }
    return try? JSONDecoder().decode(PushRegistration.self, from: data)
  }
}
