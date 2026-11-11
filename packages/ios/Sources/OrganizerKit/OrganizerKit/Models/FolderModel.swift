import Foundation
import Observation

/// One folder of the Files screen: what is in it (folders first, then by
/// name, as the server sorts), and what can be done to it. In an encrypted
/// folder (docs/E2E_SPEC.md) the names are decrypted here, and sealed
/// again for every change; while this device is locked, nothing is shown.
@MainActor
@Observable
public final class FolderModel {
  /// nil for the top of the tree, until its id is known.
  public private(set) var folderID: String?
  public let title: String
  /// The encrypted folder this one is in (its own id for the top), nil
  /// when it is not encrypted.
  public let e2eRoot: String?
  /// The drive it is in (nil: My drive's top, by `GET /files/root`).
  public let drive: DrivePlace?
  /// In a mirror (docs/M7D_DESIGN.md), or shared from another server
  /// (docs/E2E_SPEC.md §12): the folder it copies. An encrypted folder's
  /// keys and names are sealed with the ids there, so its keys are asked by
  /// it and its items' names open (and new ones are sealed) with it.
  public let sourceID: String?
  /// The user's role on the folder (`my_role`): what it offers. Learned
  /// with the root's id for My drive's top.
  public private(set) var role: Role?
  public private(set) var nodes: [FileNode] = []
  public private(set) var loading = false
  public private(set) var loaded = false
  public var error: String?
  /// The keys of the encrypted folder, once unlocked and loaded.
  public private(set) var keys: FolderKeySet?
  /// The items whose names did not decrypt ("Encrypted item").
  public private(set) var undecryptable: Set<String> = []
  @ObservationIgnored public let encryption: Encryption?
  @ObservationIgnored private var next: String?

  public init(
    folderID: String?, title: String, e2eRoot: String? = nil, drive: DrivePlace? = nil,
    role: Role? = nil, sourceID: String? = nil, encryption: Encryption? = nil
  ) {
    self.folderID = folderID
    self.title = title
    self.e2eRoot = e2eRoot
    self.drive = drive
    self.role = role
    self.sourceID = sourceID
    self.encryption = encryption
  }

  /// A folder opened from a list, a drive's top among them.
  public convenience init(_ choice: FolderChoice, encryption: Encryption? = nil) {
    self.init(
      folderID: choice.id, title: choice.name, e2eRoot: choice.e2eRoot, drive: choice.drive,
      role: choice.role, sourceID: choice.sourceID, encryption: encryption)
  }

  /// What can be done in the folder itself (add to it), by its role; only
  /// open while its drive is being moved (read-only).
  public var actions: ItemActions {
    readOnly ? [.open] : ItemActions(role: role, isFolder: true)
  }

  /// What is offered on an item in it, by its role; only open while the
  /// drive is being moved.
  public func actions(_ node: FileNode) -> ItemActions {
    readOnly ? [.open] : node.actions
  }

  /// Its drive is being moved to other storage (docs/M7C_DESIGN.md §2),
  /// or is a mirror (docs/M7D_DESIGN.md).
  public var readOnly: Bool { drive?.readOnly ?? false }

  /// What the folder says when its drive is on someone's own storage (it
  /// goes if they remove it).
  public var storageWarning: String? { drive?.warning }

  /// What the folder says while its drive is being moved.
  public var movingNote: String? {
    drive?.movingTo.map { "Moving to \($0)… Read-only until that is done." }
  }

  /// Upload only: one adds, and sees only what one added (the server
  /// lists nothing else).
  public var isUploadOnly: Bool { role == .uploadOnly }

  /// The folder as the pickers choose it.
  public var current: FolderChoice? {
    folderID.map {
      FolderChoice(
        id: $0, name: title, e2eRoot: e2eRoot, drive: drive, role: role, sourceID: sourceID)
    }
  }

  /// A folder in this one, to open: in the same drive.
  public func choice(_ node: FileNode) -> FolderChoice { FolderChoice(node, drive: drive) }

  /// The top of its drive (titled with the drive's name).
  public var isDriveRoot: Bool { drive != nil && folderID == drive?.rootID }

