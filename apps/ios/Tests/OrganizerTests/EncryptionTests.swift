import CryptoKit
import Foundation
import Testing

@testable import OrganizerKit

/// The format's test vectors (packages/e2e-crypto/vectors.json, written by
/// the reference implementation; docs/E2E_SPEC.md §9), copied into this
/// bundle by project.yml. Bytes are hex.
struct Vectors: Decodable {
  struct Identity: Decodable {
    let `private`: String
    let `public`: String
  }
  struct Passphrase: Decodable {
    let passphrase: String
    let salt: String
    let iterations: Int
    let key: String
    let `private`: String
    let nonce: String
    let wrapped: String
  }
  struct Recovery: Decodable {
    let key: String
    let text: String
    let wrappingKey: String
    let `private`: String
    let nonce: String
    let wrapped: String
  }
  struct FolderKey: Decodable {
    let folderId: String
    let generation: UInt32
    let folderKey: String
    let recipientPrivate: String
    let recipientPublic: String
    let ephemeralPrivate: String
    let ephemeralPublic: String
    let nonce: String
    let wrapped: String
    /// `wrapped_base64url`, as `convertFromSnakeCase` spells it.
    let wrappedBase64Url: String
  }
  struct Name: Decodable {
    let folderKey: String
    let generation: UInt32
    let parentId: String
    let name: String
    let decrypted: String
    let nonce: String
    let nameKey: String
    let encrypted: String
  }
  struct Contents: Decodable {
    let folderKey: String
    let generation: UInt32
    let fileNonce: String
    let fileKey: String
    let plainLength: Int
    let encryptedLength: Int
    let encryptedSha256: String
    let encrypted: String?
  }

  let formatVersion: Int
  let identities: [Identity]
  let passphrases: [Passphrase]
  let recoveryKeys: [Recovery]
  let folderKeys: [FolderKey]
  let names: [Name]
  let contents: [Contents]

  static func load() throws -> Vectors {
    let url = try #require(
      Bundle(for: BundleMarker.self).url(forResource: "vectors", withExtension: "json"))
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(Vectors.self, from: Data(contentsOf: url))
  }
}

private final class BundleMarker {}

func hex(_ text: String) -> Data {
  var data = Data()
  var index = text.startIndex
  while index < text.endIndex {
    let next = text.index(index, offsetBy: 2)
    data.append(UInt8(text[index..<next], radix: 16)!)
    index = next
  }
  return data
}

func hex(_ data: Data) -> String {
  data.map { String(format: "%02x", $0) }.joined()
}

func pattern(_ length: Int) -> Data {
  Data((0..<length).map { UInt8($0 % 251) })
}

@Suite struct E2EVectorTests {
  let vectors: Vectors

  init() throws { vectors = try Vectors.load() }

  @Test func theFormatIsVersionOne() {
    #expect(vectors.formatVersion == Int(E2E.version))
  }

  @Test func identities() throws {
    #expect(!vectors.identities.isEmpty)
    for identity in vectors.identities {
      #expect(try E2E.publicKey(of: hex(identity.private)) == hex(identity.public))
    }
  }

