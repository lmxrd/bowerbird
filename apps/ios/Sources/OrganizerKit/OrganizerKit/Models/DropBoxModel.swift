import Foundation
import Observation

/// A folder chosen in the folder picker.
public struct FolderChoice: Sendable, Equatable, Hashable {
  public var id: String
  public var name: String
  /// The encrypted folder it is in (its own id for the top), nil when it
  /// is not encrypted.
  public var e2eRoot: String?
  /// The drive it is in, when known: Move stays in it.
  public var drive: DrivePlace?
  /// The user's role on it (`my_role`), when known.
  public var role: Role?
  /// In a mirror (docs/M7D_DESIGN.md): the folder of the drive mirrored
  /// that it copies (`source_id`), whose id an encrypted folder's keys and
  /// names are sealed with.
  public var sourceID: String?

  public init(
    id: String, name: String, e2eRoot: String? = nil, drive: DrivePlace? = nil, role: Role? = nil,
    sourceID: String? = nil
  ) {
    self.id = id
    self.name = name
    self.e2eRoot = e2eRoot
    self.drive = drive
    self.role = role
    self.sourceID = sourceID
  }

  /// A folder of the tree, in `drive`.
  public init(_ node: FileNode, drive: DrivePlace? = nil) {
    self.init(
      id: node.id, name: node.name, e2eRoot: node.e2eRoot, drive: drive, role: node.myRole,
      sourceID: node.sourceId)
  }

  /// What can be put in it: files from the share sheet, a drop's item,
  /// something moved (upload only and up; not a viewer's), and not in a
  /// drive being moved (read-only meanwhile).
  public var canAdd: Bool {
    drive?.readOnly != true && ItemActions(role: role, isFolder: true).contains(.add)
  }
}

/// What the share sheet and the drop box offer per item, by the user's
/// drop box settings: the "read contents" toggle only where the settings
/// leave it to each item, and what a link becomes.
public struct DropOptions: Sendable, Equatable {
  public var offerReadContents: Bool
  public var readContentsByDefault: Bool
  public var linkMode: Components.Schemas.Links

  public init(
    offerReadContents: Bool, readContentsByDefault: Bool, linkMode: Components.Schemas.Links
  ) {
    self.offerReadContents = offerReadContents
    self.readContentsByDefault = readContentsByDefault
    self.linkMode = linkMode
  }

  public init(_ settings: Components.Schemas.DropBoxSettings) {
    switch settings.readContents {
    case .always:
      self.init(offerReadContents: false, readContentsByDefault: true, linkMode: settings.links)
    case .perItem:
      self.init(offerReadContents: true, readContentsByDefault: false, linkMode: settings.links)
    case .defaultOn:
      self.init(offerReadContents: true, readContentsByDefault: true, linkMode: settings.links)
    }
  }

  /// What to send as `read_contents`: nothing where it is not the item's
  /// to say.
  public func readContents(_ toggle: Bool) -> Bool? {
    offerReadContents ? toggle : nil
  }
}

/// The drop box screen: the items to review, those being analysed, those
/// that need a place, and those placed (with Undo).
@MainActor
@Observable
public final class DropBoxModel {
  public private(set) var items: [DropItem] = []
  public private(set) var loading = false
  public private(set) var loaded = false
  public private(set) var options: DropOptions?
  public var error: String?
  /// The items an action is under way for.
  public private(set) var busy: Set<String> = []

  public init() {}

  public var toReview: [DropItem] { items.filter { $0.item.status == .proposed } }
  public var analysing: [DropItem] {
    items.filter { [.queued, .analysing].contains($0.item.status) }
  }
  public var needsPlace: [DropItem] {
    items.filter { [.waiting, .needsManual, .failed, .rejected, .undone].contains($0.item.status) }
  }
  public var placed: [DropItem] {
    items.filter { [.confirmed, .changed, .autoAccepted].contains($0.item.status) }
  }

  public func load(_ client: Client) async {
    loading = true
    defer { loading = false }
    do {
      items = try await client.listDropBoxItems(query: .init(limit: 100)).ok.body.json
      loaded = true
      error = nil
    } catch {
      self.error = describe(error)
    }
    if options == nil,
      let settings = try? await client.getDropBoxSettings().ok.body.json
    {
      options = DropOptions(settings.settings)
    }
  }

  public func confirm(_ item: DropItem, _ client: Client) async {
    await act(item, client) {
      try await client.confirmDropBoxItem(path: .init(id: item.id)).ok.body.json
    }
  }

  /// Confirmed, but in another folder than proposed.
  public func placeElsewhere(_ item: DropItem, in folder: FolderChoice, _ client: Client) async {
    await act(item, client) {
      if item.item.status == .proposed {
        return try await client.confirmDropBoxItem(
          path: .init(id: item.id), body: .json(.init(folderId: folder.id))
        ).ok.body.json
      }
      return try await client.placeDropBoxItem(
        path: .init(id: item.id), body: .json(.init(folderId: folder.id))
      ).ok.body.json
    }
  }

  public func reject(_ item: DropItem, _ client: Client) async {
    await act(item, client) {
      try await client.rejectDropBoxItem(path: .init(id: item.id)).ok.body.json
    }
  }

  public func retry(_ item: DropItem, _ client: Client) async {
    await act(item, client) {
      try await client.retryDropBoxItem(path: .init(id: item.id)).ok.body.json
    }
  }

  public func undo(_ item: DropItem, _ client: Client) async {
    guard let placement = item.placement else { return }
    await act(item, client) {
      try await client.undoPlacement(path: .init(id: placement.id)).ok.body.json
    }
  }

  /// A link, sent at once.
  @discardableResult
  public func addLink(_ url: String, note: String?, _ client: Client) async -> Bool {
    let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let parsed = URL(string: trimmed),
      ["http", "https"].contains(parsed.scheme?.lowercased())
    else {
      error = "Enter a link that starts with http:// or https://."
      return false
    }
    do {
      _ = try await client.dropLink(body: .json(.init(note: note, url: trimmed))).created
      await load(client)
      return true
    } catch {
      self.error = describe(error)
      return false
    }
  }

  /// An action on one item: the answer replaces it in the list.
  private func act(_ item: DropItem, _ client: Client, _ action: () async throws -> DropItem) async
  {
    busy.insert(item.id)
    defer { busy.remove(item.id) }
    do {
      let updated = try await action()
      if let index = items.firstIndex(where: { $0.id == updated.id }) {
        items[index] = updated
      }
      error = nil
    } catch {
      self.error = describe(error)
      await load(client)
    }
  }
}

extension Components.Schemas.ItemView {
  /// Confidence as shown: "87 %".
  public var confidenceText: String? {
    proposal.map { "\(Int(($0.confidence * 100).rounded())) %" }
  }

  /// What the analysing row says.
  public var stateText: String {
    switch item.status {
    case .queued: "Queued"
    case .analysing: "Analysing…"
    case .waiting: "Waiting for the AI connection"
    case .needsManual: "Needs a place"
    case .failed: item.error ?? "The analysis failed"
    case .rejected: "Rejected"
    case .undone: "Undone"
    case .proposed: "To review"
    case .confirmed: "Placed"
    case .changed: "Placed elsewhere"
    case .autoAccepted: "Placed by itself"
    }
  }
}
