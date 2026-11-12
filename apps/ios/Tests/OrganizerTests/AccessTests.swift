import Foundation
import Testing

@testable import OrganizerKit

/// What the app offers by `my_role` (docs/M7B_DESIGN.md §1, §7), as the
/// server allows it (`Role::allows`).
@MainActor @Suite struct AccessTests {
  @Test func theRolesAreOrderedAsTheServerOrdersThem() {
    #expect(
      [Role.owner, .viewer, .manager, .uploadOnly, .editor].sorted()
        == [.uploadOnly, .viewer, .editor, .manager, .owner])
    #expect(Role.allCases.filter(\.canAdd) == [.uploadOnly, .editor, .manager, .owner])
    #expect(Role.allCases.filter(\.canChange) == [.editor, .manager, .owner])
  }

  @Test func eachRoleOffersItsActions() {
    let everything: ItemActions = [.open, .add, .rename, .move, .delete]
    for role in [Role.editor, .manager, .owner] {
      #expect(ItemActions(role: role, isFolder: true) == everything)
      #expect(ItemActions(role: role, isFolder: false) == [.open, .rename, .move, .delete])
    }
    // A viewer: no rename, move, delete, new folder or upload.
    #expect(ItemActions(role: .viewer, isFolder: true) == .open)
    #expect(ItemActions(role: .viewer, isFolder: false) == .open)
    // Upload only: adds, and changes nothing (not even what it added).
    #expect(ItemActions(role: .uploadOnly, isFolder: true) == [.open, .add])
    #expect(ItemActions(role: .uploadOnly, isFolder: false) == .open)
    // No role known: everything, the server decides.
    #expect(ItemActions(role: nil, isFolder: true) == everything)
  }

  @Test func aNodeOffersWhatItsRoleAllows() throws {
    let decode = { (json: String) throws -> FileNode in
      try JSONDecoder().decode(FileNode.self, from: Data(json.utf8))
    }
    #expect(try decode(node("a", "a.txt", role: "viewer")).actions == .open)
    #expect(
      try decode(node("f", "Inbox", folder: true, role: "upload_only")).actions == [.open, .add])
    #expect(try decode(node("b", "b.txt", role: "editor")).actions.contains(.delete))
  }

  @Test func aFolderKnowsItsRoleFromWhereItWasOpened() async throws {
    // My drive's top: from `GET /files/root`.
    let client = client { request in
      switch request.path {
      case "/api/files/root":
        (
          200,
          String(node("root", "", folder: true).dropLast())
            + #","path":"/","permissions":{"own":true,"mount":false,"create":true,"change":true,"delete":true,"share":true}}"#
        )
      case "/api/files/root/children":
        (
          200,
          #"{"items":[\#(node("v", "Plans", folder: true, role: "viewer"))],"next":null,"mounts":[],"shared":[]}"#
        )
      default: (404, #"{"error":"Not found"}"#)
      }
    }
    let top = FolderModel(folderID: nil, title: "Files")
    await top.load(client)
    #expect(top.role == .owner)
    #expect(top.actions.contains(.add))
    // A folder in it: its own `my_role`.
    let plans = FolderModel(top.choice(top.nodes[0]))
    #expect(plans.role == .viewer)
    #expect(!plans.actions.contains(.add))
    #expect(plans.current?.canAdd == false)

    // A drive's top: the drive's role.
    let family = try JSONDecoder().decode(
      Drive.self,
      from: Data(drive("d1", "Family", root: "r1", role: "upload_only").utf8))
    let place = try #require(DrivePlace(family))
    let familyTop = FolderModel(place.root)
    #expect(familyTop.role == .uploadOnly)
    #expect(familyTop.isUploadOnly)
    #expect(familyTop.actions == [.open, .add])
  }

  @Test func onlyAFolderOneMayAddToCanBeSentTo() {
    let model = ShareModel(
      files: [SharedFile(url: URL(filePath: "/tmp/a"), name: "a.pdf", size: 1)], link: nil)
    model.mode = .place
    model.folder = FolderChoice(id: "f", name: "Plans", role: .viewer)
    #expect(!model.canSend)
    model.folder = FolderChoice(id: "f", name: "Inbox", role: .uploadOnly)
    #expect(model.canSend)
    model.folder = FolderChoice(id: "f", name: "Taxes", role: .editor)
    #expect(model.canSend)
  }
}
