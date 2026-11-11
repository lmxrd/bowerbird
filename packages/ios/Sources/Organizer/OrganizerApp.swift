import OrganizerKit
import SwiftUI
import UIKit

/// The one upload queue of the app, which the system may wake for a
/// background session's events before any screen exists; and the app's
/// model, which a push may need before any screen exists too.
enum Shared {
  static let uploads = UploadQueue()
  @MainActor static let app = AppModel(uploads: uploads)
}

final class AppDelegate: NSObject, UIApplicationDelegate {
  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    Shared.uploads.reconnect()
    Staging.sweep()
    // Before launching returns, so that a tap that launched the app arrives.
    Notifications.shared.start()
    return true
  }

  func application(
    _ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    Notifications.shared.registered(token: deviceToken)
  }

  func application(
    _ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error
  ) {
    Notifications.shared.failedToRegister(error)
  }

  func application(
    _ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]
  ) async -> UIBackgroundFetchResult {
    await Notifications.shared.wokenByPush()
  }

  func application(
    _ application: UIApplication, handleEventsForBackgroundURLSession identifier: String,
    completionHandler: @escaping () -> Void
  ) {
    let completion = UncheckedCompletion(completionHandler)
    Shared.uploads.handleEvents(for: identifier) { completion.call() }
  }
}

/// UIKit's completion handler, handed over to the upload queue's thread.
private struct UncheckedCompletion: @unchecked Sendable {
  let call: () -> Void
  init(_ call: @escaping () -> Void) { self.call = call }
}

@main
struct OrganizerApp: App {
  @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
  @State private var app = Shared.app
  @Environment(\.scenePhase) private var scenePhase

  var body: some Scene {
    WindowGroup {
      RootView()
        .environment(app)
    }
    .onChange(of: scenePhase) { _, phase in
      switch phase {
      case .active:
        app.start()
        app.refreshJobs()
      case .background:
        app.stop()
      default:
        break
      }
    }
  }
}

struct RootView: View {
  @Environment(AppModel.self) private var app

  var body: some View {
    Group {
      if app.session == nil {
        SignInView()
      } else {
        MainTabs()
      }
    }
    .onChange(of: app.session?.token) { _, token in
      if token != nil {
        app.start()
      } else {
        Notifications.shared.signedOut()
      }
    }
    // At launch and after a sign-in: the moment to ask for notifications.
    .task(id: app.session?.token) {
      await Notifications.shared.setUp()
    }
    .onChange(of: app.toReview) { _, count in
      Notifications.shared.setBadge(count)
    }
  }
}

struct MainTabs: View {
  @Environment(AppModel.self) private var app

  var body: some View {
    @Bindable var app = app
    TabView(selection: $app.tab) {
      Tab("Files", systemImage: "folder", value: AppTab.files) {
        FilesView()
      }
      Tab("Drop box", systemImage: "tray.and.arrow.down", value: AppTab.dropBox) {
        DropBoxView()
      }
      .badge(app.toReview)
      Tab("Settings", systemImage: "gear", value: AppTab.settings) {
        SettingsView()
      }
    }
  }
}

/// The "something went wrong" alert of a model's error.
extension View {
  func errorAlert(_ error: Binding<String?>) -> some View {
    alert(
      "Something went wrong",
      isPresented: Binding(
        get: { error.wrappedValue != nil }, set: { if !$0 { error.wrappedValue = nil } })
    ) {
      Button("OK", role: .cancel) {}
    } message: {
      Text(error.wrappedValue ?? "")
    }
  }
}
