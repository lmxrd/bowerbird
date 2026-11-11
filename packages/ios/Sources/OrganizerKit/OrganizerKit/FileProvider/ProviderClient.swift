import FileProvider
import Foundation
import OpenAPIRuntime
import OpenAPIURLSession

/// What the File Provider extension asks of the server, for the domain of
/// one synced folder (docs/M5B_DESIGN.md §2): the listings, the change
/// feed, the downloads, and the changes made in Finder sent at the item's
/// version, as the desktop's sync engine does (`packages/sync`): a file
/// changed on the server since is never overwritten, the local one goes
/// beside it as a conflict copy `Name (conflict, <device>, YYYY-MM-DD).ext`.
///
/// The extension's class is a thin layer over this: the logic is here, in
/// OrganizerKit, and its tests run against a stub server.
public struct ProviderClient: Sendable {
  public let rootID: String
  let server: URL
  let token: String
  let client: Client
  let session: URLSession
  let deviceName: String
  /// Today, `YYYY-MM-DD`, for a conflict copy's name.
  let today: @Sendable () -> String

  public init(
    server: URL, token: String, rootID: String, deviceName: String,
    session: URLSession = .shared,
    today: @escaping @Sendable () -> String = { Date.now.formatted(.iso8601.year().month().day()) }
  ) {
    self.server = server
    self.token = token
    self.rootID = rootID
    self.deviceName = deviceName
    self.session = session
    self.today = today
    self.client = .organizer(server: server, token: token, session: session)
  }

  func id(_ identifier: NSFileProviderItemIdentifier) -> String {
    ProviderItem.serverID(identifier, rootID: rootID)
  }

  func item(_ node: FileNode) -> ProviderItem { ProviderItem(node, rootID: rootID) }

  // MARK: - Listings and the change feed

  /// One page of a folder's listing, and the next page's key. The
  /// end-to-end-encrypted folders in it are left out: the extension cannot
  /// decrypt (an encrypted folder is synced in full, docs/M6E_DESIGN.md).
  public func list(_ folder: NSFileProviderItemIdentifier, after: String?) async throws -> (
    nodes: [FileNode], next: String?
  ) {
    let id = id(folder)
    let page = try await client.listChildren(
      path: .init(id: id), query: .init(after: after, limit: 500)
    ).ok.body.json
    var nodes = page.items.filter { $0.e2eRoot == nil }
    // A drive's own folder is never listed (the server does not send it);
    // should one come, it is not shown at a drive's top all the same.
    if nodes.contains(where: { $0.name == ProviderScope.reserved }),
      try await isDriveRoot(id)
    {
      nodes.removeAll { $0.name == ProviderScope.reserved }
    }
    return (nodes, page.next)
  }

  /// A drive's top: the folder with no parent.
  func isDriveRoot(_ id: String) async throws -> Bool {
    try await node(id).parentId == nil
  }

  /// A name the server refuses in `parent`: the drive's own folder's at a
  /// drive's top.
  func isReserved(_ name: String, in parent: String) async throws -> Bool {
    guard name == ProviderScope.reserved else { return false }
    return try await isDriveRoot(parent)
  }

  /// The domain's drive, and that drive's top (asked once, kept in the
  /// scope): the feed's changes of other drives are then left out.
  func learnDrive(_ scope: ProviderScopeStore) async throws {
    let root = try await node(rootID)
    var driveRoot: String? = root.parentId == nil ? root.id : nil
    if driveRoot == nil {
      driveRoot = try await client.getDrive(path: .init(id: root.driveId)).ok.body.json.rootId
    }
    await scope.learn(driveID: root.driveId, driveRootID: driveRoot)
  }

  public func item(_ identifier: NSFileProviderItemIdentifier) async throws -> ProviderItem {
    item(try await node(id(identifier)))
  }

  func node(_ id: String) async throws -> FileNode {
    try await client.getFile(path: .init(id: id)).ok.body.json.value1
  }

  /// The change feed's cursor of now: the working set's anchor.
  public func cursor() async throws -> String {
    try await client.listChanges(query: .init(limit: 1)).ok.body.json.cursor
  }