  public var hasMore: Bool { next != nil }

  public var isEncrypted: Bool { e2eRoot != nil }

  /// An encrypted folder sealed with the ids where it comes from: in a
  /// mirror, or shared from another server (whose folder made a moment ago
  /// has no source yet).
  public var sealedElsewhere: Bool {
    isEncrypted && (sourceID != nil || drive?.isRemote == true)
  }

  /// The id the names in `folderID` (this folder, with `source`, or another
  /// of its tree) are sealed with.
  public func sealingID(of folderID: String, source: String?, _ client: Client) async throws
    -> String
  {
    guard let encryption else { return folderID }
    return try await encryption.sealingID(
      of: folderID, source: source, elsewhere: sealedElsewhere, client)
  }

  /// An encrypted folder on a device that has not unlocked: "Unlock to see
  /// what's inside".
  public var locked: Bool { isEncrypted && encryption?.unlocked != true }

  /// The folder's first page again.
  public func load(_ client: Client) async {
    loading = true
    defer { loading = false }
    do {
      let id = try await resolve(client)
      if locked {
        nodes = []
        next = nil
        keys = nil
        loaded = true
        error = nil
        return
      }
      let page = try await client.listChildren(path: .init(id: id)).ok.body.json
      if let encryption, isEncrypted {
        // Fresh keys each time: a new generation may have been made, and
        // opening the folder is when its duties are done. A mirror's
        // folder asks by the folder it copies (its own has no keys).
        keys = try await encryption.folderKeys(
          for: sourceID ?? id, root: sourceID == nil ? e2eRoot : nil, refresh: true, client)
        undecryptable = []
      }
      nodes = isEncrypted ? sorted(reveal(page.items, in: id)) : page.items
      next = page.next
      loaded = true
      error = nil
    } catch {
      self.error = describe(error)
    }
  }

  public func loadMore(_ client: Client) async {
    guard let next, let folderID, !loading, !locked else { return }
    loading = true
    defer { loading = false }
    do {
      let page = try await client.listChildren(path: .init(id: folderID), query: .init(after: next))
        .ok.body.json
      nodes = isEncrypted ? sorted(nodes + reveal(page.items, in: folderID)) : nodes + page.items
      self.next = page.next
    } catch {
      self.error = describe(error)
    }
  }

  /// The nodes of an encrypted folder as shown: their names decrypted
  /// ("Encrypted item" when they do not) and the plaintext sizes. They
  /// are sorted by those names after (the server sorts the encrypted ones).
  /// In a mirror, the names were sealed for the folder it copies.
  private func reveal(_ items: [FileNode], in folderID: String) -> [FileNode] {
    guard let keys else { return items }
    let parent = sourceID ?? folderID
    return items.map { item in
      var node = item
      if let name = keys.decryptName(item.name, parent: parent) {
        node.name = name
      } else {
        node.name = "Encrypted item"
        undecryptable.insert(item.id)
      }
      if !node.isFolder, let size = try? E2E.plainSize(item.size) { node.size = size }
      return node
    }
  }

  private func sorted(_ nodes: [FileNode]) -> [FileNode] {
    nodes.sorted { a, b in
      if a.isFolder != b.isFolder { return a.isFolder }
      return a.name.localizedStandardCompare(b.name) == .orderedAscending
    }
  }

  /// The folder's id: the root's is asked for once.
  public func resolve(_ client: Client) async throws -> String {
    if let folderID { return folderID }
    let root = try await client.getRoot().ok.body.json
    folderID = root.value1.id
    role = root.value1.myRole
    return root.value1.id
  }

  /// A name as the server takes it in `parent`: sealed in an encrypted
  /// folder (after checking no other item there has it) with the parent's
  /// sealing id (`source`: its `source_id`, when known), else as typed.
  private func serverName(
    _ name: String, in parent: String, source: String?, _ client: Client
  ) async throws -> String {
    guard isEncrypted else { return name }
    guard let encryption, encryption.unlocked else { throw EncryptionError.locked }
    let sealing = try await sealingID(of: parent, source: source, client)
    return try await encryption.sealName(
      name, in: parent, sealedFor: sealing, root: e2eRoot, client)
  }

