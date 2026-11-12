import Foundation
import Testing

@testable import OrganizerKit

@Suite struct PKCETests {
  @Test func followsRFC7636() {
    // The example of RFC 7636, appendix B (the server's test has it too).
    let pkce = PKCE(verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
    #expect(pkce.challenge == "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
  }

  @Test func generatesUnpaddedBase64url() {
    let pkce = PKCE.generate()
    #expect(pkce.verifier.count == 43)
    #expect(pkce.challenge.count == 43)
    let allowed = CharacterSet(
      charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_")
    #expect(pkce.verifier.unicodeScalars.allSatisfy(allowed.contains))
    #expect(PKCE.generate() != pkce)
  }
}

@Suite struct DeviceLoginTests {
  @Test func authorizeURLHasTheRedirectAndChallenge() throws {
    let pkce = PKCE(verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
    let url = DeviceLogin.authorizeURL(
      server: URL(string: "https://files.example.com/org")!, pkce: pkce,
      deviceName: "Anna's iPhone+1")
    let components = try #require(URLComponents(url: url, resolvingAgainstBaseURL: false))
    #expect(components.path == "/org/api/auth/device/authorize")
    let query = Dictionary(
      uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") })
    #expect(query["redirect_uri"] == "organizer://auth")
    #expect(query["code_challenge"] == pkce.challenge)
    #expect(query["code_challenge_method"] == "S256")
    #expect(query["device_name"] == "Anna's iPhone+1")
    #expect(url.absoluteString.contains("%2B1"))
  }

  @Test func takesTheCodeOfTheRedirect() throws {
    #expect(try DeviceLogin.code(from: URL(string: "organizer://auth?code=abc123")!) == "abc123")
    #expect(throws: DeviceLogin.Problem.noCode) {
      try DeviceLogin.code(from: URL(string: "organizer://auth")!)
    }
    #expect(throws: DeviceLogin.Problem.wrongRedirect) {
      try DeviceLogin.code(from: URL(string: "organizer://other?code=abc")!)
    }
    #expect(throws: DeviceLogin.Problem.refused("access_denied")) {
      try DeviceLogin.code(from: URL(string: "organizer://auth?error=access_denied")!)
    }
  }

  @Test func deviceNamesFitTheServer() {
    #expect(DeviceLogin.deviceName("  ") == "iPhone")
    #expect(DeviceLogin.deviceName(String(repeating: "a", count: 150)).count == 100)
  }
}

@Suite struct ServerAddressTests {
  @Test(arguments: [
    ("files.example.com", "https://files.example.com"),
    ("  https://Files.Example.com/  ", "https://files.example.com"),
    ("http://192.168.1.10:8000", "http://192.168.1.10:8000"),
    ("https://example.com/organizer/?x=1#y", "https://example.com/organizer"),
  ])
  func parses(input: String, expected: String) throws {
    #expect(try ServerAddress.parse(input).absoluteString == expected)
  }

  @Test func refusesWhatIsNoAddress() {
    #expect(throws: ServerAddress.Problem.empty) { try ServerAddress.parse("  ") }
    #expect(throws: ServerAddress.Problem.invalid) { try ServerAddress.parse("ftp://example.com") }
    #expect(throws: ServerAddress.Problem.invalid) { try ServerAddress.parse("https://") }
  }
}

@Suite(.serialized) struct SessionStoreTests {
  let session = StoredSession(
    server: URL(string: "https://files.example.com")!, token: "org_abc", keyID: "k1",
    keyPrefix: "org_abc", deviceName: "iPhone")

  @MainActor @Test func savesLoadsAndClears() throws {
    let memory = MemoryStore()
    let store = SessionStore(store: memory)
    #expect(store.session == nil)
    try store.save(session)
    #expect(SessionStore(store: memory).session == session)
    #expect(SessionStore.load(from: memory) == session)
    store.clear()
    #expect(store.session == nil)
    #expect(SessionStore.load(from: memory) == nil)
  }

  /// The real Keychain, in the shared access group (the test host has the
  /// app group's entitlement).
  @MainActor @Test func keepsTheTokenInTheKeychain() throws {
    let keychain = KeychainStore(service: "eu.vardalabs.organizer.tests.\(UUID().uuidString)")
    defer { try? keychain.delete() }
    let store = SessionStore(store: keychain)
    try store.save(session)
    #expect(SessionStore.load(from: keychain) == session)
    var changed = session
    changed.token = "org_def"
    try store.save(changed)
    #expect(SessionStore.load(from: keychain)?.token == "org_def")
    store.clear()
    #expect(try keychain.read() == nil)
  }
}

@MainActor @Suite struct SignInModelTests {
  @Test func exchangesTheCodeForTheToken() async throws {
    let log = Log()
    let app = AppModel(
      sessions: SessionStore(store: MemoryStore()), uploads: UploadQueue(root: temporaryFolder()),
      makeClient: { server, token, unauthorized in
        Client.organizer(
          server: server, token: token,
          transport: StubTransport { request in
            log.add("\(request.method) \(request.path) \(compact(request.body))")
            return (
              200,
              """
              {"token":"org_0123","key":{"id":"key-1","kind":"device","name":"Anna's iPhone","prefix":"org_0123",\
              "scope":"write","created_at":"2026-11-07T10:00:00Z","last_used_at":null,"expires_at":null}}
              """
            )
          }, onUnauthorized: unauthorized)
      }, makeEvents: { _ in AsyncStream { $0.finish() } })
    let model = SignInModel(address: "files.example.com")
    let attempt = try #require(model.begin(deviceName: "Anna's iPhone"))
    #expect(attempt.server.absoluteString == "https://files.example.com")
    await model.finish(attempt, callback: URL(string: "organizer://auth?code=one-time")!, app: app)
    #expect(model.error == nil)
    #expect(app.session?.token == "org_0123")
    #expect(app.session?.keyID == "key-1")
    let request = try #require(log.all.first)
    #expect(request.hasPrefix("POST /api/auth/device/token"))
    #expect(request.contains("\"code\":\"one-time\""))
    #expect(request.contains("\"code_verifier\":\"\(attempt.pkce.verifier)\""))
  }

  @Test func saysWhatTheServerRefused() async throws {
    let app = AppModel(
      sessions: SessionStore(store: MemoryStore()), uploads: UploadQueue(root: temporaryFolder()),
      makeClient: { server, token, unauthorized in
        Client.organizer(
          server: server, token: token,
          transport: StubTransport { _ in (400, #"{"error":"Invalid or expired code"}"#) },
          onUnauthorized: unauthorized)
      }, makeEvents: { _ in AsyncStream { $0.finish() } })
    let model = SignInModel(address: "files.example.com")
    let attempt = try #require(model.begin(deviceName: "iPhone"))
    await model.finish(attempt, callback: URL(string: "organizer://auth?code=old")!, app: app)
    #expect(model.error == "Invalid or expired code")
    #expect(app.session == nil)
  }

  @Test func refusesABadAddress() {
    let model = SignInModel(address: "ftp://nope")
    #expect(model.begin(deviceName: "iPhone") == nil)
    #expect(model.error != nil)
  }
}

func temporaryFolder() -> URL {
  let url = FileManager.default.temporaryDirectory.appending(
    path: "organizer-tests-\(UUID().uuidString)")
  try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
  return url
}