  @Test func passphrases() throws {
    #expect(!vectors.passphrases.isEmpty)
    for vector in vectors.passphrases {
      let key = try E2E.passphraseKey(
        vector.passphrase, salt: hex(vector.salt), iterations: vector.iterations)
      #expect(key == hex(vector.key), "\(vector.passphrase)")
      let wrapped = try E2E.wrapPrivate(hex(vector.private), with: key, nonce: hex(vector.nonce))
      #expect(wrapped == hex(vector.wrapped))
      #expect(
        try E2E.unwrapPrivate(
          hex(vector.wrapped), passphrase: vector.passphrase, salt: hex(vector.salt),
          iterations: vector.iterations) == hex(vector.private))
      #expect(throws: E2EError.decrypt) {
        try E2E.unwrapPrivate(
          hex(vector.wrapped), passphrase: vector.passphrase + "!", salt: hex(vector.salt),
          iterations: vector.iterations)
      }
      // Too few iterations are refused before anything is derived.
      #expect(throws: E2EError.self) {
        try E2E.unwrapPrivate(
          hex(vector.wrapped), passphrase: vector.passphrase, salt: hex(vector.salt),
          iterations: E2E.minIterations - 1)
      }
    }
    // One is decomposed (NFD): the same passphrase in either form is the
    // same key.
    #expect(
      vectors.passphrases.contains {
        $0.passphrase.unicodeScalars.count
          != $0.passphrase.precomposedStringWithCanonicalMapping.unicodeScalars.count
      })
    for vector in vectors.passphrases {
      for form in [
        vector.passphrase.precomposedStringWithCanonicalMapping,
        vector.passphrase.decomposedStringWithCanonicalMapping,
      ] {
        #expect(
          try E2E.passphraseKey(form, salt: hex(vector.salt), iterations: vector.iterations)
            == hex(vector.key))
      }
    }
  }

  @Test func recoveryKeys() throws {
    #expect(!vectors.recoveryKeys.isEmpty)
    for vector in vectors.recoveryKeys {
      #expect(E2E.encodeRecovery(hex(vector.key)) == vector.text)
      #expect(try E2E.decodeRecovery(vector.text) == hex(vector.key))
      let typed = vector.text.lowercased().replacingOccurrences(of: "-", with: " ")
      #expect(try E2E.decodeRecovery(" \(typed)\n") == hex(vector.key))
      #expect(E2E.recoveryWrappingKey(hex(vector.key)) == hex(vector.wrappingKey))
      #expect(
        try E2E.wrapPrivate(
          hex(vector.private), with: hex(vector.wrappingKey), nonce: hex(vector.nonce))
          == hex(vector.wrapped))
      #expect(
        try E2E.unwrapPrivate(hex(vector.wrapped), with: hex(vector.wrappingKey))
          == hex(vector.private))
      // Another length, another character, unused bits that are not zero.
      for typo in [
        String(vector.text.dropLast()), "1" + vector.text.dropFirst(),
        String(vector.text.dropLast()) + (vector.text.hasSuffix("A") ? "B" : "R"),
        vector.text + "A",
      ] {
        #expect(throws: E2EError.self, "\(typo)") { try E2E.decodeRecovery(typo) }
      }
    }
  }

  @Test func folderKeys() throws {
    #expect(!vectors.folderKeys.isEmpty)
    for vector in vectors.folderKeys {
      let folder = try #require(UUID(uuidString: vector.folderId))
      #expect(try E2E.publicKey(of: hex(vector.ephemeralPrivate)) == hex(vector.ephemeralPublic))
      #expect(try E2E.publicKey(of: hex(vector.recipientPrivate)) == hex(vector.recipientPublic))
      let wrapped = try E2E.wrapFolderKey(
        hex(vector.folderKey), folder: folder, generation: vector.generation,
        for: hex(vector.recipientPublic), ephemeral: hex(vector.ephemeralPrivate),
        nonce: hex(vector.nonce))
      #expect(wrapped == hex(vector.wrapped))
      #expect(E2E.base64url(wrapped) == vector.wrappedBase64Url)
      #expect(try E2E.unbase64url(vector.wrappedBase64Url) == wrapped)
      #expect(
        try E2E.unwrapFolderKey(
          wrapped, folder: folder, generation: vector.generation,
          privateKey: hex(vector.recipientPrivate)) == hex(vector.folderKey))
      #expect(throws: E2EError.decrypt) {
        try E2E.unwrapFolderKey(
          wrapped, folder: folder, generation: vector.generation + 1,
          privateKey: hex(vector.recipientPrivate))
      }
      #expect(throws: E2EError.decrypt) {
        try E2E.unwrapFolderKey(
          wrapped, folder: UUID(), generation: vector.generation,
          privateKey: hex(vector.recipientPrivate))
      }
      // A fresh wrapping (random ephemeral key and nonce) opens too.
      let fresh = try E2E.wrapFolderKey(
        hex(vector.folderKey), folder: folder, generation: vector.generation,
        for: hex(vector.recipientPublic))
      #expect(fresh.count == E2E.wrappedFolderKeyLength)
      #expect(
        try E2E.unwrapFolderKey(
          fresh, folder: folder, generation: vector.generation,
          privateKey: hex(vector.recipientPrivate)) == hex(vector.folderKey))
    }
  }

  @Test func names() throws {
    #expect(!vectors.names.isEmpty)
    for vector in vectors.names {
      let parent = try #require(UUID(uuidString: vector.parentId))
      #expect(E2E.nameKey(hex(vector.folderKey)) == hex(vector.nameKey))
      let encrypted = try E2E.encryptName(
        vector.name, folderKey: hex(vector.folderKey), generation: vector.generation,
        parent: parent, nonce: hex(vector.nonce))
      #expect(encrypted == vector.encrypted, "\(vector.name)")
      #expect(try E2E.nameGeneration(vector.encrypted) == vector.generation)
      let decrypted = try E2E.decryptName(
        vector.encrypted, folderKey: hex(vector.folderKey), parent: parent)
      #expect(decrypted == vector.decrypted)
      #expect(Array(decrypted.utf8) == Array(vector.decrypted.utf8))
      #expect(throws: E2EError.decrypt) {
        try E2E.decryptName(vector.encrypted, folderKey: hex(vector.folderKey), parent: UUID())
      }
    }
    let key = hex(vectors.names[0].folderKey)
    #expect(throws: E2EError.self) {
      try E2E.encryptName(
        String(repeating: "a", count: 159), folderKey: key, generation: 1, parent: UUID())
    }
    #expect(throws: E2EError.self) {
      try E2E.encryptName("", folderKey: key, generation: 1, parent: UUID())
    }
  }

  @Test func contents() throws {
    #expect(vectors.contents.count >= 4)
    for vector in vectors.contents {
      let plain = pattern(vector.plainLength)
      let key = hex(vector.folderKey)
      #expect(E2E.fileKey(key, fileNonce: hex(vector.fileNonce)) == hex(vector.fileKey))
      let encrypted = try E2E.encrypt(
        plain, folderKey: key, generation: vector.generation, fileNonce: hex(vector.fileNonce))
      #expect(encrypted.count == vector.encryptedLength, "\(vector.plainLength)")
      #expect(hex(Data(SHA256.hash(data: encrypted))) == vector.encryptedSha256)
      if let whole = vector.encrypted { #expect(encrypted == hex(whole)) }
      #expect(E2E.encryptedSize(Int64(vector.plainLength)) == Int64(vector.encryptedLength))
      #expect(try E2E.plainSize(Int64(vector.encryptedLength)) == Int64(vector.plainLength))

      let keys: (UInt32) -> Data? = { $0 == vector.generation ? key : nil }
      #expect(try E2E.decrypt(encrypted, keys: keys) == plain)
      #expect(throws: E2EError.noKey(vector.generation)) {
        try E2E.decrypt(encrypted, keys: { _ in nil })
      }
      // Cut: the last byte gone, or (with several segments) everything
      // after the first, which was not the last.
      #expect(throws: E2EError.self) { try E2E.decrypt(encrypted.dropLast(), keys: keys) }
      if vector.plainLength > E2E.segment {
        let first = encrypted.prefix(E2E.headerLength + E2E.sealedSegment)
        #expect(throws: E2EError.decrypt) { try E2E.decrypt(first, keys: keys) }
      }
      var changed = encrypted
      changed[E2E.headerLength + 3] ^= 1
      #expect(throws: E2EError.decrypt) { try E2E.decrypt(changed, keys: keys) }

      // The same through files, a segment at a time.
      let folder = temporaryFolder()
      let source = folder.appending(path: "plain")
      try plain.write(to: source)
      let sealed = folder.appending(path: "sealed")
      let written = try E2E.encryptFile(
        at: source, to: sealed, folderKey: key, generation: vector.generation,
        fileNonce: hex(vector.fileNonce))
      #expect(written == Int64(vector.encryptedLength))
      #expect(try Data(contentsOf: sealed) == encrypted)
      let opened = folder.appending(path: "opened")
      #expect(try E2E.decryptFile(at: sealed, to: opened, keys: keys) == Int64(plain.count))
      #expect(try Data(contentsOf: opened) == plain)
      // A failure leaves nothing at the destination.
      try encrypted.dropLast().write(to: sealed)
      let failed = folder.appending(path: "failed")
      #expect(throws: E2EError.self) { try E2E.decryptFile(at: sealed, to: failed, keys: keys) }
      #expect(!FileManager.default.fileExists(atPath: failed.path))
      #expect(try FileManager.default.contentsOfDirectory(atPath: folder.path).count == 3)
    }
  }

  @Test func base64urlIsStrict() throws {
    #expect(E2E.base64url(Data([0xfb, 0xff])) == "-_8")
    #expect(try E2E.unbase64url("-_8") == Data([0xfb, 0xff]))
    for bad in ["+/8", "-_8=", "a", "ab c"] {
      #expect(throws: E2EError.self, "\(bad)") { try E2E.unbase64url(bad) }
    }
  }
}

