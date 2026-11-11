import Foundation
import Testing

@testable import OrganizerKit

func subscription(_ id: String, token: String) -> String {
  """
  {"id":"\(id)","kind":"apns","name":"iPhone","endpoint":"\(token)",\
  "created_at":"2026-11-08T10:00:00Z","last_notice_at":null,"available":true}
  """
}

func notice(_ kind: String, title: String, body: String, path: String) -> String {
  """
  {"id":"00000000-0000-0000-0000-00000000000\(kind.count)","kind":"\(kind)","title":"\(title)",\
  "body":"\(body)","path":"\(path)","created_at":"2026-11-08T10:00:00Z"}
  """
}

@MainActor @Suite struct NotificationTests {
  let session = StoredSession(
    server: server, token: "org_token", keyID: "key-1", keyPrefix: "org_tok",
    deviceName: "Anna's iPhone")
  let token = Data([0x00, 0xAB, 0x0F, 0xFF, 0x10])

  func app(
    pushStore: MemoryStore = MemoryStore(),
    _ answer: @escaping @Sendable (StubTransport.Request) throws -> (Int, String)
  ) throws -> AppModel {
    let store = SessionStore(store: MemoryStore())
    try store.save(session)
    return AppModel(
      sessions: store, uploads: UploadQueue(root: temporaryFolder()), pushStore: pushStore,
      makeClient: { server, token, unauthorized in
        Client.organizer(
          server: server, token: token, transport: StubTransport { try answer($0) },
          onUnauthorized: unauthorized)
      }, makeEvents: { _ in AsyncStream { $0.finish() } })
  }

  @Test func theTokenIsLowercaseHex() {
    #expect(PushToken.hex(token) == "00ab0fff10")
    #expect(PushToken.hex(Data()) == "")
  }

