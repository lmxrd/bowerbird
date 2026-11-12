import Foundation
import Observation

/// The app's state: the session, the API client made from it, the
/// uploads, and the live events while the app is open.
@MainActor
@Observable
public final class AppModel {
  public let sessions: SessionStore
  public let uploads: UploadQueue
  /// The end-to-end encryption's keys on this device.
  public let encryption: Encryption
  /// Bumped on every live event, so that the screens on show refresh.
  public private(set) var changes = 0
  public private(set) var dropBoxChanges = 0
  /// The uploads not yet done, for the progress rows.
  public private(set) var jobs: [UploadJob] = []
  /// The bytes sent of the jobs being sent.
  public private(set) var sent: [UUID: Int64] = [:]
  /// The drop box items waiting for review: the tab's badge.
  public private(set) var toReview = 0
  /// Said once after a sign-out the server did not confirm, or a 401.
  public var notice: String?
  /// The signed-in user's email, from `/api/auth/session`.
  public private(set) var email: String?
  /// The tab on show; a tapped notification opens its notice's.
  public var tab: AppTab = .files

  @ObservationIgnored private let makeClient:
    @Sendable (URL, String?, @escaping @Sendable () -> Void) -> Client
  @ObservationIgnored private let makeEvents: @Sendable (StoredSession) -> AsyncStream<LiveEvent>
  @ObservationIgnored private var events: Task<Void, Never>?
  @ObservationIgnored private var observer: UUID?
  @ObservationIgnored private let pushStore: SecretStore

  public init(
    sessions: SessionStore = SessionStore(),
    uploads: UploadQueue = UploadQueue(),
    pushStore: SecretStore = KeychainStore(account: "push"),
    encryption: Encryption = Encryption(),
    makeClient: @escaping @Sendable (URL, String?, @escaping @Sendable () -> Void) -> Client = {
      Client.organizer(server: $0, token: $1, onUnauthorized: $2)
    },
    makeEvents: @escaping @Sendable (StoredSession) -> AsyncStream<LiveEvent> = {
      EventStream(server: $0.server, token: $0.token).events()
    }
  ) {
    self.sessions = sessions
    self.uploads = uploads
    self.pushStore = pushStore
    self.encryption = encryption
    self.makeClient = makeClient
    self.makeEvents = makeEvents
    observer = uploads.observe { [weak self] event in
      Task { @MainActor in self?.uploadsChanged(event) }
    }
    refreshJobs()
  }

  public var session: StoredSession? { sessions.session }

  /// A client of the signed-in server; a 401 signs the app out.
  public var client: Client? {
    guard let session else { return nil }
    return makeClient(session.server, session.token) { [weak self] in
      Task { @MainActor in self?.signedOutByServer() }
    }
  }

  /// A client without a token, for the sign-in.
  public func anonymousClient(server: URL) -> Client {
    makeClient(server, nil) {}
  }

  public func signedIn(_ session: StoredSession) throws {
    try sessions.save(session)
    notice = nil
    changes += 1
    dropBoxChanges += 1
  }

  /// Signs out: the server revokes the device's token
  /// (`DELETE /api/auth/device`), and it is forgotten here.
  public func signOut() async {
    guard let session, let client else { return }
    // While the token still works: no more notices for this device.
    await unregisterPush()
    do {
      _ = try await client.signOutDevice().noContent
      notice = nil
    } catch {
      // Not reached (offline): the token works until it is revoked.
      notice =
        "Signed out on this iPhone, but the server could not be reached. "
        + "Remove “\(session.deviceName)” on the web under Settings → Devices."
    }
    stop()
    encryption.lock()
    sessions.clear()
    toReview = 0
    email = nil
  }

  private func signedOutByServer() {
    guard session != nil else { return }
    // The token no longer works, so the subscription cannot be removed
    // from here; the app unregisters from APNs instead, and the server
    // drops the row when APNs says the token is gone.
    try? pushStore.delete()
    stop()
    encryption.lock()
    sessions.clear()
    toReview = 0
    email = nil
    notice = "The server signed this iPhone out. Sign in again."
  }

  // MARK: Live events