  @discardableResult
  public func createFolder(named name: String, _ client: Client) async -> Bool {
    await run(client) { id in
      let name = try await self.serverName(name, in: id, source: self.sourceID, client)
      _ = try await client.createFolder(path: .init(id: id), body: .json(.init(name: name)))
        .created
    }
  }

  @discardableResult
  public func rename(_ node: FileNode, to name: String, _ client: Client) async -> Bool {
    await run(client) { id in
      let name = try await self.serverName(
        name, in: node.parentId ?? id, source: self.sourceID, client)
      _ = try await client.moveFile(
        path: .init(id: node.id), body: .json(.init(ifVersion: node.version, name: name))
      ).ok
    }
  }

  /// In an encrypted folder, the name goes sealed again for the new
  /// parent (it is bound to it); the server refuses a move across the
  /// folder's edge.
  @discardableResult
  public func move(_ node: FileNode, to folderID: String, _ client: Client) async -> Bool {
    await run(client) { _ in
      var name: String?
      if self.isEncrypted {
        guard !self.undecryptable.contains(node.id) else { throw EncryptionError.undecryptable }
        name = try await self.serverName(node.name, in: folderID, source: nil, client)
      }
      _ = try await client.moveFile(
        path: .init(id: node.id),
        body: .json(.init(ifVersion: node.version, name: name, parentId: folderID))
      ).ok
    }
  }

  /// To the trash (restored on the web).
  @discardableResult
  public func delete(_ node: FileNode, _ client: Client) async -> Bool {
    await run(client) { _ in
      _ = try await client.deleteFile(
        path: .init(id: node.id), query: .init(ifVersion: node.version)
      )
      .ok
    }
  }

  /// Does a change, then shows the folder as it is now.
  private func run(_ client: Client, _ change: (String) async throws -> Void) async -> Bool {
    do {
      try await change(try await resolve(client))
      await load(client)
      return true
    } catch {
      self.error = describe(error)
      return false
    }
  }
}

/// Files downloaded to open them (Quick Look), in the caches, by version:
/// a file opened again unchanged is not downloaded again.
public struct FileCache: Sendable {
  public let root: URL
  let session: URLSession

  public init(
    root: URL = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
      .appending(path: "Files"),
    session: URLSession = .shared
  ) {
    self.root = root
    self.session = session
  }

  /// Where files decrypted to open them go, apart: Lock removes them.
  var decrypted: URL { root.appending(path: "decrypted") }

  public func location(of node: FileNode, decrypted: Bool = false) -> URL {
    (decrypted ? self.decrypted : root).appending(path: "\(node.id)-\(node.version)")
      .appending(path: node.name)
  }

  public func clearDecrypted() {
    try? FileManager.default.removeItem(at: decrypted)
  }

  /// The file, downloaded; with `keys` (a file of an encrypted folder,
  /// `node.name` its decrypted name), decrypted a segment at a time.
  public func open(_ node: FileNode, server: URL, token: String, keys: FolderKeySet? = nil)
    async throws -> URL
  {
    let destination = location(of: node, decrypted: keys != nil)
    if FileManager.default.fileExists(atPath: destination.path) { return destination }
    var request = URLRequest(url: server.appending(path: "api/files/\(node.id)/content"))
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    let (temporary, response) = try await session.download(for: request)
    let status = (response as? HTTPURLResponse)?.statusCode ?? 0
    guard status == 200 else {
      let body = (try? Data(contentsOf: temporary)) ?? Data()
      throw APIError(status: status, body: body)
    }
    try FileManager.default.createDirectory(
      at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
    if let keys {
      defer { try? FileManager.default.removeItem(at: temporary) }
      try E2E.decryptFile(at: temporary, to: destination, keys: keys.key)
      return destination
    }
    try? FileManager.default.removeItem(at: destination)
    try FileManager.default.moveItem(at: temporary, to: destination)
    return destination
  }
}
