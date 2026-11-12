import FileProvider
import Foundation

/// What the working set follows of the change feed, which tells of all
/// the user's drives: the folders of the domain the extension has seen
/// (its own folder, the ones Finder listed, the ones the feed brought), so
/// that a change is told to the system only when it is in the domain. As
/// the sync engine's mirror does (`apply_change` in `packages/sync`), with
/// what the extension needs and no more: a change under a folder never
/// listed is not needed, as Finder lists it fresh when it is opened.
///
/// A domain's folder is a drive's top or a folder in one
/// (docs/M7A_DESIGN.md): once its drive is known (`learn`), the changes of
/// the other drives are not looked at, and the delete of the domain's
/// folder or of its drive's top (the user left the drive, or it was
/// deleted) is told apart, never as items gone: the extension checks it
/// and reports the folder unavailable, and the local files stay.
public struct ProviderScope: Codable, Sendable, Equatable {
  public let rootID: String
  public private(set) var folders: Set<String>
  /// The drive the domain's folder is in, once learned.
  public private(set) var driveID: String?
  /// That drive's top (the domain's folder itself, or above it).
  public private(set) var driveRootID: String?

  /// What a page of the feed means here: the nodes to tell as updated
  /// (their items are asked for), those to tell as gone, and whether the
  /// domain's folder (or its drive) was deleted.
  public struct Applied: Sendable, Equatable {
    public var updated: [String] = []
    public var deleted: [String] = []
    public var rootDeleted = false
  }

  /// The name of a drive's own folder (its trash and versions) at its
  /// top: the server never lists it, and refuses that name there.
  public static let reserved = ".organizer"

  public init(rootID: String, driveID: String? = nil, driveRootID: String? = nil) {
    self.rootID = rootID
    self.folders = [rootID]
    self.driveID = driveID
    self.driveRootID = driveRootID
  }

  /// The domain's drive and that drive's top.
  public mutating func learn(driveID: String, driveRootID: String?) {
    self.driveID = driveID
    self.driveRootID = driveRootID
  }

  /// A folder's listing: its folders are in the domain.
  public mutating func listed(_ nodes: [FileNode], in folder: String) {
    guard folders.contains(folder) else { return }
    for node in nodes where node.isFolder { folders.insert(node.id) }
  }

  public mutating func apply(_ changes: [Components.Schemas.Change]) -> Applied {
    var applied = Applied()
    for change in changes {
      // Another drive's: never in this domain (the server refuses moves
      // between drives).
      if let driveID, change.driveId != driveID { continue }
      let id = change.nodeId
      if id == rootID || id == driveRootID {
        if change.op == "delete" { applied.rootDeleted = true }
        continue
      }
      if change.name == Self.reserved, let parent = change.parentId, parent == driveRootID {
        continue
      }
      applied.updated.removeAll { $0 == id }
      applied.deleted.removeAll { $0 == id }
      if change.op == "delete" {
        folders.remove(id)
        applied.deleted.append(id)
        continue
      }
      let inScope = change.parentId.map(folders.contains) ?? false
      if inScope {
        if change.kind == .folder { folders.insert(id) }
        applied.updated.append(id)
      } else if change.op == "move" {
        // Moved out of the domain (or never in it: telling the system of
        // an item it does not know is harmless).
        folders.remove(id)
        applied.deleted.append(id)
      }
    }
    return applied
  }
}

/// A domain's scope, kept in a file between the extension's runs.
public actor ProviderScopeStore {
  private let url: URL
  private var scope: ProviderScope

  public init(url: URL, rootID: String) {
    self.url = url
    if let data = try? Data(contentsOf: url),
      let saved = try? JSONDecoder().decode(ProviderScope.self, from: data),
      saved.rootID == rootID
    {
      scope = saved
    } else {
      scope = ProviderScope(rootID: rootID)
    }
  }

  public var current: ProviderScope { scope }

  public func listed(_ nodes: [FileNode], in folder: String) {
    scope.listed(nodes, in: folder)
    save()
  }

  public func apply(_ changes: [Components.Schemas.Change]) -> ProviderScope.Applied {
    let result = scope.apply(changes)
    save()
    return result
  }

  public func learn(driveID: String, driveRootID: String?) {
    scope.learn(driveID: driveID, driveRootID: driveRootID)
    save()
  }

  /// The feed's cursor was too old: everything is listed again (the
  /// drive stays what it was).
  public func reset() {
    scope = ProviderScope(
      rootID: scope.rootID, driveID: scope.driveID, driveRootID: scope.driveRootID)
    save()
  }

  private func save() {
    try? FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try? JSONEncoder().encode(scope).write(to: url, options: .atomic)
  }
}