  @Test func sendsTheTokenAsAnAPNsSubscription() async throws {
    let log = Log()
    let bodies = Log()
    let pushStore = MemoryStore()
    let app = try app(pushStore: pushStore) { request in
      log.add("\(request.method) \(request.path) \(request.authorization ?? "")")
      bodies.add(compact(request.body))
      return (201, subscription("s1", token: "00ab0fff10"))
    }
    #expect(await app.registerPush(token: token))
    #expect(log.all == ["POST /api/notifications/subscriptions Bearer org_token"])
    let body = try #require(bodies.all.first)
    #expect(body.contains(#""kind":"apns""#))
    #expect(body.contains(#""endpoint":"00ab0fff10""#))
    // Compact drops the spaces of the name too.
    #expect(body.contains(#""name":"Anna'siPhone""#))
    #expect(
      PushRegistration.load(from: pushStore)
        == PushRegistration(id: "s1", token: "00ab0fff10", keyID: "key-1"))
  }

  @Test func aNewTokenReplacesTheOldRow() async throws {
    let log = Log()
    let pushStore = MemoryStore(
      try JSONEncoder().encode(PushRegistration(id: "old", token: "aa", keyID: "key-1")))
    let app = try app(pushStore: pushStore) { request in
      log.add("\(request.method) \(request.path)")
      return request.method == .post ? (201, subscription("new", token: "00ab0fff10")) : (204, "")
    }
    #expect(await app.registerPush(token: token))
    #expect(
      log.all == [
        "POST /api/notifications/subscriptions", "DELETE /api/notifications/subscriptions/old",
      ])
    #expect(PushRegistration.load(from: pushStore)?.id == "new")
  }

  @Test func aRefusedTokenIsNotKept() async throws {
    let pushStore = MemoryStore()
    let app = try app(pushStore: pushStore) { _ in (409, #"{"error":"Too many devices"}"#) }
    #expect(await app.registerPush(token: token) == false)
    #expect(PushRegistration.load(from: pushStore) == nil)
  }

  @Test func signOutRemovesTheSubscriptionBeforeTheToken() async throws {
    let log = Log()
    let pushStore = MemoryStore(
      try JSONEncoder().encode(PushRegistration(id: "s1", token: "00ab", keyID: "key-1")))
    let app = try app(pushStore: pushStore) { request in
      log.add("\(request.method) \(request.path) \(request.authorization ?? "")")
      return (204, "")
    }
    await app.signOut()
    #expect(
      log.all == [
        "DELETE /api/notifications/subscriptions/s1 Bearer org_token",
        "DELETE /api/auth/device Bearer org_token",
      ])
    #expect(PushRegistration.load(from: pushStore) == nil)
    #expect(app.session == nil)
  }

  @Test func signOutStillRevokesWhenTheSubscriptionIsGone() async throws {
    let log = Log()
    let pushStore = MemoryStore(
      try JSONEncoder().encode(PushRegistration(id: "s1", token: "00ab", keyID: "key-1")))
    let app = try app(pushStore: pushStore) { request in
      log.add("\(request.method) \(request.path)")
      return request.path.hasPrefix("/api/notifications")
        ? (404, #"{"error":"No such device"}"#) : (204, "")
    }
    await app.signOut()
    #expect(log.all.last == "DELETE /api/auth/device")
    #expect(PushRegistration.load(from: pushStore) == nil)
    #expect(app.notice == nil)
  }

  @Test func anotherSessionsRegistrationIsNotRemovedWithThisToken() async throws {
    let log = Log()
    let pushStore = MemoryStore(
      try JSONEncoder().encode(PushRegistration(id: "s1", token: "00ab", keyID: "key-0")))
    let app = try app(pushStore: pushStore) { request in
      log.add("\(request.method) \(request.path)")
      return (204, "")
    }
    await app.signOut()
    #expect(log.all == ["DELETE /api/auth/device"])
    #expect(PushRegistration.load(from: pushStore) == nil)
  }

  @Test func newsGivesTheNewestNoticeAndTheCount() async throws {
    let log = Log()
    let app = try app { request in
      log.add("\(request.method) \(request.path)")
      return (
        200,
        """
        {"review":3,"notices":[\(notice("review", title: "Drop box", body: "3 items to review", path: "/drop-box")),\
        \(notice("shared", title: "Shared with you", body: "Something was shared with you", path: "/"))]}
        """
      )
    }
    let first = try await app.news()
    #expect(log.all == ["GET /api/notifications/news"])
    #expect(first == PushNotice(title: "Drop box", body: "3 items to review", path: "/drop-box"))
    #expect(first?.tab == .dropBox)
    #expect(app.toReview == 3)
  }

  @Test func noNewsIsNoNotice() async throws {
    let app = try app { _ in (200, #"{"review":0,"notices":[]}"#) }
    #expect(try await app.news() == nil)
    #expect(app.toReview == 0)
  }

  @Test func newsThatFailsThrowsAndKeepsTheTab() async throws {
    let app = try app { _ in (500, #"{"error":"Something broke"}"#) }
    let error = await #expect(throws: (any Error).self) { try await app.news() }
    #expect(error.flatMap(APIError.of)?.status == 500)
    app.tab = .settings
    await app.open(tab: nil)
    #expect(app.tab == .settings)
  }

  @Test func aTappedPushOpensTheNewestNoticesTab() async throws {
    let app = try app { _ in
      (
        200,
        #"{"review":1,"notices":[\#(notice("review", title: "Drop box", body: "1 item to review", path: "/drop-box"))]}"#
      )
    }
    await app.open(tab: nil)
    #expect(app.tab == .dropBox)
    // Our own notification says its tab: no need to ask.
    await app.open(tab: .settings)
    #expect(app.tab == .settings)
  }

  @Test func pathsOpenTheirTabs() {
    #expect(AppTab(path: "/drop-box") == .dropBox)
    #expect(AppTab(path: "/drop-box/items/1") == .dropBox)
    #expect(AppTab(path: "/settings/backups") == .settings)
    #expect(AppTab(path: "/") == .files)
    #expect(AppTab(path: "") == .files)
    #expect(AppTab(path: "/files/abc") == .files)
  }
}
