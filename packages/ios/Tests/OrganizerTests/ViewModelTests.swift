import Foundation
import Testing

@testable import OrganizerKit

func client(
  _ log: Log = Log(), _ answer: @escaping @Sendable (StubTransport.Request) -> (Int, String)
)
  -> Client
{
  Client.organizer(
    server: server, token: "org_token",
    transport: StubTransport { request in
      log.add("\(request.method) \(request.path)")
      return answer(request)
    })
}

/// Every permission, as on the user's own files.
private let allowed =
  #"{"own":true,"mount":false,"create":true,"change":true,"delete":true,"share":true}"#

@MainActor @Suite struct FolderModelTests {
  @Test func loadsTheRootThenItsChildrenPageByPage() async throws {
    let log = Log()
    let client = client(log) { request in
      switch request.path {
      case "/api/files/root":
        (
          200,
          node("root", "", folder: true).replacingOccurrences(
            of: "}",
            with:
              #","path":"/","permissions":"# + allowed + "}"
          )
        )
      case "/api/files/root/children":
        (
          200,
          #"{"items":[\#(node("a", "Taxes", folder: true))],"next":"p2","mounts":[],"shared":[]}"#
        )
      case "/api/files/root/children?after=p2":
        (200, #"{"items":[\#(node("b", "notes.txt"))],"next":null,"mounts":[],"shared":[]}"#)
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let model = FolderModel(folderID: nil, title: "Files")
    await model.load(client)
    #expect(model.error == nil, "\(log.all)")
    #expect(model.folderID == "root")
    #expect(model.nodes.map(\.name) == ["Taxes"])
    #expect(model.hasMore)
    await model.loadMore(client)
    #expect(model.nodes.map(\.name) == ["Taxes", "notes.txt"])
    #expect(!model.hasMore)
    #expect(model.error == nil)
  }

  @Test func renamesMovesAndDeletesWithTheVersion() async throws {
    let log = Log()
    let bodies = Log()
    let client = Client.organizer(
      server: server, token: "org_token",
      transport: StubTransport { request in
        log.add("\(request.method) \(request.path)")
        if let body = request.body { bodies.add(compact(body)) }
        switch (request.method, request.path) {
        case (.get, _): return (200, #"{"items":[],"next":null,"mounts":[],"shared":[]}"#)
        case (.patch, _): return (200, node("b", "new.txt", version: 4))
        case (.delete, _):
          return (
            200,
            #"{"id":"t","node_id":"b","name":"new.txt","kind":"file","size":1,"original_path":"/new.txt","deleted_at":"x","purge_at":"y"}"#
          )
        default: return (201, node("c", "New", folder: true))
        }
      })
    let model = FolderModel(folderID: "root", title: "Files")
    let file = try JSONDecoder().decode(
      FileNode.self, from: Data(node("b", "old.txt", version: 3).utf8))
    #expect(await model.rename(file, to: "new.txt", client))
    #expect(await model.move(file, to: "folder-2", client))
    #expect(await model.createFolder(named: "New", client))
    _ = await model.delete(file, client)
    #expect(log.all.contains("PATCH /api/files/b"))
    #expect(log.all.contains("POST /api/files/root/folders"))
    #expect(log.all.contains("DELETE /api/files/b?if_version=3"))
    #expect(
      bodies.all.contains { $0.contains(#""if_version":3"#) && $0.contains(#""name":"new.txt""#) })
    #expect(bodies.all.contains { $0.contains(#""parent_id":"folder-2""#) })
  }

  @Test func showsTheServersError() async {
    let client = client { _ in (409, #"{"error":"The name is taken"}"#) }
    let model = FolderModel(folderID: "root", title: "Files")
    #expect(await model.createFolder(named: "Taxes", client) == false)
    #expect(model.error == "The name is taken")
  }
}

func drive(
  _ id: String, _ name: String, shared: Bool = true, root: String? = nil, members: Int = 3,
  role: String? = nil, backend: String = disk, unavailable: String? = nil,
  locked: Bool = false, movingTo: String? = nil, mirrorOf: String? = nil, remote: String? = nil
) -> String {
  let kind = remote != nil ? "remote" : mirrorOf != nil ? "mirror" : shared ? "shared" : "my"
  return """
    {"id":"\(id)","kind":"\(kind)","name":"\(name)","mirror_of":\(mirrorOf ?? "null"),\
    "remote":\(remote ?? "null"),\
    "root_id":\(root.map { "\"\($0)\"" } ?? "null"),"used":1500000,"limit":null,\
    "manages":\(!shared),"members":\(members),"managers":\(shared ? #"["anna@example.com"]"# : "[]"),\
    "team_id":null,"team_name":null,"my_role":"\(role ?? (shared ? "editor" : "owner"))",\
    "backend":\(backend),"available":\(unavailable == nil),\
    "unavailable":\(unavailable == nil ? "null" : locked ? #""locked""# : #""failing""#),\
    "unavailable_reason":\(unavailable.map { "\"\($0)\"" } ?? "null"),\
    "moving_to":\(movingTo ?? "null")}
    """
}

/// Where a drive is stored (`BackendRef`): the server's disk.
let disk = """
  {"id":"00000000-0000-0000-0000-000000000000","name":"The server's disk","kind":"disk",\
  "owner":null,"backed_up":true}
  """

/// Someone's own S3 storage, or a global bucket (`owner` nil).
func bucket(_ name: String, owner: String? = nil) -> String {
  """
  {"id":"0190f2a0-0000-7000-8000-000000000001","name":"\(name)","kind":"s3",\
  "owner":\(owner.map { "\"\($0)\"" } ?? "null"),"backed_up":false}
  """
}

/// Drives in storage backends (docs/M7C_DESIGN.md): an unavailable one
/// (failing, or locked) offers nothing, a moving one is read-only, one on
/// someone's own storage warns its members.
@MainActor @Suite struct DriveStorageTests {
  let drives = """
    [\(drive("d1", "Family", root: "r1")),\
    \(drive("d2", "Locked", root: "r2", backend: bucket("Alice's", owner: "alice@example.com"),
      unavailable: "Locked until alice@example.com signs in", locked: true)),\
    \(drive("d3", "Failing", root: "r3", backend: bucket("S3"),
      unavailable: "The storage cannot be reached")),\
    \(drive("d4", "Moving", root: "r4", movingTo: bucket("S3"))),\
    \(drive("d5", "Alice's", root: "r5", backend: bucket("Alice's", owner: "alice@example.com")))]
    """

  func load() async -> DrivesModel {
    let drives = drives
    let model = DrivesModel()
    await model.load(client { _ in (200, drives) })
    return model
  }

  @Test func mapsEachDrivesStorage() async throws {
    let model = await load()
    #expect(model.error == nil)
    let storage = model.drives.map(\.storage)
    #expect(storage[0] == .ready)
    #expect(
      storage[1]
        == .unavailable(reason: "Locked until alice@example.com signs in", locked: true))
    #expect(storage[2] == .unavailable(reason: "The storage cannot be reached", locked: false))
    #expect(storage[3] == .moving(to: "S3"))
    #expect(storage[4] == .ready)
    // What the list says under the name: the reason, or where it moves.
    #expect(model.drives[1].storageLine == "Locked until alice@example.com signs in")
    #expect(model.drives[3].storageLine == "Moving to S3…")
    #expect(model.drives[4].storageLine == nil)
    // The warning of someone's own storage, and no other.
    #expect(model.drives[0].ownerWarning == nil)
    #expect(model.drives[2].ownerWarning == nil)
    #expect(
      model.drives[4].ownerWarning?.hasPrefix(
        "Stored on alice@example.com's own storage. If they remove it, this drive goes.") == true)
  }

  @Test func anUnavailableDriveOffersNothingAndAMovingOneIsReadOnly() async throws {
    let model = await load()
    // Unavailable: not opened; moving: opened, but not picked to add to.
    #expect(model.drives.compactMap(DrivePlace.init).map(\.id) == ["d1", "d4", "d5"])
    #expect(model.available.map(\.id) == ["d1", "d5"])

    let moving = try #require(DrivePlace(model.drives[3]))
    #expect(moving.readOnly)
    let folder = FolderModel(moving.root)
    #expect(folder.readOnly)
    #expect(folder.actions == [.open])
    #expect(folder.actions(try decoded(node("f", "a.txt"))) == [.open])
    #expect(folder.movingNote == "Moving to S3… Read-only until that is done.")
    #expect(!moving.root.canAdd)

    let own = try #require(DrivePlace(model.drives[4]))
    let ownFolder = FolderModel(own.root)
    #expect(!ownFolder.readOnly)
    #expect(ownFolder.actions.contains(.add))
    #expect(ownFolder.storageWarning?.hasPrefix("Stored on alice@example.com's") == true)
    // A folder in it says the same.
    let inside = FolderModel(ownFolder.choice(try decoded(node("p", "Plans", folder: true))))
    #expect(inside.storageWarning == ownFolder.storageWarning)
  }

  @Test func saysClearlyWhyTheStorageRefused() async {
    let model = FolderModel(folderID: "r2", title: "Locked")
    await model.load(
      client { _ in
        (503, #"{"error":"Locked until alice@example.com signs in","code":"storage_unavailable"}"#)
      })
    #expect(
      model.error == "This drive is not available now: Locked until alice@example.com signs in.")
  }

  private func decoded(_ json: String) throws -> FileNode {
    try JSONDecoder().decode(FileNode.self, from: Data(json.utf8))
  }
}

/// What a mirror's `mirror_of` says (docs/M7D_DESIGN.md §9).
func mirrorOf(_ status: String, stopped: String? = nil) -> String {
  """
  {"drive_id":"d1","drive_name":"Family","schedule":"continuous","status":"\(status)",\
  "stopped":\(stopped.map { "\"\($0)\"" } ?? "null"),"restores_into_drive":true}
  """
}

/// Mirrors (docs/M7D_DESIGN.md §5): a group of their own after the shared
/// drives, labelled as mirrors with their state, browsed read-only, never
/// offered by a picker.
@MainActor @Suite struct MirrorTests {
  let drives = """
    [\(drive("my", "My drive", shared: false, root: "r0", members: 1)),\
    \(drive("d1", "Family", root: "r1")),\
    \(drive("m1", "Family", root: "m1r", role: "viewer",
      backend: bucket("Anna's", owner: "anna@example.com"), mirrorOf: mirrorOf("up_to_date"))),\
    \(drive("m2", "Studio", root: "m2r", role: "viewer", mirrorOf: mirrorOf("copying"))),\
    \(drive("m3", "Archive", root: "m3r", role: "viewer", mirrorOf: mirrorOf("waiting_for_unlock"))),\
    \(drive("m4", "Old", root: "m4r", role: "viewer", mirrorOf: mirrorOf("failing"))),\
    \(drive("m5", "Gone", root: "m5r", role: "viewer",
      mirrorOf: mirrorOf("stopped", stopped: "drive_deleted")))]
    """

  func load() async -> DrivesModel {
    let drives = drives
    let model = DrivesModel()
    await model.load(client { _ in (200, drives) })
    return model
  }

  @Test func listsTheMirrorsInTheirOwnGroupWithTheirState() async {
    let model = await load()
    #expect(model.error == nil)
    #expect(model.ownAndShared.map(\.id) == ["my", "d1"])
    #expect(model.mirrors.map(\.id) == ["m1", "m2", "m3", "m4", "m5"])
    #expect(model.mirrors[0].title == "Family · mirror")
    #expect(model.drives[1].title == "Family")
    #expect(
      model.mirrors.map(\.mirrorLine) == [
        "Up to date", "Copying…", "Waiting for your storage to be unlocked",
        "Copying fails; it is tried again in a few minutes", "Stopped: The drive was deleted",
      ])
    #expect(model.drives[1].mirrorLine == nil)
    // A mirror is in the user's own storage: no warning of it.
    #expect(model.mirrors[0].ownerWarning == nil)
    #expect(model.mirrors[0].summary == nil)
  }

  @Test func aMirrorIsBrowsedReadOnlyAndNeverPicked() async throws {
    let model = await load()
    #expect(model.available.map(\.id) == ["my", "d1"])
    let mirror = try #require(DrivePlace(model.mirrors[0]))
    #expect(mirror.isMirror && mirror.readOnly)
    #expect(mirror.name == "Family · mirror")
    let folder = FolderModel(mirror.root)
    #expect(folder.title == "Family · mirror")
    #expect(folder.actions == [.open])
    #expect(!mirror.root.canAdd)
    let file = try JSONDecoder().decode(
      FileNode.self, from: Data(node("f", "a.txt", role: "viewer").utf8))
    #expect(folder.actions(file) == [.open])
    // A folder in it stays in the mirror, read-only too.
    let inside = try JSONDecoder().decode(
      FileNode.self, from: Data(node("p", "Plans", folder: true, role: "viewer").utf8))
    #expect(!FolderModel(folder.choice(inside)).actions.contains(.add))
  }
}

/// Where a drive or an item from another server comes from
/// (`RemoteDrive`, docs/M7E_DESIGN.md §5).
func remoteFrom(
  _ owner: String = "alice@cloud.example", server: String = "cloud.example",
  pending: Bool = false, error: String? = nil
) -> String {
  """
  {"server":"\(server)","owner":"\(owner)","owner_name":"Alice","shared_by":"\(owner)",\
  "state":"\(pending ? "pending" : "accepted")","error":\(error.map { "\"\($0)\"" } ?? "null")}
  """
}

/// Drives and items from other servers (docs/M7E_DESIGN.md §5, §6): a group
/// of their own after the mirrors, with whose they are, where and how they
/// stand; browsed by their role once accepted, Accept and Decline before;
/// never offered by a picker.
@MainActor @Suite struct RemoteTests {
  let drives = """
    [\(drive("my", "My drive", shared: false, root: "r0", members: 1)),\
    \(drive("d1", "Family", root: "r1")),\
    \(drive("m1", "Family", root: "m1r", role: "viewer", mirrorOf: mirrorOf("up_to_date"))),\
    \(drive("x1", "Plans", root: "x1r", role: "editor", backend: bucket("Alice's", owner: "alice"),
      remote: remoteFrom())),\
    \(drive("x2", "Photos", root: "x2r", role: "viewer",
      remote: remoteFrom("bob@example.com", server: "files.example.org", pending: true))),\
    \(drive("x3", "Old", root: "x3r", role: "viewer", remote: remoteFrom(error: "timed out")))]
    """

  func load(_ log: Log = Log()) async -> (DrivesModel, Client) {
    let drives = drives
    let client = client(log) { request in
      request.path == "/api/drives" ? (200, drives) : (204, "")
    }
    let model = DrivesModel()
    await model.load(client)
    return (model, client)
  }

  @Test func listsTheRemoteDrivesInTheirOwnGroupWithWhoseAndWhere() async {
    let (model, _) = await load()
    #expect(model.error == nil)
    #expect(model.ownAndShared.map(\.id) == ["my", "d1"])
    #expect(model.mirrors.map(\.id) == ["m1"])
    #expect(model.remotes.map(\.id) == ["x1", "x2", "x3"])
    #expect(
      model.remotes.map { $0.remote?.fromLine } == [
        "From alice@cloud.example", "From bob@example.com on files.example.org",
        "From alice@cloud.example",
      ])
    #expect(
      model.remotes.map { $0.remote?.stateLine } == [
        nil, "Waiting for you to accept it", "Its server cannot be reached now: timed out",
      ])
    // Stored on the other server: no warning of anyone's own storage here.
    #expect(model.remotes[0].ownerWarning == nil)
    #expect(model.remotes[0].title == "Plans")
  }

  @Test func anAcceptedOneIsBrowsedByItsRoleAndNeverPicked() async throws {
    let (model, _) = await load()
    // Nothing new is put on another server by a picker.
    #expect(model.available.map(\.id) == ["my", "d1"])
    let plans = try #require(DrivePlace(model.remotes[0]))
    #expect(plans.isRemote && !plans.readOnly)
    let folder = FolderModel(plans.root)
    #expect(folder.actions.contains(.add))
    #expect(plans.root.canAdd)
    // Not accepted yet: it does not open.
    #expect(DrivePlace(model.remotes[1]) == nil)
    let viewed = try #require(DrivePlace(model.remotes[2]))
    #expect(!FolderModel(viewed.root).actions.contains(.add))
  }

  @Test func aPendingDriveIsAcceptedOrDeclinedByItsRoot() async {
    let log = Log()
    let (model, client) = await load(log)
    await model.answer(model.remotes[1], accept: true, client: client)
    await model.answer(model.remotes[1], accept: false, client: client)
    #expect(model.error == nil)
    #expect(
      log.all == [
        "GET /api/drives", "POST /api/shared/x2r/accept", "GET /api/drives",
        "DELETE /api/shared/x2r", "GET /api/drives",
      ])
  }

  @Test func sharedWithMeMarksWhatCameFromAnotherServer() async throws {
    let log = Log()
    let shared = """
      [{"node":\(node("s1", "Recipes", folder: true, role: "viewer", remote: "cloud.example")),\
      "owner":{"id":"00000000-0000-0000-0000-000000000000","email":"alice@cloud.example"},\
      "role":"viewer","shared_by":{"id":"00000000-0000-0000-0000-000000000000",\
      "email":"alice@cloud.example"},"group":null,"permissions":\(allowed),\
      "shared_at":"2026-11-07T10:00:00Z","new":true,"placed":null,"remote":\(remoteFrom(pending: true))},\
      {"node":\(node("s2", "Notes", folder: true, role: "editor")),\
      "owner":{"id":"u2","email":"bob@example.com"},"role":"editor",\
      "shared_by":{"id":"u2","email":"bob@example.com"},"group":null,"permissions":\(allowed),\
      "shared_at":"2026-11-07T10:00:00Z","new":false,"placed":null,"remote":null}]
      """
    let client = client(log) { request in
      request.path == "/api/shared" ? (200, shared) : (204, "")
    }
    let model = SharedModel()
    await model.load(client)
    #expect(model.items.map(\.fromLine) == ["From alice@cloud.example", "From bob@example.com"])
    #expect(model.items.map(\.awaitsAnswer) == [true, false])
    #expect(SharedModel.place(model.items[0].node).isRemote)
    #expect(!SharedModel.place(model.items[1].node).isRemote)
    await model.answer(model.items[0], accept: true, client: client)
    await model.answer(model.items[0], accept: false, client: client)
    #expect(model.error == nil)
    #expect(
      log.all == [
        "GET /api/shared", "POST /api/shared/s1/accept", "GET /api/shared",
        "DELETE /api/shared/s1", "GET /api/shared",
      ])
  }

  @Test func aRefusalIsToldByItsCode() async {
    let (model, _) = await load()
    let failing = client { _ in
      (502, #"{"error":"files.example.org: connection refused","code":"remote_unreachable"}"#)
    }
    await model.answer(model.remotes[1], accept: true, client: failing)
    #expect(model.error == "The other server cannot be reached now. Try again later.")
  }
}

@MainActor @Suite struct DrivesModelTests {
  @Test func listsTheDrivesAndOpensOneAtItsTop() async throws {
    let log = Log()
    let drives = """
      [\(drive("my", "My drive", shared: false, root: "r0", members: 1)),\
      \(drive("d1", "Family", root: "r1")),\(drive("d2", "Archive")),\
      \(drive("d3", "Studio", root: "r3", members: 1))]
      """
    let client = client(log) { request in
      switch request.path {
      case "/api/drives": (200, drives)
      case "/api/files/r1/children":
        (200, #"{"items":[\#(node("p", "Plans", folder: true))],"mounts":[],"shared":[]}"#)
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let model = DrivesModel()
    await model.load(client)
    #expect(model.error == nil)
    #expect(model.drives.map(\.name) == ["My drive", "Family", "Archive", "Studio"])
    // My drive says nothing under its name; a shared drive its members and size.
    #expect(model.drives[0].summary == nil)
    #expect(model.drives[1].summary?.hasPrefix("3 members · ") == true)
    #expect(model.drives[3].summary?.hasPrefix("1 member · ") == true)
    // A drive whose folder is not available cannot be opened nor picked.
    #expect(model.available.map(\.id) == ["my", "d1", "d3"])

    // Its top opens in the folder view, titled with the drive's name; the
    // folders in it stay in the drive (Move does not leave it).
    let family = try #require(model.available.first { $0.id == "d1" })
    let folder = FolderModel(family.root)
    #expect(folder.title == "Family")
    #expect(folder.isDriveRoot)
    await folder.load(client)
    #expect(folder.nodes.map(\.name) == ["Plans"])
    let plans = folder.choice(folder.nodes[0])
    #expect(plans.drive == family)
    #expect(!FolderModel(plans).isDriveRoot)
    #expect(log.all == ["GET /api/drives", "GET /api/files/r1/children"])
  }

  @Test func showsTheServersError() async {
    let model = DrivesModel()
    await model.load(client { _ in (500, #"{"error":"Something broke"}"#) })
    #expect(model.error == "Something broke")
    #expect(!model.loaded)
  }

  @Test func sharedWithMeOpensAFolderAsTheTopOfItsTree() async throws {
    let shared = """
      [{"node":\(node("s1", "Recipes", folder: true, role: "viewer")),\
      "owner":{"id":"u2","email":"bob@example.com"},"role":"viewer",\
      "shared_by":{"id":"u2","email":"bob@example.com"},"group":null,"permissions":\(allowed),\
      "shared_at":"2026-11-07T10:00:00Z","new":true,"placed":null}]
      """
    let client = client { request in
      request.path == "/api/shared" ? (200, shared) : (404, #"{"error":"Not found"}"#)
    }
    let model = SharedModel()
    await model.load(client)
    #expect(model.items.map(\.node.name) == ["Recipes"])
    let place = SharedModel.place(model.items[0].node)
    #expect(place == DrivePlace(id: "d1", name: "Recipes", rootID: "s1", role: .viewer))
    // Shared to view: it opens, and offers nothing to add.
    let folder = FolderModel(place.root)
    #expect(folder.isDriveRoot)
    #expect(folder.role == .viewer)
    #expect(!folder.actions.contains(.add))
  }
}

@MainActor @Suite struct DropBoxModelTests {
  let items = """
    [\(item("1", status: "proposed", proposal: true)),\(item("2", status: "analysing")),\
    \(item("3", status: "needs_manual")),\(item("4", status: "confirmed", proposal: true, placement: true)),\
    \(item("5", status: "queued"))]
    """

  @Test func sortsTheItemsIntoSections() async {
    let items = self.items
    let client = client { request in
      request.path.hasPrefix("/api/drop-box/settings") ? (200, settingsJSON) : (200, items)
    }
    let model = DropBoxModel()
    await model.load(client)
    #expect(model.toReview.map(\.id) == ["1"])
    #expect(model.analysing.map(\.id) == ["2", "5"])
    #expect(model.needsPlace.map(\.id) == ["3"])
    #expect(model.placed.map(\.id) == ["4"])
    #expect(model.toReview[0].confidenceText == "87 %")
    #expect(model.toReview[0].proposal?.folderPath == "/Taxes/2026")
    #expect(
      model.options
        == DropOptions(offerReadContents: true, readContentsByDefault: false, linkMode: .snapshot))
  }

  @Test func confirmsElsewhereRejectsAndUndoes() async {
    let items = self.items
    let log = Log()
    let client = client(log) { request in
      switch (request.method, request.path) {
      case (.get, let path) where path.hasPrefix("/api/drop-box/settings"): (200, settingsJSON)
      case (.get, _): (200, items)
      case (_, "/api/drop-box/items/1/confirm"):
        (200, item("1", status: "confirmed", proposal: true, placement: true))
      case (_, "/api/drop-box/items/3/place"): (200, item("3", status: "changed", placement: true))
      case (_, "/api/placements/p-4/undo"): (200, item("4", status: "undone"))
      default: (200, item("1", status: "rejected"))
      }
    }
    let model = DropBoxModel()
    await model.load(client)
    await model.confirm(model.toReview[0], client)
    #expect(model.toReview.isEmpty)
    #expect(model.placed.map(\.id).contains("1"))
    await model.placeElsewhere(
      model.needsPlace[0], in: FolderChoice(id: "f9", name: "Other"), client)
    #expect(model.needsPlace.isEmpty)
    let placed = model.placed.first { $0.id == "4" }!
    await model.undo(placed, client)
    #expect(model.needsPlace.map(\.id) == ["4"])
    #expect(
      log.all.filter { $0.hasPrefix("POST") } == [
        "POST /api/drop-box/items/1/confirm", "POST /api/drop-box/items/3/place",
        "POST /api/placements/p-4/undo",
      ])
  }

  @Test func refusesALinkThatIsNotOne() async {
    let model = DropBoxModel()
    #expect(await model.addLink("not a link", note: nil, client { _ in (201, "{}") }) == false)
    #expect(model.error != nil)
  }
}

@MainActor @Suite struct ShareModelTests {
  @Test func offersWhatTheSettingsAllow() throws {
    let settings = try JSONDecoder().decode(DropSettings.self, from: Data(settingsJSON.utf8))
      .settings
    let perItem = DropOptions(settings)
    #expect(perItem.offerReadContents && !perItem.readContentsByDefault)
    #expect(perItem.readContents(true) == true)
    var always = settings
    always.readContents = .always
    #expect(DropOptions(always).readContents(false) == nil)
    var on = settings
    on.readContents = .defaultOn
    #expect(DropOptions(on).readContentsByDefault)
  }

  @Test func aLinkGoesToTheDropBoxOnly() {
    let model = ShareModel(files: [], link: "https://example.com")
    #expect(!model.canPlace)
    #expect(model.canSend)
    #expect(model.summary == "example.com")
    let files = ShareModel(
      files: [SharedFile(url: URL(filePath: "/tmp/a"), name: "a.pdf", size: 1)], link: nil)
    files.mode = .place
    #expect(!files.canSend)
    files.folder = FolderChoice(id: "f", name: "Taxes")
    #expect(files.canSend)
  }

  @Test func sendsToTheDropBoxWithTheOptions() async throws {
    FakeServer.current = FakeServer()
    let queue = makeQueue()
    let source = try file(10)
    let model = ShareModel(files: [SharedFile(url: source, name: "bill.pdf", size: 10)], link: nil)
    model.apply(
      DropOptions(offerReadContents: true, readContentsByDefault: true, linkMode: .bookmark))
    model.note = "for taxes"
    model.readContents = false
    #expect(await model.send(queue))
    try await settle(queue)
    let body = String(decoding: try #require(FakeServer.current.posts.first).body, as: UTF8.self)
    #expect(body.contains("for taxes"))
    #expect(body.contains("name=\"read_contents\"\r\n\r\nfalse"))
  }
}

@MainActor @Suite struct AppModelTests {
  let session = StoredSession(
    server: server, token: "org_token", keyID: "key-1", keyPrefix: "org_tok", deviceName: "iPhone")

  func app(_ answer: @escaping @Sendable (StubTransport.Request) throws -> (Int, String)) throws
    -> AppModel
  {
    let store = SessionStore(store: MemoryStore())
    try store.save(session)
    return AppModel(
      sessions: store, uploads: UploadQueue(root: temporaryFolder()), pushStore: MemoryStore(),
      encryption: Encryption(store: MemoryStore()),
      makeClient: { server, token, unauthorized in
        Client.organizer(
          server: server, token: token, transport: StubTransport { try answer($0) },
          onUnauthorized: unauthorized)
      }, makeEvents: { _ in AsyncStream { $0.finish() } })
  }

  @Test func signOutRevokesTheToken() async throws {
    let log = Log()
    let app = try app { request in
      log.add("\(request.method) \(request.path) \(request.authorization ?? "")")
      return (204, "")
    }
    await app.signOut()
    #expect(app.session == nil)
    #expect(app.notice == nil)
    #expect(log.all == ["DELETE /api/auth/device Bearer org_token"])
  }

  @Test func signOutOfflineSaysWhereToRevoke() async throws {
    let app = try app { _ in throw URLError(.notConnectedToInternet) }
    await app.signOut()
    #expect(app.session == nil)
    #expect(app.notice?.contains("Settings → Devices") == true)
  }

  @Test func showsWhoIsSignedIn() async throws {
    let app = try app { request in
      request.path == "/api/auth/session"
        ? (
          200,
          """
          {"needsSetup":false,"password_login":true,"totp_required":false,"files_locked":false,"passkeys":false,"signup":false,\
          "oidc":null,"user":{"id":"u1","email":"anna@example.com","is_superadmin":true,\
          "has_password":true,"single_sign_on":false,"totp_enabled":false}}
          """
        ) : (500, "")
    }
    await app.refreshAccount()
    #expect(app.email == "anna@example.com")
  }

  @Test func a401SignsOut() async throws {
    let app = try app { _ in (401, #"{"error":"Not signed in"}"#) }
    await app.refreshBadge()
    // The sign-out is a task of its own on the main actor.
    for _ in 0..<50 where app.session != nil { try await Task.sleep(nanoseconds: 10_000_000) }
    #expect(app.session == nil)
    #expect(app.notice != nil)
  }

  @Test func countsTheItemsToReview() async throws {
    let log = Log()
    let app = try app { request in
      log.add(request.path)
      return (200, "[\(item("1", status: "proposed")),\(item("2", status: "proposed"))]")
    }
    await app.refreshBadge()
    #expect(app.toReview == 2)
    #expect(log.all == ["/api/drop-box/items?status=proposed&limit=500"])
  }
}
