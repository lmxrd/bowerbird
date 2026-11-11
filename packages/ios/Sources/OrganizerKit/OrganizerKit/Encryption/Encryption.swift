import Foundation
import Observation

/// Why something in an encrypted folder cannot be done, fit to show.
public enum EncryptionError: Error, Equatable, LocalizedError {
  case locked
  case notSetUp
  case wrongPassphrase
  case keysDoNotMatch
  case noKey
  case nameTaken(String)
  case undecryptable
  /// Shared from another server (docs/E2E_SPEC.md §12), whose server does
  /// not answer for the keys (502 `remote_unreachable`).
  case ownerUnreachable
  /// A folder shared from another server that is not there yet (no
  /// `source_id`): nothing is sealed under it until it is.
  case notOnOwnersServer

  public var errorDescription: String? {
    switch self {
    case .locked: "This folder is encrypted: unlock it in Organizer first."
    case .notSetUp:
      "Encryption is not set up for your account. Set it up on the web, in Settings → Encryption."
    case .wrongPassphrase: "That is not your encryption passphrase."
    case .keysDoNotMatch:
      "Your encryption keys on the server do not match each other. Unlock on the web to check them."
    case .noKey:
      "There is no key for you in this folder yet: a member's device adds it when it next opens the folder."
    case .nameTaken(let name): "\(name) already exists"
    case .undecryptable: "Its name could not be decrypted, so it cannot be moved."
    case .ownerUnreachable: "The owner's server is not reachable. Try again later."
    case .notOnOwnersServer:
      "This folder isn't on its owner's server yet. Try again in a moment."
    }
  }
}

/// The keys this device holds of one encrypted folder: every generation it
/// could unwrap, and the newest, which new names and contents use.
public struct FolderKeySet: Sendable, Equatable {
  /// The encrypted folder's top.
  public let folderID: String
  public let generation: UInt32
  public let keys: [UInt32: Data]

  public init(folderID: String, generation: UInt32, keys: [UInt32: Data]) {
    self.folderID = folderID
    self.generation = generation
    self.keys = keys
  }

  public func key(_ generation: UInt32) -> Data? { keys[generation] }

  /// A name of the folder `parent`, nil when it cannot be decrypted.
  public func decryptName(_ encrypted: String, parent: String) -> String? {
    guard let parent = UUID(uuidString: parent),
      let generation = try? E2E.nameGeneration(encrypted), let key = keys[generation]
    else { return nil }
    return try? E2E.decryptName(encrypted, folderKey: key, parent: parent)
  }

  /// A name sealed for the folder `parent` with the newest key.
  public func encryptName(_ name: String, parent: String) throws -> String {
    guard let key = keys[generation] else { throw EncryptionError.noKey }
    guard let parent = UUID(uuidString: parent) else { throw E2EError.malformed("a folder id") }
    return try E2E.encryptName(name, folderKey: key, generation: generation, parent: parent)
  }
}

/// This device's end-to-end encryption (docs/E2E_SPEC.md): the user's
/// private key once unlocked with the passphrase, kept in the Keychain (in
/// the app group's access group, so the share extension reads it; removed
/// by Lock and at sign out), and the folder keys unwrapped with it, in
/// memory. It also does a member's device's duties: wrapping keys for
/// members waiting, and new generations when a folder needs one.
///
/// In a mirror, and in a folder shared from another Organizer server
/// (docs/E2E_SPEC.md §12), an encrypted folder is sealed with the ids where
/// it comes from (`source_id`): its keys with the answer's `folder_id`, each
/// name with its parent's source (`sealingID`).
@MainActor
@Observable
public final class Encryption {
  public private(set) var unlocked: Bool
  @ObservationIgnored private let store: SecretStore
  @ObservationIgnored private var privateKey: Data?
  /// By the encrypted folder's top id.
  @ObservationIgnored private var folders: [String: FolderKeySet] = [:]

  public init(store: SecretStore = KeychainStore(account: "encryption")) {
    self.store = store
    let stored = (try? store.read()).flatMap { $0.count == 32 ? $0 : nil }
    privateKey = stored
    unlocked = stored != nil
  }

  /// Unwraps the private key with the passphrase (`GET /api/encryption`),
  /// checks it against the public key, and keeps it.
  public func unlock(passphrase: String, _ client: Client) async throws {
    let keys = try await client.getEncryption().ok.body.json
    guard keys.setUp, let wrap = keys.passphrase, let publicKey = keys.publicKey else {
      throw EncryptionError.notSetUp
    }
    let wrapped = try E2E.unbase64url(wrap.wrapped)
    let salt = try E2E.unbase64url(wrap.salt)
    let expected = try E2E.unbase64url(publicKey)
    let iterations = Int(wrap.iterations)
    // PBKDF2's 600,000 rounds take a moment: not on the main thread.
    let privateKey: Data
    do {
      privateKey = try await Task.detached {
        try E2E.unwrapPrivate(wrapped, passphrase: passphrase, salt: salt, iterations: iterations)
      }.value
    } catch E2EError.decrypt {
      throw EncryptionError.wrongPassphrase
    }
    guard try E2E.publicKey(of: privateKey) == expected else {
      throw EncryptionError.keysDoNotMatch
    }
    try store.write(privateKey)
    self.privateKey = privateKey
    folders = [:]
    unlocked = true
  }