// MARK: The view models in an encrypted folder

/// A member of an encrypted folder, with its folder key of generation 1,
/// as the server would answer for it.
struct EncryptedFolderFixture {
  let top = "0b6a3b9e-61f4-4f1c-9a53-2a4d6a1f0c11"
  let userID = "6f0e8d0c-2b8a-4a54-8a4f-6a3f6a0d1b22"
  let privateKey = E2E.random(32)
  let folderKey = E2E.random(32)
  let salt = E2E.random(16)
  let passphrase = "correct horse"

  var publicKey: Data { try! E2E.publicKey(of: privateKey) }

  var encryptionJSON: String {
    let key = try! E2E.passphraseKey(passphrase, salt: salt, iterations: E2E.minIterations)
    let wrapped = try! E2E.wrapPrivate(privateKey, with: key, nonce: E2E.random(12))
    return """
      {"set_up":true,"public_key":"\(E2E.base64url(publicKey))",\
      "passphrase":{"salt":"\(E2E.base64url(salt))","iterations":\(E2E.minIterations),\
      "wrapped":"\(E2E.base64url(wrapped))"},"recovery_wrapped":"x"}
      """
  }

  /// The folder's keys: generation 1 wrapped for this member, or none yet
  /// (a new folder, which needs its first key).
  func keysJSON(generations: [UInt32: Data]? = nil, needsNewKey: Bool = false) -> String {
    let generations = generations ?? [1: folderKey]
    let keys = generations.sorted { $0.key < $1.key }.map { generation, key in
      let wrapped = try! E2E.wrapFolderKey(
        key, folder: UUID(uuidString: top)!, generation: generation, for: publicKey)
      return #"{"generation":\#(generation),"wrapped":"\#(E2E.base64url(wrapped))"}"#
    }
    return """
      {"folder_id":"\(top)","generation":\(generations.keys.max() ?? 0),\
      "needs_new_key":\(needsNewKey),"keys":[\(keys.joined(separator: ","))],\
      "members":[{"user_id":"\(userID)","email":"anna@example.com",\
      "public_key":"\(E2E.base64url(publicKey))","missing":[]}]}
      """
  }