  /// A page of the feed since `cursor`, as the domain sees it: the items
  /// changed in it (asked for, as the feed does not say all of an item),
  /// those gone, the next cursor, and whether more wait. A cursor too old
  /// (410) is `NSFileProviderError.syncAnchorExpired`: the system lists
  /// everything again.
  ///
  /// The domain's folder deleted, or its drive (the user left it, or it
  /// was deleted), is `noSuchItem`, as listing an unavailable folder is:
  /// nothing is told as gone (the local files stay), the cursor does not
  /// move on, and the app removes the domain (`organizer-fileprovider
  /// remove`, which keeps aside what was not sent).
  public func changes(since cursor: String, scope: ProviderScopeStore) async throws -> (
    updated: [ProviderItem], deleted: [NSFileProviderItemIdentifier], cursor: String,
    hasMore: Bool
  ) {
    let page: Components.Schemas.Changes
    do {
      page = try await client.listChanges(query: .init(cursor: cursor, limit: 500)).ok.body.json
    } catch  where APIError.of(error)?.status == 410 {
      await scope.reset()
      throw NSFileProviderError(.syncAnchorExpired)
    }
    if await scope.current.driveID == nil {
      try await unavailable { try await learnDrive(scope) }
    }
    let applied = await scope.apply(page.changes)
    if applied.rootDeleted {
      // Gone for good, or only moved back from the trash since: asked.
      try await unavailable { _ = try await node(rootID) }
    }
    let (updated, deleted) = (applied.updated, applied.deleted)
    var items: [ProviderItem] = []
    var gone = deleted.map { ProviderItem.identifier($0, rootID: rootID) }
    for id in updated {
      do {
        let node = try await node(id)
        if node.e2eRoot != nil {
          // Encrypted (one moved in, say): not in the domain.
          gone.append(ProviderItem.identifier(id, rootID: rootID))
        } else {
          items.append(item(node))
        }
      } catch  where APIError.of(error)?.status == 404 {
        // Deleted since: its delete is further on in the feed, or told now.
        gone.append(ProviderItem.identifier(id, rootID: rootID))
      }
    }
    return (items, gone, page.cursor, page.hasMore)
  }

  /// `work` on the domain's folder: one the user may no longer see (gone,
  /// or its drive left) is `noSuchItem`.
  func unavailable(_ work: () async throws -> Void) async throws {
    do {
      try await work()
    } catch  where APIError.of(error).map({ [403, 404].contains($0.status) }) == true {
      throw NSFileProviderError(.noSuchItem)
    }
  }

  // MARK: - Content

