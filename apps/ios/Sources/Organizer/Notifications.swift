import OrganizerKit
import UIKit
import UserNotifications
import os

/// Push notifications through APNs (docs/M6D_DESIGN.md). The server's push
/// says nothing ("Organizer: Something new for you", content-available);
/// the app asks the server what is new (`AppModel.news`):
///
/// - in the foreground, the generic alert is not shown: a local
///   notification with the newest notice's words is, in its place;
/// - woken in the background, it sets the count to review and the icon's
///   badge (the alert shown is the generic one: changing it would take a
///   notification service extension);
/// - a tapped notification opens the notice's tab.
@MainActor
final class Notifications: NSObject, UNUserNotificationCenterDelegate {
  static let shared = Notifications()
  private let log = Logger(subsystem: "eu.vardalabs.organizer", category: "push")
  /// Our own notifications' key for the page they are about.
  nonisolated private static let pathKey = "path"

  private var app: AppModel { Shared.app }

  func start() {
    UNUserNotificationCenter.current().delegate = self
  }

  /// Once signed in (and at every launch): asks whether notices may be
  /// shown, the first time, then registers for remote notifications (the
  /// token comes to `AppDelegate`, which sends it). Turned off in iOS's
  /// Settings: the device's subscription is removed.
  func setUp() async {
    guard app.session != nil else { return }
    let center = UNUserNotificationCenter.current()
    var status = await center.notificationSettings().authorizationStatus
    if status == .notDetermined {
      _ = try? await center.requestAuthorization(options: [.alert, .badge, .sound])
      status = await center.notificationSettings().authorizationStatus
    }
    log.debug("Notifications authorization: \(status.rawValue, privacy: .public)")
    switch status {
    case .authorized, .provisional, .ephemeral:
      UIApplication.shared.registerForRemoteNotifications()
    case .denied:
      await app.unregisterPush()
    default:
      break
    }
  }

  /// Signed out: nothing more is wanted here. After a sign-out by the
  /// server the subscription is still there, and only this makes APNs
  /// answer that the token is gone, so that the server removes it.
  func signedOut() {
    UIApplication.shared.unregisterForRemoteNotifications()
    setBadge(0)
  }

  func setBadge(_ count: Int) {
    UNUserNotificationCenter.current().setBadgeCount(count) { _ in }
  }

  func registered(token: Data) {
    Task { await app.registerPush(token: token) }
  }

  func failedToRegister(_ error: any Error) {
    // Without `aps-environment` in the signature (no team yet): expected.
    log.error("No APNs token: \(error.localizedDescription, privacy: .public)")
  }

  /// Woken in the background by a push: the count to review and the badge.
  func wokenByPush() async -> UIBackgroundFetchResult {
    // In the foreground `willPresent` does it.
    guard UIApplication.shared.applicationState != .active else { return .noData }
    do {
      _ = try await app.news()
      setBadge(app.toReview)
      return .newData
    } catch {
      return .failed
    }
  }

  // MARK: UNUserNotificationCenterDelegate

  // Nonisolated, as the protocol is: what is needed of UserNotifications'
  // objects (not Sendable) is taken here, the rest is on the main actor.

  nonisolated func userNotificationCenter(
    _ center: UNUserNotificationCenter, willPresent notification: UNNotification
  ) async -> UNNotificationPresentationOptions {
    let isPush = notification.request.trigger is UNPushNotificationTrigger
    return await presentation(isPush: isPush)
  }

  nonisolated func userNotificationCenter(
    _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
  ) async {
    let path = response.notification.request.content.userInfo[Self.pathKey] as? String
    await app.open(tab: path.map(AppTab.init(path:)))
  }

  /// In the foreground: the newest notice's words in place of the push's.
  private func presentation(isPush: Bool) async -> UNNotificationPresentationOptions {
    let shown: UNNotificationPresentationOptions = [.banner, .list, .sound]
    // Our own, with the notice's words.
    guard isPush else { return shown }
    let notice: PushNotice?
    do {
      notice = try await app.news()
    } catch {
      // The server cannot say: the generic words, rather than nothing.
      return shown
    }
    setBadge(app.toReview)
    guard let notice else { return [] }
    let content = UNMutableNotificationContent()
    content.title = notice.title
    content.body = notice.body
    content.sound = .default
    content.userInfo = [Self.pathKey: notice.path]
    // The same identifier as the push's collapse id: the newest replaces.
    try? await UNUserNotificationCenter.current().add(
      UNNotificationRequest(identifier: "organizer", content: content, trigger: nil))
    return []
  }
}