  func sealed(_ name: String, in parent: String? = nil) -> String {
    try! E2E.encryptName(
      name, folderKey: folderKey, generation: 1, parent: UUID(uuidString: parent ?? top)!)
  }

  func node(_ id: String, _ name: String, folder: Bool = false, size: Int64 = 12) -> String {
    """
    {"id":"\(id)","drive_id":"d1","parent_id":"\(top)","name":"\(name)","kind":"\(folder ? "folder" : "file")",\
    "size":\(folder ? 0 : size),"mime":null,"version":1,"portable":true,\
    "created_at":"2026-11-08T10:00:00Z","updated_at":"2026-11-08T10:00:00Z","e2e_root":"\(top)",\
    "my_role":"owner"}
    """
  }

  var childrenJSON: String {
    let items = [
      node("1", sealed("zebra.txt"), size: E2E.encryptedSize(5)),
      node("2", sealed("Archive"), folder: true),
      node("3", sealed("apple.txt"), size: E2E.encryptedSize(70_000)),
      node("4", "AQAAAAEnotreallyencrypted0000000000000000000000000"),
    ]
    return #"{"items":[\#(items.joined(separator: ","))],"next":null,"mounts":[],"shared":[]}"#
  }
}

/// Answers of a stub server that change as the test goes.
final class Answers: @unchecked Sendable {
  private let lock = NSLock()
  private var values: [String: String] = [:]
  subscript(key: String) -> String? {
    get { lock.withLock { values[key] } }
    set { lock.withLock { values[key] = newValue } }
  }
}

@MainActor @Suite struct EncryptedFolderTests {
  let fixture = EncryptedFolderFixture()