  /// Follows the live events while the app is in the foreground.
  public func start() {
    guard events == nil, let session else { return }
    let stream = makeEvents(session)
    events = Task { [weak self] in
      for await event in stream {
        guard let self else { return }
        switch event {
        case .changes: self.changes += 1
        case .dropBox:
          self.dropBoxChanges += 1
          await self.refreshBadge()
        }
      }
    }
    Task {
      await refreshAccount()
      await refreshBadge()
      if let client, encryption.unlocked { await encryption.seePending(client) }
    }
  }

  /// Who is signed in, for Settings.
  public func refreshAccount() async {
    guard let client else { return }
    if let current = try? await client.getSession().ok.body.json {
      email = current.user?.email
    }
  }

  public func stop() {
    events?.cancel()
    events = nil
  }

  public func refreshBadge() async {
    guard let client else { return }
    if let items = try? await client.listDropBoxItems(
      query: .init(status: .proposed, limit: 500)
    ).ok.body.json {
      toReview = items.count
    }
  }

  // MARK: Encryption

  /// Unlocks this device's encryption with the passphrase, then sees to
  /// the encrypted folders waiting for a device (new keys, members).
  public func unlock(passphrase: String) async throws {
    guard let client else { return }
    try await encryption.unlock(passphrase: passphrase, client)
    Task { await encryption.seePending(client) }
  }

  // MARK: Notifications

  /// Sends this device's APNs token to the server (kind `apns`, named as
  /// the device), which replaces the row of the same token. Called at every
  /// launch with the token iOS gives, so a new token, or a row removed on
  /// the web, is sent again; an old token's row is removed.
  @discardableResult
  public func registerPush(token: Data) async -> Bool {
    guard let session, let client else { return false }
    let hex = PushToken.hex(token)
    do {
      let subscription = try await client.addPushSubscription(
        body: .json(.init(endpoint: hex, kind: .apns, name: session.deviceName))
      ).created.body.json
      let previous = PushRegistration.load(from: pushStore)
      let registration = PushRegistration(
        id: subscription.id, token: hex, keyID: session.keyID)
      try? pushStore.write(JSONEncoder().encode(registration))
      if let previous, previous.keyID == session.keyID, previous.id != subscription.id {
        _ = try? await client.removePushSubscription(path: .init(id: previous.id)).noContent
      }
      return true
    } catch {
      return false
    }
  }

  /// Removes this device's subscription on the server (at sign-out, or
  /// when notifications are turned off in iOS's Settings).
  public func unregisterPush() async {
    guard let registration = PushRegistration.load(from: pushStore) else { return }
    if let session, session.keyID == registration.keyID, let client {
      // A 404: removed on the web already.
      _ = try? await client.removePushSubscription(path: .init(id: registration.id)).noContent
    }
    try? pushStore.delete()
  }

  /// What is new, after a push: sets the count to review and gives the
  /// newest notice, nil when there is none.
  public func news() async throws -> PushNotice? {
    guard let client else { return nil }
    let news = try await client.getNews().ok.body.json
    toReview = Int(news.review)
    return news.notices.first.map(PushNotice.init)
  }

  /// A tapped notification: its tab, or the newest notice's (a push says
  /// nothing itself). Nothing changes when the server cannot say.
  public func open(tab: AppTab?) async {
    if let tab {
      self.tab = tab
    } else if let notice = try? await news() {
      self.tab = notice.tab
    }
  }

  // MARK: Uploads

  public func refreshJobs() {
    jobs = uploads.jobs()
    sent = Dictionary(
      uniqueKeysWithValues: jobs.compactMap { job in
        uploads.sentBytes(of: job.id).map { (job.id, $0) }
      }
    )
  }

  private func uploadsChanged(_ event: UploadQueue.Event) {
    if case .progress = event {
      // Often: only the numbers, not the jobs from disk.
      for job in jobs { sent[job.id] = uploads.sentBytes(of: job.id) }
      return
    }
    refreshJobs()
    if case .finished(let job) = event {
      if job.folderID == nil {
        dropBoxChanges += 1
      } else {
        changes += 1
      }
    }
  }
}
