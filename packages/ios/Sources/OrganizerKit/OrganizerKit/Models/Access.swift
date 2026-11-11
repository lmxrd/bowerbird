import Foundation

/// What the signed-in user may do with an item or a drive
/// (docs/M7B_DESIGN.md §1), from the least to the most.
public typealias Role = Components.Schemas.Role

extension Components.Schemas.Role: Comparable {
  /// The server's order: upload only < viewer < editor < manager < owner.
  var rank: Int {
    switch self {
    case .uploadOnly: 0
    case .viewer: 1
    case .editor: 2
    case .manager: 3
    case .owner: 4
    }
  }

  public static func < (lhs: Self, rhs: Self) -> Bool { lhs.rank < rhs.rank }

  /// Adds files and folders: every role but viewer (upload only may, it
  /// is what it is for), as the server's `Need::Create`.
  public var canAdd: Bool { self != .viewer }

  /// Renames, moves and deletes (to the trash): an editor or above, as the
  /// server's `Need::Change` and `Need::Delete`. Upload only does not
  /// change even what it added.
  public var canChange: Bool { self >= .editor }
}

/// What the app offers on a file or folder, by the user's role on it, so
/// that a viewer sees no edit actions and upload only sees only "add". The
/// server refuses the rest all the same (`Role::allows`, files/roles.rs).
public struct ItemActions: OptionSet, Sendable, Hashable {
  public let rawValue: Int

  public init(rawValue: Int) { self.rawValue = rawValue }

  /// Open and download it (all one is shown, one may read).
  public static let open = ItemActions(rawValue: 1 << 0)
  /// Upload files and make folders in it (a folder).
  public static let add = ItemActions(rawValue: 1 << 1)
  public static let rename = ItemActions(rawValue: 1 << 2)
  public static let move = ItemActions(rawValue: 1 << 3)
  /// To the trash.
  public static let delete = ItemActions(rawValue: 1 << 4)

  /// Rename, move and delete.
  public static let change: ItemActions = [.rename, .move, .delete]

  /// The actions of a role on a file or a folder. No role known (an
  /// answer without one: never to a signed-in user) leaves it all to the
  /// server, as before roles.
  public init(role: Role?, isFolder: Bool) {
    var actions: ItemActions = [.open]
    if isFolder && role?.canAdd ?? true { actions.insert(.add) }
    if role?.canChange ?? true { actions.formUnion(.change) }
    self = actions
  }
}

extension Components.Schemas.Node {
  /// What the app offers on it, by `my_role`.
  public var actions: ItemActions { ItemActions(role: myRole, isFolder: isFolder) }
}
