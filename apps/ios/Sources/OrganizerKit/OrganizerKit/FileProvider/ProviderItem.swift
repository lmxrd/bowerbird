import FileProvider
import Foundation
import UniformTypeIdentifiers

/// A node of the server as Finder (or, later, the Files app) shows it, in
/// the File Provider domain of one synced folder (docs/M5B_DESIGN.md §2):
/// the item's identifier is the node's id, but the domain's own folder is
/// the root container.
public final class ProviderItem: NSObject, NSFileProviderItem, @unchecked Sendable {
  public let node: FileNode
  public let rootID: String

  public init(_ node: FileNode, rootID: String) {
    self.node = node
    self.rootID = rootID
  }

  public var itemIdentifier: NSFileProviderItemIdentifier {
    Self.identifier(node.id, rootID: rootID)
  }

  public var parentItemIdentifier: NSFileProviderItemIdentifier {
    guard node.id != rootID, let parent = node.parentId else { return .rootContainer }
    return Self.identifier(parent, rootID: rootID)
  }

  public var filename: String { node.name }

  public var contentType: UTType {
    if node.isFolder { return .folder }
    return UTType(filenameExtension: (node.name as NSString).pathExtension) ?? .data
  }

  public var documentSize: NSNumber? { node.isFolder ? nil : NSNumber(value: node.size) }

  /// What Finder (and the Files app) let the user do, by `my_role`
  /// (docs/M7B_DESIGN.md §7): a viewer reads only, upload only reads and
  /// adds to a folder, an editor and above does everything.
  public var capabilities: NSFileProviderItemCapabilities {
    Self.capabilities(role: node.myRole, isFolder: node.isFolder)
  }

  /// The capabilities of a role on a file or folder: what the app offers
  /// on it (`ItemActions`). No role known leaves it all to the server.
  public static func capabilities(role: Role?, isFolder: Bool)
    -> NSFileProviderItemCapabilities
  {
    let actions = ItemActions(role: role, isFolder: isFolder)
    var capabilities: NSFileProviderItemCapabilities = []
    if actions.contains(.open) { capabilities.insert(.allowsReading) }
    if actions.contains(.rename) { capabilities.insert(.allowsRenaming) }
    if actions.contains(.move) { capabilities.insert(.allowsReparenting) }
    if actions.contains(.delete) { capabilities.insert(.allowsDeleting) }
    if isFolder {
      capabilities.insert(.allowsContentEnumerating)
      if actions.contains(.add) { capabilities.insert(.allowsAddingSubItems) }
    } else if role?.canChange ?? true {
      // New content is a change: an editor's, not upload only's.
      capabilities.insert(.allowsWriting)
    }
    return capabilities
  }

  /// The server's version: it moves on with the content and with a move,
  /// so it is both the content's and the metadata's.
  public var itemVersion: NSFileProviderItemVersion {
    let version = Self.versionData(node.version)
    return NSFileProviderItemVersion(contentVersion: version, metadataVersion: version)
  }

  public var contentModificationDate: Date? { Self.date(node.updatedAt) }
  public var creationDate: Date? { Self.date(node.createdAt) }

  /// The item of a node's id: the domain's folder is the root container.
  public static func identifier(_ id: String, rootID: String) -> NSFileProviderItemIdentifier {
    id == rootID ? .rootContainer : NSFileProviderItemIdentifier(id)
  }

  /// The node's id of an item.
  public static func serverID(_ identifier: NSFileProviderItemIdentifier, rootID: String) -> String
  {
    identifier == .rootContainer ? rootID : identifier.rawValue
  }

  public static func versionData(_ version: Int64) -> Data { Data(String(version).utf8) }

  /// The server's version an item's version data holds, if any.
  public static func version(of data: Data) -> Int64? {
    Int64(String(decoding: data, as: UTF8.self))
  }

  /// An RFC 3339 time of the server, with or without fractions.
  static func date(_ text: String) -> Date? {
    if let date = try? Date(text, strategy: .iso8601) { return date }
    let fractional = Date.ISO8601FormatStyle(includingFractionalSeconds: true)
    return try? Date(text, strategy: fractional)
  }
}