  /// A file's content into `directory`, and its item (of the version
  /// downloaded).
  public func download(_ identifier: NSFileProviderItemIdentifier, into directory: URL)
    async throws -> (URL, ProviderItem)
  {
    let id = id(identifier)
    var request = URLRequest(url: server.appending(path: "api/files/\(id)/content"))
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    let (temp, response) = try await session.download(for: request)
    let http = try check(response, body: try? Data(contentsOf: temp))
    let target = directory.appending(path: "download-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    try FileManager.default.moveItem(at: temp, to: target)
    var node = try await node(id)
    // The item says the version of the bytes downloaded, should the file
    // have changed in between (the next change brings the new one).
    if let etag = http.value(forHTTPHeaderField: "ETag"),
      let version = Int64(etag.trimmingCharacters(in: CharacterSet(charactersIn: "\"W/")))
    {
      node.version = version
    }
    return (target, item(node))
  }

  // MARK: - Changes made in Finder

  /// A new folder, or a new file with its content.
  public func create(
    name: String, in parent: NSFileProviderItemIdentifier, isFolder: Bool, contents: URL?
  ) async throws -> ProviderItem {
    // A local `.organizer` at a drive's top stays local: the server
    // refuses the name there (its own folder's).
    if try await isReserved(name, in: id(parent)) {
      throw NSFileProviderError(.excludedFromSync)
    }
    do {
      if isFolder {
        let node = try await client.createFolder(
          path: .init(id: id(parent)), body: .json(.init(name: name))
        ).created.body.json
        return item(node)
      }
      return item(try await upload(contents, name: name, parent: id(parent)))
    } catch  where APIError.of(error)?.isConflict == true {
      throw NSFileProviderError(.filenameCollision)
    }
  }

  /// What a modification came to: the server's item now, a conflict copy
  /// made beside it, and whether the system must download the content
  /// again (the server's won).
  public struct Modified: Sendable {
    public let item: ProviderItem
    public let conflictCopy: ProviderItem?
    public let fetchContent: Bool
  }

  /// A rename or move, then new content, at the version the item had
  /// (`baseVersion`). One that moved on meanwhile: the server's rename
  /// stands; new content goes beside the server's as a conflict copy.
  public func modify(
    _ identifier: NSFileProviderItemIdentifier, baseVersion: Int64, name: String?,
    parent: NSFileProviderItemIdentifier?, contents: URL?
  ) async throws -> Modified {
    let id = id(identifier)
    var version = baseVersion
    var node: FileNode?
    if name != nil || parent != nil {
      // Not to be named `.organizer` at a drive's top (the server refuses
      // it): the item is asked about only when that could be so.
      if name == ProviderScope.reserved || (name == nil && parent != nil) {
        let current = try await self.node(id)
        let target = parent.map(self.id) ?? current.parentId
        if let target, try await isReserved(name ?? current.name, in: target) {
          throw NSFileProviderError(.cannotSynchronize)
        }
      }
      do {
        let moved = try await client.moveFile(
          path: .init(id: id),
          body: .json(.init(ifVersion: version, name: name, parentId: parent.map(self.id)))
        ).ok.body.json
        node = moved
        version = moved.version
      } catch  where APIError.of(error)?.isConflict == true {
        // Changed on the server since: its own name and place stand, and
        // this item is told again as it is there.
        return Modified(item: item(try await self.node(id)), conflictCopy: nil, fetchContent: true)
      }
    }
    if let contents {
      do {
        node = try await replace(id, ifVersion: version, contents: contents)
      } catch  where APIError.of(error)?.isConflict == true {
        let current = try await self.node(id)
        let copy = try await conflictCopy(of: current, contents: contents)
        return Modified(item: item(current), conflictCopy: item(copy), fetchContent: true)
      }
    }
    let current: FileNode
    if let node {
      current = node
    } else {
      current = try await self.node(id)
    }
    return Modified(item: item(current), conflictCopy: nil, fetchContent: false)
  }

  /// To the server's trash, at the version it had; one that changed since
  /// is not deleted (`deletionRejected`: the system brings it back).
  public func delete(_ identifier: NSFileProviderItemIdentifier, baseVersion: Int64) async throws {
    do {
      _ = try await client.deleteFile(
        path: .init(id: id(identifier)), query: .init(ifVersion: baseVersion)
      ).ok
    } catch  where APIError.of(error)?.isConflict == true {
      throw NSFileProviderError(.deletionRejected)
    } catch  where APIError.of(error)?.status == 404 {
      // Gone already.
    }
  }

  /// The local content beside the server's file, under a conflict copy's
  /// name (another number when that is taken).
  func conflictCopy(of node: FileNode, contents: URL) async throws -> FileNode {
    let base = ConflictName.name(node.name, device: deviceName, date: today())
    var name = base
    for n in 2...20 {
      do {
        return try await upload(contents, name: name, parent: node.parentId ?? rootID)
      } catch  where APIError.of(error)?.isConflict == true {
        name = ConflictName.withSuffix(base, " \(n)")
      }
    }
    throw APIError(status: 409, message: "No free name for a conflict copy of \(node.name)")
  }

  // MARK: - Uploads

  /// A new file: plainly (multipart) under 4 MB, else resumable.
  func upload(_ contents: URL?, name: String, parent: String) async throws -> FileNode {
    let source = contents ?? emptyFile()
    let size = try size(of: source)
    if size < Organizer.plainUploadLimit {
      let writer = MultipartWriter()
      let body = FileManager.default.temporaryDirectory.appending(
        path: "multipart-\(UUID().uuidString)")
      defer { try? FileManager.default.removeItem(at: body) }
      try writer.write(
        fields: [], files: [.init(filename: name, source: source)], to: body)
      var request = URLRequest(url: server.appending(path: "api/files/\(parent)/files"))
      request.httpMethod = "POST"
      request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
      request.setValue(writer.contentType, forHTTPHeaderField: "Content-Type")
      return try await send(request, file: body)
    }
    let create = ResumableUpload.create(
      server: server, token: token, parentID: parent, name: name, length: size)
    return try await resumable(create, source: source)
  }

  /// A file's new content at its version: `PUT` with `If-Match` under
  /// 4 MB, else resumable.
  func replace(_ id: String, ifVersion: Int64, contents: URL) async throws -> FileNode {
    let size = try size(of: contents)
    if size < Organizer.plainUploadLimit {
      var request = URLRequest(url: server.appending(path: "api/files/\(id)/content"))
      request.httpMethod = "PUT"
      request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
      request.setValue("\"\(ifVersion)\"", forHTTPHeaderField: "If-Match")
      request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
      return try await send(request, file: contents)
    }
    let create = ResumableUpload.createReplacement(
      server: server, token: token, id: id, ifVersion: ifVersion, length: size)
    return try await resumable(create, source: contents)
  }

  /// A resumable upload, its bytes in one request: should it break off,
  /// the system asks again, and it starts over.
  func resumable(_ create: URLRequest, source: URL) async throws -> FileNode {
    let (body, response) = try await session.data(for: create)
    let http = try check(response, body: body)
    guard
      let location = http.value(forHTTPHeaderField: "Location"),
      let upload = URL(string: location, relativeTo: server)?.absoluteURL
    else { throw APIError(status: http.statusCode, message: "An upload without its Location") }
    let append = ResumableUpload.append(upload: upload, token: token, offset: 0, last: true)
    return try await send(append, file: source)
  }

  func send(_ request: URLRequest, file: URL) async throws -> FileNode {
    let (body, response) = try await session.upload(for: request, fromFile: file)
    _ = try check(response, body: body)
    return try JSONDecoder().decode(FileNode.self, from: body)
  }

  /// The response as HTTP, or the server's error (a 401 signs out: the
  /// system shows the domain as needing a sign-in).
  func check(_ response: URLResponse, body: Data?) throws -> HTTPURLResponse {
    guard let http = response as? HTTPURLResponse else {
      throw APIError(status: 0, message: "Not an HTTP answer")
    }
    guard http.statusCode < 400 else {
      if http.statusCode == 401 { throw NSFileProviderError(.notAuthenticated) }
      throw APIError(status: http.statusCode, body: body ?? Data())
    }
    return http
  }

  func size(of url: URL) throws -> Int64 {
    let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
    return (attributes[.size] as? NSNumber)?.int64Value ?? 0
  }

  func emptyFile() -> URL {
    let url = FileManager.default.temporaryDirectory.appending(path: "empty-\(UUID().uuidString)")
    FileManager.default.createFile(atPath: url.path, contents: Data())
    return url
  }
}

/// The error the system understands, of any error of the client: not
/// signed in, no permission, not there, the server away. A drive whose
/// storage is unavailable (its backend fails, its owner's credentials are
/// locked) or that is being moved (read-only meanwhile) is the server away
/// too, and so is the other server of a drive from another server
/// (docs/M7E_DESIGN.md): the system keeps the change and tries again
/// later. That server refusing is not retried.
public func providerError(_ error: any Error) -> any Error {
  if error is NSFileProviderError { return error }
  if let api = APIError.of(error) {
    if api.isStorageUnavailable || api.isReadOnly || api.isRemoteUnreachable {
      return NSFileProviderError(.serverUnreachable)
    }
    if api.isRemoteRefused { return api }
    switch api.status {
    case 401: return NSFileProviderError(.notAuthenticated)
    // The role does not allow it (taken away since the item was listed,
    // say): no permission, which the system does not retry.
    case 403: return CocoaError(.fileWriteNoPermission)
    case 404: return NSFileProviderError(.noSuchItem)
    case 502...504: return NSFileProviderError(.serverUnreachable)
    default: return api
    }
  }
  if error is URLError { return NSFileProviderError(.serverUnreachable) }
  if let client = error as? ClientError, client.underlyingError is URLError {
    return NSFileProviderError(.serverUnreachable)
  }
  return error
}

/// The names of conflict copies, as the sync engine makes them
/// (`names.rs`): `Report (conflict, MacBook, 2026-11-07).docx`.
public enum ConflictName {
  public static func name(_ name: String, device: String, date: String) -> String {
    withSuffix(name, " (conflict, \(device), \(date))")
  }

  /// `suffix` before the extension (none for a name that starts with its
  /// only dot).
  public static func withSuffix(_ name: String, _ suffix: String) -> String {
    guard let dot = name.lastIndex(of: "."), dot != name.startIndex else {
      return name + suffix
    }
    return name[..<dot] + suffix + name[dot...]
  }
}
