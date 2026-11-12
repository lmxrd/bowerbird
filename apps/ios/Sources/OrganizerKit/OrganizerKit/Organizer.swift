import Foundation

/// The identifiers the app and its share extension share, and the few
/// numbers the uploads go by (docs/M4_DESIGN.md §2).
public enum Organizer {
  /// The app group: its container holds the uploads waiting to be sent,
  /// and it is the Keychain access group of the session.
  public static let appGroup = "group.eu.vardalabs.organizer"
  /// The app's own redirect scheme for the device login.
  public static let callbackScheme = "organizer"
  public static let redirectURI = "organizer://auth"
  /// The background URLSession of the app.
  public static let backgroundSession = "eu.vardalabs.organizer.uploads"
  /// The prefix of a share extension's own background sessions.
  public static let extensionSessionPrefix = "eu.vardalabs.organizer.share."
  /// Files under this size go in one plain request; larger ones (into a
  /// folder or the drop box) through the resumable uploads.
  public static let plainUploadLimit: Int64 = 4 * 1024 * 1024
  /// The interop version of the resumable upload draft the server speaks.
  public static let uploadInteropVersion = 6

  /// The app group's container, or a folder of the temporary directory
  /// where there is none (tests without the entitlement).
  public static func sharedContainer() -> URL {
    if let url = FileManager.default.containerURL(
      forSecurityApplicationGroupIdentifier: appGroup)
    {
      return url
    }
    return FileManager.default.temporaryDirectory.appending(path: "OrganizerShared")
  }
}