  /// Forgets the private key, the folder keys and the files decrypted to
  /// open them.
  public func lock() {
    try? store.delete()
    privateKey = nil
    folders = [:]
    unlocked = false
    FileCache().clearDecrypted()
  }

  /// The keys of the encrypted folder `nodeID` is in (`root`: its top, when
  /// known): from memory, or from the server when `refresh` or not known
  /// yet, after the device's duties there.
  public func folderKeys(
    for nodeID: String, root: String?, refresh: Bool = false, _ client: Client
  ) async throws -> FolderKeySet {
    if !refresh, let root, let known = folders[root] { return known }
    guard let privateKey else { throw EncryptionError.locked }
    let info: Components.Schemas.FolderKeys
    do {
      info = try await client.getFolderKeys(path: .init(id: nodeID)).ok.body.json
    } catch {
      if APIError.of(error)?.isRemoteUnreachable == true { throw EncryptionError.ownerUnreachable }
      throw error
    }
    let settled = await settle(info, privateKey, client)
    let keys = Self.unwrap(settled, privateKey)
    folders[keys.folderID] = keys
    return keys
  }

  /// The device's duties in every folder that needs them
  /// (`GET /api/encryption/pending`), after unlocking and now and then.
  /// Best effort: what fails is tried again next time.
  public func seePending(_ client: Client) async {
    guard let privateKey,
      let tasks = try? await client.listEncryptionTasks().ok.body.json
    else { return }
    for task in tasks {
      guard
        let info = try? await client.getFolderKeys(path: .init(id: task.folderId)).ok.body.json
      else { continue }
      let keys = Self.unwrap(await settle(info, privateKey, client), privateKey)
      folders[keys.folderID] = keys
    }
  }

  /// A new generation when the folder needs one, for every member with a
  /// public key; else the generations this device holds, for the members
  /// who lack them. Answers the folder's keys as they are after it.
  private func settle(
    _ info: Components.Schemas.FolderKeys, _ privateKey: Data, _ client: Client
  ) async -> Components.Schemas.FolderKeys {
    let wrapped = Self.memberKeys(
      info, held: Self.unwrap(info, privateKey), fresh: info.needsNewKey ? E2E.random(32) : nil)
    guard !wrapped.isEmpty else { return info }
    do {
      return try await client.addFolderKeys(
        path: .init(id: info.folderId), body: .json(.init(keys: wrapped))
      ).ok.body.json
    } catch {
      // A 409: another device made that generation first. Read again.
      return (try? await client.getFolderKeys(path: .init(id: info.folderId)).ok.body.json)
        ?? info
    }
  }

  /// The keys to post for a folder's members: with `fresh`, a new
  /// generation for every member with a public key (the server refuses one
  /// that leaves any out); else the generations `held` here, for the
  /// members who lack them. A member on another server (`remote`) is
  /// wrapped for as any, by their `user_id`, which is the share's id: never
  /// looked up as a user here.
  nonisolated static func memberKeys(
    _ info: Components.Schemas.FolderKeys, held: FolderKeySet, fresh: Data?
  ) -> [Components.Schemas.MemberKey] {
    guard let folder = UUID(uuidString: info.folderId) else { return [] }
    var wrapped: [Components.Schemas.MemberKey] = []
    func wrap(_ key: Data, _ generation: Int64, for member: Components.Schemas.Member) {
      guard let publicKey = publicKey(member), let number = UInt32(exactly: generation),
        let sealed = try? E2E.wrapFolderKey(
          key, folder: folder, generation: number, for: publicKey)
      else { return }
      wrapped.append(
        .init(generation: generation, userId: member.userId, wrapped: E2E.base64url(sealed)))
    }
    for member in info.members {
      if let fresh {
        wrap(fresh, info.generation + 1, for: member)
        continue
      }
      for generation in member.missing {
        if let number = UInt32(exactly: generation), let key = held.key(number) {
          wrap(key, generation, for: member)
        }
      }
    }
    return wrapped
  }

  private nonisolated static func publicKey(_ member: Components.Schemas.Member) -> Data? {
    guard let text = member.publicKey, let key = try? E2E.unbase64url(text), key.count == 32
    else { return nil }
    return key
  }

