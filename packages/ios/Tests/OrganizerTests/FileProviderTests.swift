import FileProvider
import Foundation
import Testing

@testable import OrganizerKit

/// The server as the File Provider extension sees it, behind a URLProtocol:
/// a small tree with versions, the listings, the files' content (with the
/// version as ETag), the uploads at a version (`If-Match`, `if_version`),
/// moves, deletes, and a change feed the test scripts.
final class TreeServer: @unchecked Sendable {
  nonisolated(unsafe) static var current = TreeServer()

  struct Node {
    var parent: String?
    var name: String
    var folder: Bool
    var version: Int64
    var content: Data
    /// The end-to-end-encrypted folder it is in, if any.
    var e2eRoot: String? = nil
    /// The user's role on it (`my_role`).
    var role = "owner"
    /// In a mirror: the node it copies (`source_id`).
    var source: String? = nil
    /// From another server: that server (`remote`).
    var remote: String? = nil
  }

  private let lock = NSLock()
  private(set) var nodes: [String: Node] = [
    "root": Node(
      parent: nil, name: "Docs", folder: true, version: 1, content: Data())
  ]
  private(set) var log: [String] = []
  /// The change feed's answers, in turn: a status and a body.
  var feed: [(Int, String)] = []

  func add(
    _ id: String, in parent: String, _ name: String, folder: Bool = false, _ text: String = "",
    e2eRoot: String? = nil, role: String = "owner", source: String? = nil, remote: String? = nil
  ) {
    lock.withLock {
      nodes[id] = Node(
        parent: parent, name: name, folder: folder, version: 1, content: Data(text.utf8),
        e2eRoot: e2eRoot, role: role, source: source, remote: remote)
    }
  }

  /// The domain's folder as the top of a drive from another server
  /// (docs/M7E_DESIGN.md §6), with the role it was given there.
  func remoteRoot(role: String) {
    lock.withLock {
      nodes["root"]?.role = role
      nodes["root"]?.remote = "cloud.example"
    }
  }

  /// The domain's folder as a mirror's top (docs/M7D_DESIGN.md): viewed.
  func mirrorRoot() {
    lock.withLock {
      nodes["root"]?.role = "viewer"
      nodes["root"]?.source = "source-root"
    }
  }

  func node(_ id: String) -> Node? { lock.withLock { nodes[id] } }

  /// Gone from the server, or out of the user's sight (a drive left).
  func remove(_ id: String) { lock.withLock { nodes[id] = nil } }

  /// Changed on the server meanwhile: a new version.
  func bump(_ id: String, _ text: String) {
    lock.withLock {
      nodes[id]?.version += 1
      nodes[id]?.content = Data(text.utf8)
    }
  }

  func json(_ id: String, _ node: Node) -> String {
    """
    {"id":"\(id)","drive_id":"d1","parent_id":\(node.parent.map { "\"\($0)\"" } ?? "null"),"name":"\(node.name)",\
    "kind":"\(node.folder ? "folder" : "file")","size":\(node.content.count),\
    "mime":\(node.folder ? "null" : "\"text/plain\""),"version":\(node.version),"portable":true,\
    "created_at":"2026-11-07T10:00:00Z","updated_at":"2026-11-07T10:00:00.123456Z",\
    "my_role":"\(node.role)"\(node.e2eRoot.map { #","e2e_root":"\#($0)""# } ?? ""),\
    "source_id":\(node.source.map { "\"\($0)\"" } ?? "null"),\
    "remote":\(node.remote.map { "\"\($0)\"" } ?? "null")}
    """
  }

  func entry(_ id: String, _ node: Node) -> String {
    String(json(id, node).dropLast())
      + #","path":"/x","permissions":{"own":true,"mount":false,"create":true,"change":true,"delete":true,"share":true}}"#
  }

  typealias Answer = (Int, [String: String], Data)