  @Test func lockedShowsNothingThenUnlockedDecryptsAndSorts() async throws {
    let log = Log()
    let fixture = self.fixture
    let client = client(log) { request in
      switch request.path {
      case "/api/encryption": (200, fixture.encryptionJSON)
      case "/api/encryption/folders/\(fixture.top)": (200, fixture.keysJSON())
      case "/api/files/\(fixture.top)/children": (200, fixture.childrenJSON)
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let encryption = Encryption(store: MemoryStore())
    let model = FolderModel(
      folderID: fixture.top, title: "Private", e2eRoot: fixture.top, encryption: encryption)
    #expect(model.locked)
    await model.load(client)
    #expect(model.loaded && model.nodes.isEmpty && model.error == nil)
    #expect(log.all.isEmpty, "Nothing is asked while locked")

    await #expect(throws: EncryptionError.wrongPassphrase) {
      try await encryption.unlock(passphrase: "wrong", client)
    }
    #expect(!encryption.unlocked)
    try await encryption.unlock(passphrase: fixture.passphrase, client)
    #expect(encryption.unlocked && !model.locked)

    await model.load(client)
    #expect(model.error == nil)
    #expect(model.nodes.map(\.name) == ["Archive", "apple.txt", "Encrypted item", "zebra.txt"])
    #expect(model.undecryptable == ["4"])
    #expect(model.nodes.first { $0.id == "3" }?.size == 70_000)
    #expect(model.nodes.first { $0.id == "1" }?.size == 5)
    #expect(model.keys?.generation == 1)

    // Lock forgets the key.
    encryption.lock()
    #expect(model.locked)
  }

  @Test func theKeyStaysInTheStoreForTheShareExtension() async throws {
    let fixture = self.fixture
    let store = MemoryStore()
    let encryption = Encryption(store: store)
    try await encryption.unlock(
      passphrase: fixture.passphrase,
      client { _ in (200, fixture.encryptionJSON) })
    #expect(try store.read() == fixture.privateKey)
    // Another process (the share extension) finds it unlocked.
    #expect(Encryption(store: store).unlocked)
    encryption.lock()
    #expect(try store.read() == nil)
  }

  /// In a mirror (docs/M7D_DESIGN.md §9) the copied encrypted folder has
  /// no keys of its own: its keys are asked by the folder it copies
  /// (`source_id`), and its names, sealed for the source's folders, open
  /// with their ids.
  @Test func aMirrorsEncryptedFolderOpensWithItsSourcesKeys() async throws {
    let fixture = self.fixture
    let mirrorTop = "7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d"
    let sourceArchive = "5a4b3c2d-1e0f-4a9b-8c7d-6e5f4a3b2c1d"
    let mirrorArchive = "1f2e3d4c-5b6a-4987-8a6b-5c4d3e2f1a0b"
    func item(_ id: String, _ name: String, folder: Bool = false, source: String) -> String {
      """
      {"id":"\(id)","drive_id":"m1","parent_id":"\(mirrorTop)","name":"\(name)",\
      "kind":"\(folder ? "folder" : "file")","size":\(folder ? 0 : E2E.encryptedSize(5)),\
      "mime":null,"version":1,"portable":true,"created_at":"2026-11-08T10:00:00Z",\
      "updated_at":"2026-11-08T10:00:00Z","e2e_root":"\(mirrorTop)","source_id":"\(source)",\
      "my_role":"viewer"}
      """
    }
    let top = [
      item("m-a", fixture.sealed("notes.txt"), source: "s-a"),
      item(mirrorArchive, fixture.sealed("Archive"), folder: true, source: sourceArchive),
    ]
    let inArchive = [item("m-b", fixture.sealed("old.txt", in: sourceArchive), source: "s-b")]
    func page(_ items: [String]) -> String {
      #"{"items":[\#(items.joined(separator: ","))],"next":null,"mounts":[],"shared":[]}"#
    }
    let (topPage, archivePage) = (page(top), page(inArchive))
    let log = Log()
    let client = client(log) { request in
      switch request.path {
      case "/api/encryption/folders/\(fixture.top)": (200, fixture.keysJSON())
      case "/api/encryption/folders/\(sourceArchive)": (200, fixture.keysJSON())
      case "/api/files/\(mirrorTop)/children": (200, topPage)
      case "/api/files/\(mirrorArchive)/children": (200, archivePage)
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let encryption = Encryption(store: MemoryStore(fixture.privateKey))
    let mirror = DrivePlace(
      id: "m1", name: "Family · mirror", rootID: "m1r", role: .viewer, isMirror: true)
    let model = FolderModel(
      FolderChoice(
        id: mirrorTop, name: "Private", e2eRoot: mirrorTop, drive: mirror, role: .viewer,
        sourceID: fixture.top), encryption: encryption)
    await model.load(client)
    #expect(model.error == nil)
    #expect(model.nodes.map(\.name) == ["Archive", "notes.txt"])
    #expect(model.undecryptable.isEmpty)
    #expect(model.actions == [.open])
    #expect(log.all.contains("GET /api/encryption/folders/\(fixture.top)"))
    #expect(!log.all.contains("GET /api/encryption/folders/\(mirrorTop)"))

    // A folder in it opens with its own source's id.
    let archive = try #require(model.nodes.first { $0.isFolder })
    let inside = FolderModel(model.choice(archive), encryption: encryption)
    #expect(inside.sourceID == sourceArchive)
    await inside.load(client)
    #expect(inside.error == nil)
    #expect(inside.nodes.map(\.name) == ["old.txt"])
  }

  @Test func namesAreSealedForTheirParentAndClashesRefused() async throws {
    let fixture = self.fixture
    let bodies = Log()
    let client = Client.organizer(
      server: server, token: "org_token",
      transport: StubTransport { request in
        if request.method == .post, let body = request.body { bodies.add(compact(body)) }
        switch (request.method, request.path) {
        case (.get, "/api/encryption/folders/\(fixture.top)"): return (200, fixture.keysJSON())
        case (.get, "/api/files/\(fixture.top)/children"): return (200, fixture.childrenJSON)
        case (.post, "/api/files/\(fixture.top)/folders"):
          return (201, fixture.node("5", "x", folder: true))
        default: return (404, #"{"error":"Not found"}"#)
        }
      })
    let store = MemoryStore(fixture.privateKey)
    let encryption = Encryption(store: store)
    let model = FolderModel(
      folderID: fixture.top, title: "Private", e2eRoot: fixture.top, encryption: encryption)
    await model.load(client)

    #expect(await model.createFolder(named: "Taxes", client))
    let body = try #require(bodies.all.first)
    let sealed = try #require(
      body.split(separator: "\"").drop { $0 != "name" }.dropFirst(2).first.map(String.init))
    #expect(
      try E2E.decryptName(
        sealed, folderKey: fixture.folderKey, parent: UUID(uuidString: fixture.top)!) == "Taxes")

    // "Apple.txt" is there already, as far as the device can tell.
    #expect(!(await model.createFolder(named: "APPLE.TXT", client)))
    #expect(model.error == "APPLE.TXT already exists")
    #expect(bodies.all.count == 1)
  }

  @Test func aNewFolderGetsItsFirstKeyFromThisDevice() async throws {
    let fixture = self.fixture
    let answers = Answers()
    answers["keys"] = fixture.keysJSON(generations: [:], needsNewKey: true)
    let posted = Log()
    let client = Client.organizer(
      server: server, token: "org_token",
      transport: StubTransport { request in
        switch (request.method, request.path) {
        case (.get, "/api/encryption/folders/\(fixture.top)"): return (200, answers["keys"]!)
        case (.post, "/api/encryption/folders/\(fixture.top)/keys"):
          let body = try JSONDecoder().decode(
            Components.Schemas.NewFolderKeys.self, from: request.body ?? Data())
          posted.add(compact(request.body))
          // The server keeps it: the member's key of generation 1.
          let key = try E2E.unwrapFolderKey(
            E2E.unbase64url(body.keys[0].wrapped), folder: UUID(uuidString: fixture.top)!,
            generation: 1, privateKey: fixture.privateKey)
          answers["keys"] = fixture.keysJSON(generations: [1: key])
          return (200, answers["keys"]!)
        case (.get, "/api/files/\(fixture.top)/children"):
          return (200, #"{"items":[],"next":null,"mounts":[],"shared":[]}"#)
        default: return (404, #"{"error":"Not found"}"#)
        }
      })
    let encryption = Encryption(store: MemoryStore(fixture.privateKey))
    let keys = try await encryption.folderKeys(for: fixture.top, root: fixture.top, client)
    #expect(keys.generation == 1)
    #expect(keys.key(1) != nil)
    #expect(posted.all.count == 1)
    #expect(posted.all[0].contains(#""generation":1"#))
    #expect(posted.all[0].contains(fixture.userID))
  }

  @Test func theShareSheetRefusesALockedFolderAndEncryptsForAnUnlockedOne() async throws {
    let fixture = self.fixture
    let locked = ShareModel(
      files: [SharedFile(url: URL(filePath: "/tmp/a"), name: "a.pdf", size: 1)], link: nil,
      encryption: Encryption(store: MemoryStore()))
    locked.mode = .place
    locked.folder = FolderChoice(id: fixture.top, name: "Private", e2eRoot: fixture.top)
    #expect(locked.folderLocked && !locked.canSend)

    FakeServer.current = FakeServer()
    let queue = makeQueue()
    let source = try file(10)
    let client = client { request in
      switch request.path {
      case "/api/encryption/folders/\(fixture.top)": (200, fixture.keysJSON())
      case "/api/files/\(fixture.top)/children": (200, fixture.childrenJSON)
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let model = ShareModel(
      files: [SharedFile(url: source, name: "bill.pdf", size: 10)], link: nil,
      encryption: Encryption(store: MemoryStore(fixture.privateKey)))
    model.mode = .place
    model.folder = FolderChoice(id: fixture.top, name: "Private", e2eRoot: fixture.top)
    #expect(model.canSend)
    #expect(await model.send(queue, client: client), "\(model.error ?? "")")
    try await settle(queue)

    // One multipart POST, whose file is the encrypted name and contents.
    let post = try #require(FakeServer.current.posts.first)
    #expect(post.path == "/api/files/\(fixture.top)/files")
    let body = post.body
    let text = String(decoding: body, as: UTF8.self)
    let name = try #require(
      text.components(separatedBy: "filename=\"").dropFirst().first?
        .components(separatedBy: "\"").first)
    #expect(
      try E2E.decryptName(
        name, folderKey: fixture.folderKey, parent: UUID(uuidString: fixture.top)!)
        == "bill.pdf")
    let start = try #require(body.range(of: Data("\r\n\r\n".utf8))).upperBound
    let encrypted = body.subdata(in: start..<start + Int(E2E.encryptedSize(10)))
    #expect(try E2E.decrypt(encrypted, keys: { $0 == 1 ? fixture.folderKey : nil }) == pattern(10))
    #expect(!FileManager.default.fileExists(atPath: source.path))

    // A name that is there already is refused before anything is sent.
    let again = ShareModel(
      files: [SharedFile(url: try file(3), name: "Zebra.txt", size: 3)], link: nil,
      encryption: Encryption(store: MemoryStore(fixture.privateKey)))
    again.mode = .place
    again.folder = model.folder
    #expect(!(await again.send(queue, client: client)))
    #expect(again.error == "Zebra.txt already exists")
  }
}

/// Encrypted folders shared between Organizer servers (`organizer-e2e`,
/// docs/E2E_SPEC.md §12).
@MainActor @Suite struct RemoteEncryptedFolderTests {
  let fixture = EncryptedFolderFixture()

  func keysInfo(_ json: String) throws -> Components.Schemas.FolderKeys {
    try JSONDecoder().decode(Components.Schemas.FolderKeys.self, from: Data(json.utf8))
  }

  /// A member: on another server with an address (`remote`), whose
  /// `user_id` is the share's id.
  func member(_ id: String, _ publicKey: Data?, missing: [Int64], remote: String?) -> String {
    """
    {"user_id":"\(id)","email":"\(remote ?? "bea@example.com")",\
    "public_key":\(publicKey.map { "\"\(E2E.base64url($0))\"" } ?? "null"),\
    "missing":\(missing),"remote":\(remote.map { "\"\($0)\"" } ?? "null")}
    """
  }

  func info(generation: Int, members: [String]) -> String {
    """
    {"folder_id":"\(fixture.top)","generation":\(generation),"needs_new_key":false,"keys":[],\
    "members":[\(members.joined(separator: ","))]}
    """
  }

  @Test func theOwnerWrapsForAMemberOnAnotherServerByTheSharesID() throws {
    let theirs = E2E.random(32)
    let share = "9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c6d"
    let data = try keysInfo(
      info(
        generation: 2,
        members: [
          member(share, try E2E.publicKey(of: theirs), missing: [1, 2], remote: "bob@b.example"),
          member(UUID().uuidString, nil, missing: [1], remote: "eve@c.example"),
        ]))
    let held = FolderKeySet(folderID: fixture.top, generation: 2, keys: [1: fixture.folderKey])
    let keys = Encryption.memberKeys(data, held: held, fresh: nil)
    #expect(keys.map(\.userId) == [share])
    #expect(keys.map(\.generation) == [1])
    let opened = try E2E.unwrapFolderKey(
      E2E.unbase64url(keys[0].wrapped), folder: UUID(uuidString: fixture.top)!, generation: 1,
      privateKey: theirs)
    #expect(opened == fixture.folderKey)
  }

  @Test func aNewGenerationIncludesTheMembersOnOtherServersWithAKey() throws {
    let theirs = E2E.random(32)
    let (local, remote) = (UUID().uuidString, UUID().uuidString)
    let data = try keysInfo(
      info(
        generation: 2,
        members: [
          member(local, fixture.publicKey, missing: [], remote: nil),
          member(remote, try E2E.publicKey(of: theirs), missing: [], remote: "bob@b.example"),
          member(UUID().uuidString, nil, missing: [], remote: "eve@c.example"),
        ]))
    let fresh = E2E.random(32)
    let keys = Encryption.memberKeys(
      data, held: FolderKeySet(folderID: fixture.top, generation: 2, keys: [:]), fresh: fresh)
    #expect(keys.map(\.userId) == [local, remote])
    #expect(keys.allSatisfy { $0.generation == 3 })
    let opened = try E2E.unwrapFolderKey(
      E2E.unbase64url(keys[1].wrapped), folder: UUID(uuidString: fixture.top)!, generation: 3,
      privateKey: theirs)
    #expect(opened == fresh)
  }

  /// The member's device: a drive from another server whose top is the
  /// encrypted folder. Its keys are asked by its id here and unwrapped with
  /// the owner's folder (`folder_id`); names open, and new ones are sealed,
  /// with the parent's `source_id`; a folder without one yet is listed
  /// again first, and nothing is sealed under it while it has none.
  @Test func aMemberOpensAndWritesWithTheOwnersIDs() async throws {
    let fixture = self.fixture
    let here = "3c2b1a09-8f7e-4d6c-9b5a-4c3d2e1f0a9b"
    let made = "4d3c2b1a-0f9e-4d7c-8b6a-5d4c3e2f1a0b"
    let theirsMade = "5e4d3c2b-1a0f-4e8d-9c7b-6e5d4f3a2b1c"
    let page =
      #"{"items":[\#(fixture.node("1", fixture.sealed("notes.txt")))],"#
      + #""next":null,"mounts":[],"shared":[]}"#
    let empty = #"{"items":[],"next":null,"mounts":[],"shared":[]}"#
    let answers = Answers()
    answers["source"] = "null"
    let posted = Log()
    let client = Client.organizer(
      server: server, token: "org_token",
      transport: StubTransport { request in
        if request.method == .post { posted.add("\(request.path) \(compact(request.body))") }
        switch (request.method, request.path) {
        // Asked by its id here, or by the owner's (`source_id`).
        case (.get, "/api/encryption/folders/\(here)"), (.get, "/api/encryption/folders/\(made)"),
          (.get, "/api/encryption/folders/\(fixture.top)"):
          return (200, fixture.keysJSON())
        case (.get, "/api/files/\(here)/children"): return (200, page)
        case (.get, "/api/files/\(made)/children"): return (200, empty)
        case (.get, "/api/files/\(made)"):
          return (
            200,
            #"{"id":"\#(made)","drive_id":"r1","parent_id":"\#(here)","name":"x","kind":"folder","#
              + #""size":0,"mime":null,"version":1,"portable":true,"#
              + #""created_at":"2026-11-10T10:00:00Z","updated_at":"2026-11-10T10:00:00Z","#
              + #""e2e_root":"\#(here)","source_id":\#(answers["source"]!),"my_role":"editor","#
              + #""remote":"b.example","path":"/x","permissions":{"own":false,"mount":false,"create":true,"change":true,"delete":true,"share":false}}"#
          )
        case (.post, "/api/files/\(here)/folders"), (.post, "/api/files/\(made)/folders"):
          return (201, fixture.node("5", "x", folder: true))
        default: return (404, #"{"error":"Not found"}"#)
        }
      })
    let encryption = Encryption(store: MemoryStore(fixture.privateKey))
    let drive = DrivePlace(id: "r1", name: "Private", rootID: here, role: .editor, isRemote: true)
    let model = FolderModel(
      FolderChoice(
        id: here, name: "Private", e2eRoot: here, drive: drive, role: .editor,
        sourceID: fixture.top), encryption: encryption)
    await model.load(client)
    #expect(model.error == nil)
    #expect(model.nodes.map(\.name) == ["notes.txt"])

    // A new folder's name is sealed with the owner's id of its parent.
    #expect(await model.createFolder(named: "Taxes", client), "\(model.error ?? "")")
    let sealed = try #require(
      posted.all.first?.components(separatedBy: "\"name\":\"").last?
        .components(separatedBy: "\"").first)
    #expect(
      try E2E.decryptName(
        sealed, folderKey: fixture.folderKey, parent: UUID(uuidString: fixture.top)!) == "Taxes")

    // A folder made a moment ago, not on the owner's server yet: listed
    // again, refused while it has no source, then sealed with it.
    let inside = FolderModel(
      FolderChoice(
        id: made, name: "Taxes", e2eRoot: here, drive: drive, role: .editor, sourceID: nil),
      encryption: encryption)
    await inside.load(client)
    #expect(!(await inside.createFolder(named: "2026", client)))
    #expect(inside.error == EncryptionError.notOnOwnersServer.errorDescription)
    #expect(posted.all.count == 1)
    answers["source"] = "\"\(theirsMade)\""
    #expect(await inside.createFolder(named: "2026", client), "\(inside.error ?? "")")
    let second = try #require(
      posted.all.last?.components(separatedBy: "\"name\":\"").last?
        .components(separatedBy: "\"").first)
    #expect(
      try E2E.decryptName(
        second, folderKey: fixture.folderKey, parent: UUID(uuidString: theirsMade)!) == "2026")
  }

  @Test func anOwnersServerThatDoesNotAnswerIsSaid() async throws {
    let fixture = self.fixture
    let client = client { request in
      switch request.path {
      case "/api/files/\(fixture.top)/children": (200, fixture.childrenJSON)
      case "/api/encryption/folders/\(fixture.top)":
        (502, #"{"error":"no answer","code":"remote_unreachable"}"#)
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let model = FolderModel(
      folderID: fixture.top, title: "Private", e2eRoot: fixture.top,
      encryption: Encryption(store: MemoryStore(fixture.privateKey)))
    await model.load(client)
    #expect(model.error == "The owner's server is not reachable. Try again later.")
  }
}