  /// Every generation that unwraps; one that does not is left out (its
  /// names show as encrypted items).
  static func unwrap(_ info: Components.Schemas.FolderKeys, _ privateKey: Data) -> FolderKeySet {
    var keys: [UInt32: Data] = [:]
    if let folder = UUID(uuidString: info.folderId) {
      for wrapped in info.keys {
        guard let generation = UInt32(exactly: wrapped.generation),
          let bytes = try? E2E.unbase64url(wrapped.wrapped),
          let key = try? E2E.unwrapFolderKey(
            bytes, folder: folder, generation: generation, privateKey: privateKey)
        else { continue }
        keys[generation] = key
      }
    }
    return FolderKeySet(
      folderID: info.folderId, generation: UInt32(exactly: info.generation) ?? 0, keys: keys)
  }

  // MARK: Names and uploads

  /// The id the names in the folder `folderID` of an encrypted folder are
  /// sealed with (docs/E2E_SPEC.md §6, §12): its own; in a mirror and in a
  /// folder shared from another server, its `source_id` (`source`), its id
  /// where it comes from. In a tree sealed elsewhere (`elsewhere`) a folder
  /// whose source is not known here is asked again (one made a moment ago
  /// gets it once the other server has it), and refused while it has none.
  public func sealingID(
    of folderID: String, source: String?, elsewhere: Bool, _ client: Client
  ) async throws -> String {
    if let source { return source }
    guard elsewhere else { return folderID }
    let node = try await client.getFile(path: .init(id: folderID)).ok.body.json
    guard let again = node.value1.sourceId else { throw EncryptionError.notOnOwnersServer }
    return again
  }

  /// The names in the folder `folderID` as compared for clashes (NFC, case
  /// ignored, as the server compares plain names): the server cannot.
  /// `parent`: the id they are sealed with (`sealingID`), its own by default.
  public func takenNames(
    in folderID: String, sealedFor parent: String? = nil, _ keys: FolderKeySet, _ client: Client
  ) async throws
    -> Set<String>
  {
    var names = Set<String>()
    var after: String?
    repeat {
      let page = try await client.listChildren(
        path: .init(id: folderID), query: .init(after: after)
      )
      .ok.body.json
      for node in page.items {
        if let name = keys.decryptName(node.name, parent: parent ?? folderID) {
          names.insert(Self.clashKey(name))
        }
      }
      after = page.next
    } while after != nil
    return names
  }

  public nonisolated static func clashKey(_ name: String) -> String {
    name.precomposedStringWithCanonicalMapping.lowercased()
  }

  /// A name for the folder `folderID`, sealed (with `parent`, its sealing
  /// id, by default its own), after checking that no other item there has
  /// it.
  public func sealName(
    _ name: String, in folderID: String, sealedFor parent: String? = nil, root: String?,
    _ client: Client
  ) async throws -> String {
    let parent = parent ?? folderID
    let keys = try await folderKeys(for: folderID, root: root, refresh: true, client)
    let taken = try await takenNames(in: folderID, sealedFor: parent, keys, client)
    guard !taken.contains(Self.clashKey(name)) else { throw EncryptionError.nameTaken(name) }
    return try keys.encryptName(name, parent: parent)
  }

  /// Files into the encrypted folder `folderID`: their names checked
  /// against what is there, then each encrypted (name and contents, with
  /// the newest key) next to it in the staging folder, and handed to the
  /// upload queue as any file. The plaintext copies are removed.
  public func enqueue(
    _ files: [SharedFile], into folderID: String, sealedFor parent: String? = nil, root: String?,
    queue: UploadQueue, _ client: Client
  ) async throws {
    let parent = parent ?? folderID
    let keys = try await folderKeys(for: folderID, root: root, refresh: true, client)
    guard let key = keys.key(keys.generation) else { throw EncryptionError.noKey }
    var taken = try await takenNames(in: folderID, sealedFor: parent, keys, client)
    var sealed: [(file: SharedFile, name: String, title: String)] = []
    for file in files {
      let title = file.name.precomposedStringWithCanonicalMapping
      guard taken.insert(Self.clashKey(title)).inserted else {
        throw EncryptionError.nameTaken(title)
      }
      sealed.append((file, try keys.encryptName(title, parent: parent), title))
    }
    let generation = keys.generation
    for (file, name, title) in sealed {
      let encrypted = file.url.deletingLastPathComponent()
        .appending(path: ".\(UUID().uuidString).encrypted")
      let source = file.url
      try await Task.detached {
        try E2E.encryptFile(at: source, to: encrypted, folderKey: key, generation: generation)
      }.value
      try? FileManager.default.removeItem(at: source)
      try await queue.enqueueFile(
        at: encrypted, name: name, folderID: folderID, move: true, title: title)
    }
  }
}