  func handle(_ request: URLRequest, body: Data) -> Answer {
    lock.withLock {
      let method = request.httpMethod ?? "GET"
      let url = request.url!
      let parts = url.path.split(separator: "/").map(String.init)
      log.append("\(method) \(url.path)")
      let text = { (status: Int, body: String) -> Answer in
        (status, ["Content-Type": "application/json"], Data(body.utf8))
      }
      guard request.value(forHTTPHeaderField: "Authorization") == "Bearer org_token" else {
        return text(401, #"{"error":"Not signed in"}"#)
      }
      // `api/files/<id>/<what>`.
      let id = parts.count > 2 ? parts[2] : ""
      let what = parts.count > 3 ? parts[3] : ""
      let route = parts.count > 1 ? "\(parts[0])/\(parts[1])" : ""
      switch (method, route, what) {
      case ("GET", "api/drives", _):
        // Every node is in the one shared drive d1, whose top is `root`.
        return text(
          200,
          #"{"id":"\#(id)","kind":"shared","name":"Docs","root_id":"root","used":0,"limit":null,"#
            + #""manages":false,"members":2,"managers":[],"team_id":null,"team_name":null,"#
            + #""my_role":"editor","backend":"# + disk
            + #","available":true,"unavailable":null,"unavailable_reason":null,"moving_to":null}"#)
      case ("GET", "api/changes", _):
        if feed.isEmpty { return text(200, #"{"changes":[],"cursor":"c0","has_more":false}"#) }
        let (status, body) = feed.removeFirst()
        return text(status, body)
      case ("GET", "api/files", "children"):
        let items = nodes.filter { $0.value.parent == id }.sorted { $0.key < $1.key }
          .map { json($0.key, $0.value) }
        return text(200, #"{"items":[\#(items.joined(separator: ","))],"mounts":[],"shared":[]}"#)
      case ("GET", "api/files", ""):
        guard let node = nodes[id] else { return text(404, #"{"error":"Not found"}"#) }
        return text(200, entry(id, node))
      case ("GET", "api/files", "content"):
        guard let node = nodes[id] else { return text(404, #"{"error":"Not found"}"#) }
        return (200, ["ETag": "\"\(node.version)\""], node.content)
      case ("PUT", "api/files", "content"):
        guard var node = nodes[id] else { return text(404, #"{"error":"Not found"}"#) }
        guard request.value(forHTTPHeaderField: "If-Match") == "\"\(node.version)\"" else {
          return text(409, #"{"error":"Changed since"}"#)
        }
        node.version += 1
        node.content = body
        nodes[id] = node
        return text(200, json(id, node))
      case ("POST", "api/files", "files"):
        let parent = id
        let form = String(decoding: body, as: UTF8.self)
        guard let range = form.range(of: #"filename=""#),
          let end = form[range.upperBound...].firstIndex(of: "\"")
        else { return text(400, #"{"error":"No file"}"#) }
        let name = String(form[range.upperBound..<end])
        if nodes.values.contains(where: { $0.parent == parent && $0.name == name }) {
          return text(409, #"{"error":"The name is taken"}"#)
        }
        let content =
          form.components(separatedBy: "\r\n\r\n").dropFirst().first?
          .components(separatedBy: "\r\n--").first ?? ""
        let new = "new-\(nodes.count)"
        nodes[new] = Node(
          parent: parent, name: name, folder: false, version: 1, content: Data(content.utf8))
        return text(201, json(new, nodes[new]!))
      case ("POST", "api/files", "folders"):
        let parent = id
        let name =
          (try? JSONDecoder().decode([String: String].self, from: body))?["name"] ?? ""
        let new = "folder-\(nodes.count)"
        nodes[new] = Node(parent: parent, name: name, folder: true, version: 1, content: Data())
        return text(201, json(new, nodes[new]!))
      case ("PATCH", "api/files", ""):
        guard var node = nodes[id] else { return text(404, #"{"error":"Not found"}"#) }
        let move = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] ?? [:]
        guard (move["if_version"] as? Int).map(Int64.init) == node.version else {
          return text(409, #"{"error":"Changed since"}"#)
        }
        if let name = move["name"] as? String { node.name = name }
        if let parent = move["parent_id"] as? String { node.parent = parent }
        node.version += 1
        nodes[id] = node
        return text(200, json(id, node))
      case ("DELETE", "api/files", ""):
        guard let node = nodes[id] else { return text(404, #"{"error":"Not found"}"#) }
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        guard query.first(where: { $0.name == "if_version" })?.value == String(node.version) else {
          return text(409, #"{"error":"Changed since"}"#)
        }
        nodes[id] = nil
        return text(
          200,
          #"{"id":"\#(id)","name":"\#(node.name)","kind":"file","original_path":"x","size":0,"deleted_at":"2026-11-08T10:00:00Z"}"#
        )
      default:
        return text(404, #"{"error":"Not found"}"#)
      }
    }
  }
}

final class TreeProtocol: URLProtocol {
  override class func canInit(with request: URLRequest) -> Bool { true }
  override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

  override func startLoading() {
    var body = request.httpBody ?? Data()
    if body.isEmpty, let stream = request.httpBodyStream {
      stream.open()
      var buffer = [UInt8](repeating: 0, count: 64 * 1024)
      while stream.hasBytesAvailable {
        let read = stream.read(&buffer, maxLength: buffer.count)
        if read <= 0 { break }
        body.append(buffer, count: read)
      }
      stream.close()
    }
    let (status, headers, data) = TreeServer.current.handle(request, body: body)
    let response = HTTPURLResponse(
      url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
    client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
    if !data.isEmpty { client?.urlProtocol(self, didLoad: data) }
    client?.urlProtocolDidFinishLoading(self)
  }

  override func stopLoading() {}
}

/// The client of a domain: by default the domain is the drive's top.
func providerClient(rootID: String = "root") -> ProviderClient {
  let configuration = URLSessionConfiguration.ephemeral
  configuration.protocolClasses = [TreeProtocol.self]
  return ProviderClient(
    server: server, token: "org_token", rootID: rootID, deviceName: "Mac",
    session: URLSession(configuration: configuration), today: { "2026-11-08" })
}

func change(
  _ id: String, _ op: String, parent: String? = nil, folder: Bool = false, version: Int64 = 2,
  drive: String = "d1", name: String? = nil
) -> Components.Schemas.Change {
  .init(
    driveId: drive, kind: op == "delete" ? nil : (folder ? .folder : .file),
    name: op == "delete" ? nil : (name ?? id), nodeId: id, op: op, parentId: parent,
    version: op == "delete" ? nil : version)
}

func scopeStore(rootID: String = "root") -> ProviderScopeStore {
  ProviderScopeStore(url: temporaryFolder().appending(path: "scope.json"), rootID: rootID)
}

func temporaryFile(_ text: String) throws -> URL {
  let url = temporaryFolder().appending(path: "contents-\(UUID().uuidString)")
  try Data(text.utf8).write(to: url)
  return url
}

@Suite struct ProviderItemTests {
  @Test func theDomainsFolderIsTheRootContainer() throws {
    let server = TreeServer()
    server.add("a", in: "root", "a.txt", "hello")
    server.add("f", in: "root", "Taxes", folder: true)
    server.add("b", in: "f", "b.pdf")
    let decode = { (id: String) throws -> FileNode in
      try JSONDecoder().decode(FileNode.self, from: Data(server.json(id, server.node(id)!).utf8))
    }
    let root = ProviderItem(try decode("root"), rootID: "root")
    #expect(root.itemIdentifier == .rootContainer)
    #expect(root.parentItemIdentifier == .rootContainer)
    let a = ProviderItem(try decode("a"), rootID: "root")
    #expect(a.itemIdentifier.rawValue == "a")
    #expect(a.parentItemIdentifier == .rootContainer)
    #expect(a.documentSize == 5)
    #expect(a.contentType == .plainText)
    #expect(a.capabilities.contains(.allowsWriting))
    #expect(ProviderItem.version(of: a.itemVersion.contentVersion) == 1)
    #expect(a.contentModificationDate != nil)
    let folder = ProviderItem(try decode("f"), rootID: "root")
    #expect(folder.contentType == .folder)
    #expect(folder.capabilities.contains(.allowsContentEnumerating))
    // (Adding sub-items is the same bit as writing.)
    #expect(folder.capabilities.contains(.allowsAddingSubItems))
    let b = ProviderItem(try decode("b"), rootID: "root")
    #expect(b.parentItemIdentifier.rawValue == "f")
    #expect(ProviderItem.serverID(.rootContainer, rootID: "root") == "root")
  }

  /// Finder and the Files app enforce what `my_role` allows.
  @Test func capabilitiesFollowTheRole() throws {
    let server = TreeServer()
    let item = { (role: String, folder: Bool) throws -> NSFileProviderItemCapabilities in
      server.add("x", in: "root", "x", folder: folder, role: role)
      let node = try JSONDecoder().decode(
        FileNode.self, from: Data(server.json("x", server.node("x")!).utf8))
      #expect(node.myRole?.rawValue == role)
      return ProviderItem(node, rootID: "root").capabilities
    }
    let all: NSFileProviderItemCapabilities = [
      .allowsReading, .allowsRenaming, .allowsReparenting, .allowsDeleting,
    ]
    for role in ["editor", "manager", "owner"] {
      #expect(try item(role, false) == all.union(.allowsWriting))
      #expect(
        try item(role, true) == all.union([.allowsContentEnumerating, .allowsAddingSubItems]))
    }
    // A viewer reads: no writing, renaming, moving or deleting, nothing added.
    #expect(try item("viewer", false) == .allowsReading)
    #expect(try item("viewer", true) == [.allowsReading, .allowsContentEnumerating])
    // Upload only adds to a folder, and reads what it added, nothing more.
    #expect(try item("upload_only", false) == .allowsReading)
    #expect(
      try item("upload_only", true)
        == [.allowsReading, .allowsContentEnumerating, .allowsAddingSubItems])
    // No role known: as before roles, the server decides.
    #expect(ProviderItem.capabilities(role: nil, isFolder: false) == all.union(.allowsWriting))
  }

  @Test func conflictCopiesAreNamedAsTheSyncEngineNamesThem() {
    #expect(
      ConflictName.name("Report.docx", device: "MacBook", date: "2026-11-07")
        == "Report (conflict, MacBook, 2026-11-07).docx")
    #expect(ConflictName.name("notes", device: "Mac", date: "d") == "notes (conflict, Mac, d)")
    #expect(ConflictName.name(".env", device: "Mac", date: "d") == ".env (conflict, Mac, d)")
    #expect(ConflictName.withSuffix("a.tar.gz", " 2") == "a.tar 2.gz")
  }
}

@Suite struct ProviderScopeTests {
  @Test func onlyChangesInTheDomainAreTold() {
    var scope = ProviderScope(rootID: "root")
    let applied = scope.apply([
      change("a", "create", parent: "root"),
      change("elsewhere", "create", parent: "other"),
      change("f", "create", parent: "root", folder: true),
      change("b", "create", parent: "f"),
      change("root", "move", parent: "x"),
    ])
    #expect(applied.updated == ["a", "f", "b"])
    #expect(applied.deleted.isEmpty)
    #expect(!applied.rootDeleted)
    #expect(scope.folders == ["root", "f"])
  }

  @Test func movedOutOrDeletedIsGone() {
    var scope = ProviderScope(rootID: "root")
    _ = scope.apply([change("f", "create", parent: "root", folder: true)])
    let applied = scope.apply([
      change("a", "update", parent: "root"),
      change("a", "move", parent: "other"),
      change("f", "delete"),
      change("g", "move", parent: "f"),
    ])
    #expect(applied.updated.isEmpty)
    #expect(applied.deleted == ["a", "f", "g"])
    #expect(scope.folders == ["root"])
  }

  @Test func otherDrivesChangesAreLeftOut() {
    // The domain is a folder `f` in the shared drive d1, whose top is `top`.
    var scope = ProviderScope(rootID: "f", driveID: "d1", driveRootID: "top")
    let applied = scope.apply([
      change("a", "create", parent: "f"),
      // Another drive's, even under a folder id the domain knows, and its
      // deletes: not this domain's.
      change("b", "create", parent: "f", drive: "d2"),
      change("c", "delete", drive: "d2"),
      change("c", "move", parent: "elsewhere", drive: "d2"),
      // Another drive's top appearing: the user joined it.
      change("top2", "create", folder: true, drive: "d2"),
    ])
    #expect(applied.updated == ["a"])
    #expect(applied.deleted.isEmpty)
    #expect(!applied.rootDeleted)
  }

  @Test func theDrivesOwnFolderIsNeverTold() {
    var scope = ProviderScope(rootID: "top", driveID: "d1", driveRootID: "top")
    let applied = scope.apply([
      change("o", "create", parent: "top", folder: true, name: ".organizer"),
      change("p", "create", parent: "top", folder: true, name: "Plans"),
    ])
    #expect(applied.updated == ["p"])
    #expect(scope.folders == ["top", "p"])
  }

  @Test func theDomainsFolderOrItsDriveDeletedIsToldApart() {
    // Its own folder deleted.
    var scope = ProviderScope(rootID: "f", driveID: "d1", driveRootID: "top")
    let own = scope.apply([change("a", "update", parent: "f"), change("f", "delete")])
    #expect(own.rootDeleted)
    // Nothing of the domain is told as gone for it.
    #expect(own.deleted.isEmpty)
    #expect(own.updated == ["a"])
    // The drive's top deleted: the user left the drive, or it was deleted.
    var left = ProviderScope(rootID: "f", driveID: "d1", driveRootID: "top")
    #expect(left.apply([change("top", "delete")]).rootDeleted)
    // Another drive's top deleted: not this domain's.
    var other = ProviderScope(rootID: "f", driveID: "d1", driveRootID: "top")
    #expect(!other.apply([change("top2", "delete", drive: "d2")]).rootDeleted)
  }

  @Test func aListedFolderJoinsTheScopeAndItIsKept() async throws {
    let server = TreeServer()
    server.add("f", in: "root", "Taxes", folder: true)
    server.add("a", in: "root", "a.txt")
    let nodes = try ["f", "a"].map {
      try JSONDecoder().decode(FileNode.self, from: Data(server.json($0, server.node($0)!).utf8))
    }
    let url = temporaryFolder().appending(path: "scope.json")
    let store = ProviderScopeStore(url: url, rootID: "root")
    await store.listed(nodes, in: "root")
    #expect(await store.current.folders == ["root", "f"])
    // Kept for the next run; another domain's is not taken.
    #expect(await ProviderScopeStore(url: url, rootID: "root").current.folders == ["root", "f"])
    #expect(await ProviderScopeStore(url: url, rootID: "other").current.folders == ["other"])
  }
}

@Suite(.serialized) struct ProviderClientTests {
  init() { TreeServer.current = TreeServer() }

  @Test func aFolderIsListedAndAFileDownloadedAtItsVersion() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.add("f", in: "root", "Taxes", folder: true)
    let client = providerClient()
    let (nodes, next) = try await client.list(.rootContainer, after: nil)
    #expect(nodes.map(\.id) == ["a", "f"])
    #expect(next == nil)
    #expect(TreeServer.current.log.contains("GET /api/files/root/children"))

    let (url, item) = try await client.download(
      NSFileProviderItemIdentifier("a"), into: temporaryFolder())
    #expect(try String(contentsOf: url, encoding: .utf8) == "hello")
    #expect(item.itemIdentifier.rawValue == "a")
    #expect(ProviderItem.version(of: item.itemVersion.contentVersion) == 1)
  }

  @Test func listedItemsCarryTheirRolesCapabilities() async throws {
    TreeServer.current.add("v", in: "root", "Plans", folder: true, role: "viewer")
    TreeServer.current.add("p", in: "v", "plan.pdf", role: "viewer")
    TreeServer.current.add("u", in: "root", "Inbox", folder: true, role: "upload_only")
    let client = providerClient()
    let (top, _) = try await client.list(.rootContainer, after: nil)
    let items = Dictionary(uniqueKeysWithValues: top.map { ($0.id, client.item($0)) })
    #expect(items["v"]?.capabilities == [.allowsReading, .allowsContentEnumerating])
    #expect(items["u"]?.capabilities.contains(.allowsAddingSubItems) == true)
    #expect(items["u"]?.capabilities.contains(.allowsDeleting) == false)
    let plan = try await client.item(NSFileProviderItemIdentifier("p"))
    #expect(plan.capabilities == .allowsReading)
  }

  /// A mirror (docs/M7D_DESIGN.md §5) is a viewer's drive: every item of
  /// its domain, the top too, is read-only in Finder.
  @Test func aMirrorsItemsAreReadOnly() async throws {
    TreeServer.current.mirrorRoot()
    TreeServer.current.add("f", in: "root", "Plans", folder: true, role: "viewer", source: "s-f")
    TreeServer.current.add("a", in: "f", "a.txt", "a", role: "viewer", source: "s-a")
    let client = providerClient()
    let root = try await client.item(.rootContainer)
    #expect(root.node.sourceId == "source-root")
    #expect(root.capabilities == [.allowsReading, .allowsContentEnumerating])
    let (top, _) = try await client.list(.rootContainer, after: nil)
    #expect(
      top.map { client.item($0).capabilities } == [[.allowsReading, .allowsContentEnumerating]])
    let a = try await client.item(NSFileProviderItemIdentifier("a"))
    #expect(a.capabilities == .allowsReading)
  }

  /// A drive from another server (docs/M7E_DESIGN.md §5, §6) is a drive
  /// like any, its bytes through the user's server: its items follow the
  /// role it was given there.
  @Test func aRemoteDrivesItemsFollowTheirRole() async throws {
    TreeServer.current.remoteRoot(role: "editor")
    TreeServer.current.add(
      "f", in: "root", "Plans", folder: true, role: "editor", remote: "cloud.example")
    TreeServer.current.add("a", in: "f", "a.txt", "a", role: "viewer", remote: "cloud.example")
    let client = providerClient()
    let root = try await client.item(.rootContainer)
    #expect(root.node.remote == "cloud.example")
    #expect(root.capabilities.contains(.allowsAddingSubItems))
    let (top, _) = try await client.list(.rootContainer, after: nil)
    #expect(top.map { client.item($0).capabilities.contains(.allowsWriting) } == [true])
    let a = try await client.item(NSFileProviderItemIdentifier("a"))
    #expect(a.capabilities == .allowsReading)
    let (url, _) = try await client.download(
      NSFileProviderItemIdentifier("a"), into: temporaryFolder())
    #expect(try String(contentsOf: url, encoding: .utf8) == "a")
  }

  @Test func encryptedFoldersAreLeftOutOfTheDomain() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.add("s", in: "root", "Secret", folder: true, e2eRoot: "s")
    TreeServer.current.add("x", in: "s", "c2VhbGVk", e2eRoot: "s")
    let client = providerClient()
    let (nodes, _) = try await client.list(.rootContainer, after: nil)
    #expect(nodes.map(\.id) == ["a"])

    // The feed's changes to them are told as gone, never as items.
    TreeServer.current.feed = [
      (
        200,
        #"{"changes":[{"node_id":"s","drive_id":"d1","op":"create","kind":"folder","parent_id":"root","name":"Secret","version":1},"#
          + #"{"node_id":"x","drive_id":"d1","op":"create","kind":"file","parent_id":"s","name":"c2VhbGVk","version":1},"#
          + #"{"node_id":"a","drive_id":"d1","op":"update","kind":"file","parent_id":"root","name":"a.txt","version":1}],"#
          + #""cursor":"c2","has_more":false}"#
      )
    ]
    let scope = ProviderScopeStore(
      url: temporaryFolder().appending(path: "scope.json"), rootID: "root")
    let page = try await client.changes(since: "c1", scope: scope)
    #expect(page.updated.map(\.itemIdentifier.rawValue) == ["a"])
    #expect(Set(page.deleted.map(\.rawValue)) == ["s", "x"])
  }

  @Test func theWorkingSetFollowsTheFeed() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.feed = [
      (
        200,
        #"{"changes":[{"node_id":"a","drive_id":"d1","op":"update","kind":"file","parent_id":"root","name":"a.txt","version":2},"#
          + #"{"node_id":"x","drive_id":"d1","op":"create","kind":"file","parent_id":"other","name":"x","version":1},"#
          + #"{"node_id":"gone","drive_id":"d1","op":"delete","kind":null,"parent_id":null,"name":null,"version":null}],"#
          + #""cursor":"c2","has_more":true}"#
      ),
      (410, #"{"error":"The cursor is too old"}"#),
    ]
    let client = providerClient()
    let scope = ProviderScopeStore(
      url: temporaryFolder().appending(path: "scope.json"), rootID: "root")
    let page = try await client.changes(since: "c1", scope: scope)
    #expect(page.updated.map(\.itemIdentifier.rawValue) == ["a"])
    #expect(page.deleted.map(\.rawValue) == ["gone"])
    #expect(page.cursor == "c2")
    #expect(page.hasMore)

    // Too old: the system is told to list everything again.
    await #expect(throws: NSFileProviderError.self) {
      try await client.changes(since: "c2", scope: scope)
    }
    #expect(try await client.cursor() == "c0")
  }

  @Test func aDomainInASharedDriveFollowsOnlyItsDrive() async throws {
    TreeServer.current.add("f", in: "root", "Plans", folder: true)
    TreeServer.current.add("a", in: "f", "a.txt", "hello")
    TreeServer.current.feed = [
      (
        200,
        #"{"changes":[{"node_id":"a","drive_id":"d1","op":"update","kind":"file","parent_id":"f","name":"a.txt","version":2},"#
          + #"{"node_id":"b","drive_id":"d2","op":"create","kind":"file","parent_id":"f","name":"b","version":1},"#
          + #"{"node_id":"c","drive_id":"d2","op":"delete","kind":null,"parent_id":null,"name":null,"version":null}],"#
          + #""cursor":"c2","has_more":false}"#
      )
    ]
    let client = providerClient(rootID: "f")
    let scope = scopeStore(rootID: "f")
    let page = try await client.changes(since: "c1", scope: scope)
    #expect(page.updated.map(\.itemIdentifier.rawValue) == ["a"])
    #expect(page.deleted.isEmpty)
    // The drive and its top were asked once, and kept.
    #expect(await scope.current.driveID == "d1")
    #expect(await scope.current.driveRootID == "root")
    #expect(TreeServer.current.log.filter { $0 == "GET /api/drives/d1" }.count == 1)
    _ = try await client.changes(since: "c2", scope: scope)
    #expect(TreeServer.current.log.filter { $0 == "GET /api/drives/d1" }.count == 1)
  }

  @Test func aDriveLeftMakesTheDomainUnavailableAndDeletesNothing() async throws {
    TreeServer.current.add("f", in: "root", "Plans", folder: true)
    TreeServer.current.add("a", in: "f", "a.txt", "hello")
    let left =
      #"{"changes":[{"node_id":"root","drive_id":"d1","op":"delete","kind":null,"parent_id":null,"name":null,"version":null}],"#
      + #""cursor":"c3","has_more":false}"#
    TreeServer.current.feed = [
      (200, #"{"changes":[],"cursor":"c2","has_more":false}"#), (200, left), (200, left),
    ]
    let client = providerClient(rootID: "f")
    let scope = scopeStore(rootID: "f")
    _ = try await client.changes(since: "c1", scope: scope)

    // The top's delete while the folder is still there (moved back from
    // the trash since): nothing happens.
    let still = try await client.changes(since: "c2", scope: scope)
    #expect(still.deleted.isEmpty)
    #expect(still.cursor == "c3")

    // The user left the drive: the domain's folder is not there for them.
    TreeServer.current.remove("f")
    TreeServer.current.remove("root")
    do {
      _ = try await client.changes(since: "c2", scope: scope)
      Issue.record("A drive left was not reported")
    } catch let error as NSFileProviderError {
      #expect(error.code == .noSuchItem)
    }
  }

  @Test func theDrivesOwnFolderIsNeverListedNorMadeAtItsTop() async throws {
    TreeServer.current.add("o", in: "root", ".organizer", folder: true)
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.add("f", in: "root", "Plans", folder: true)
    TreeServer.current.add("o2", in: "f", ".organizer", folder: true)
    let client = providerClient()
    #expect(try await client.list(.rootContainer, after: nil).nodes.map(\.id) == ["a", "f"])
    // Deeper down, it is a name like any other.
    #expect(
      try await client.list(NSFileProviderItemIdentifier("f"), after: nil).nodes.map(\.id)
        == ["o2"])

    do {
      _ = try await client.create(
        name: ".organizer", in: .rootContainer, isFolder: true, contents: nil)
      Issue.record("A .organizer was made at the drive's top")
    } catch let error as NSFileProviderError {
      #expect(error.code == .excludedFromSync)
    }
    do {
      _ = try await client.modify(
        NSFileProviderItemIdentifier("a"), baseVersion: 1, name: ".organizer", parent: nil,
        contents: nil)
      Issue.record("Renamed to .organizer at the drive's top")
    } catch let error as NSFileProviderError {
      #expect(error.code == .cannotSynchronize)
    }
    #expect(TreeServer.current.node("a")?.name == "a.txt")
  }

  @Test func newContentGoesAtTheItemsVersion() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    let client = providerClient()
    let result = try await client.modify(
      NSFileProviderItemIdentifier("a"), baseVersion: 1, name: nil, parent: nil,
      contents: try temporaryFile("hello, edited"))
    #expect(!result.fetchContent)
    #expect(result.conflictCopy == nil)
    #expect(ProviderItem.version(of: result.item.itemVersion.contentVersion) == 2)
    #expect(TreeServer.current.node("a")?.content == Data("hello, edited".utf8))
  }

  @Test func contentChangedOnTheServerSinceBecomesAConflictCopy() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.bump("a", "theirs")
    let client = providerClient()
    let result = try await client.modify(
      NSFileProviderItemIdentifier("a"), baseVersion: 1, name: nil, parent: nil,
      contents: try temporaryFile("mine"))
    // The server's stays, and comes down; mine is beside it.
    #expect(result.fetchContent)
    #expect(TreeServer.current.node("a")?.content == Data("theirs".utf8))
    let copy = try #require(result.conflictCopy)
    #expect(copy.filename == "a (conflict, Mac, 2026-11-08).txt")
    #expect(copy.parentItemIdentifier == .rootContainer)
    #expect(TreeServer.current.node(copy.itemIdentifier.rawValue)?.content == Data("mine".utf8))
  }

  @Test func renamesAndMovesGoAtTheVersionAndAMovedOnOneStands() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.add("f", in: "root", "Taxes", folder: true)
    let client = providerClient()
    let moved = try await client.modify(
      NSFileProviderItemIdentifier("a"), baseVersion: 1, name: "b.txt",
      parent: NSFileProviderItemIdentifier("f"), contents: nil)
    #expect(moved.item.filename == "b.txt")
    #expect(moved.item.parentItemIdentifier.rawValue == "f")
    #expect(!moved.fetchContent)

    // At an old version: the server's name stands, told again.
    let stale = try await client.modify(
      NSFileProviderItemIdentifier("a"), baseVersion: 1, name: "c.txt", parent: nil,
      contents: nil)
    #expect(stale.item.filename == "b.txt")
    #expect(stale.fetchContent)
  }

  @Test func newFilesAndFoldersAreMadeAndATakenNameCollides() async throws {
    let client = providerClient()
    let folder = try await client.create(
      name: "Taxes", in: .rootContainer, isFolder: true, contents: nil)
    #expect(folder.contentType == .folder)
    let file = try await client.create(
      name: "bill.txt", in: folder.itemIdentifier, isFolder: false,
      contents: try temporaryFile("a bill"))
    #expect(file.parentItemIdentifier == folder.itemIdentifier)
    #expect(TreeServer.current.node(file.itemIdentifier.rawValue)?.content == Data("a bill".utf8))
    await #expect(throws: NSFileProviderError.self) {
      try await client.create(
        name: "bill.txt", in: folder.itemIdentifier, isFolder: false,
        contents: try temporaryFile("again"))
    }
  }

  @Test func aDeleteGoesAtTheVersionAndAChangedFileIsKept() async throws {
    TreeServer.current.add("a", in: "root", "a.txt", "hello")
    TreeServer.current.add("b", in: "root", "b.txt", "hello")
    TreeServer.current.bump("b", "changed")
    let client = providerClient()
    try await client.delete(NSFileProviderItemIdentifier("a"), baseVersion: 1)
    #expect(TreeServer.current.node("a") == nil)
    do {
      try await client.delete(NSFileProviderItemIdentifier("b"), baseVersion: 1)
      Issue.record("A changed file was deleted")
    } catch let error as NSFileProviderError {
      #expect(error.code == .deletionRejected)
    }
    #expect(TreeServer.current.node("b") != nil)
  }

  @Test func errorsAreTheSystemsOwn() {
    #expect(
      (providerError(APIError(status: 401, message: "")) as? NSFileProviderError)?.code
        == .notAuthenticated)
    #expect(
      (providerError(APIError(status: 404, message: "")) as? NSFileProviderError)?.code
        == .noSuchItem)
    // A role that does not allow it.
    #expect(
      (providerError(APIError(status: 403, message: "")) as? CocoaError)?.code
        == .fileWriteNoPermission)
    #expect(
      (providerError(URLError(.notConnectedToInternet)) as? NSFileProviderError)?.code
        == .serverUnreachable)
    // A drive whose storage is unavailable, or which is being moved: the
    // server away, which the system tries again later (never a conflict
    // copy of a read-only refusal).
    let locked = APIError(
      status: 503,
      body: Data(
        #"{"error":"Locked until alice@example.com signs in","code":"storage_unavailable"}"#.utf8))
    #expect(locked.isStorageUnavailable)
    #expect((providerError(locked) as? NSFileProviderError)?.code == .serverUnreachable)
    let moving = APIError(
      status: 409,
      body: Data(
        #"{"error":"This drive is being moved to other storage: it is read-only until that is done","code":"read_only"}"#
          .utf8))
    #expect(moving.isReadOnly && !moving.isConflict)
    // Told by its code, not by its words.
    #expect(APIError(status: 409, message: "This drive is being moved").isConflict)
    #expect((providerError(moving) as? NSFileProviderError)?.code == .serverUnreachable)
    #expect(APIError(status: 409, message: "Changed since").isConflict)
    // The other server of a drive from another server away: tried again;
    // refusing: not. By the code.
    let away = APIError(
      status: 502,
      body: Data(#"{"error":"cloud.example: timed out","code":"remote_unreachable"}"#.utf8))
    #expect(away.isRemoteUnreachable)
    #expect(away.errorDescription == "The other server cannot be reached now. Try again later.")
    #expect((providerError(away) as? NSFileProviderError)?.code == .serverUnreachable)
    let refused = APIError(
      status: 502, body: Data(#"{"error":"cloud.example: 403","code":"remote_refused"}"#.utf8))
    #expect(refused.isRemoteRefused && !refused.isRemoteUnreachable)
    #expect(providerError(refused) as? APIError == refused)
  }
}
