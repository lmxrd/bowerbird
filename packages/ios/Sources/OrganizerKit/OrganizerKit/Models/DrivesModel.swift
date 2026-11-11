import Foundation
import Observation

/// The drive a folder is in, as the Files tab and the pickers need it:
/// where its tree starts (for Move, which stays in it: the server refuses
/// a move between drives) and the name its top shows. For something
/// shared with the user, its own folder is the top.
public struct DrivePlace: Sendable, Equatable, Hashable {
  public var id: String
  public var name: String
  public var rootID: String
  /// The user's role on its top: the drive's, or a shared folder's.
  public var role: Role?
  /// Being moved to other storage (docs/M7C_DESIGN.md §2), its name:
  /// read-only until done, so nothing in it offers an edit.
  public var movingTo: String?
  /// On someone's own storage: the warning its folders show (it goes if
  /// they remove it).
  public var warning: String?
  /// A mirror (docs/M7D_DESIGN.md): a read-only copy of a drive, always.
  public var isMirror: Bool
  /// From another server (docs/M7E_DESIGN.md §5): browsed by its role like
  /// any, its bytes through the user's own server, but never where a
  /// picker puts something new (the share sheet, the drop box).
  public var isRemote: Bool

  public init(
    id: String, name: String, rootID: String, role: Role? = nil, movingTo: String? = nil,
    warning: String? = nil, isMirror: Bool = false, isRemote: Bool = false
  ) {
    self.id = id
    self.name = name
    self.rootID = rootID
    self.role = role
    self.movingTo = movingTo
    self.warning = warning
    self.isMirror = isMirror
    self.isRemote = isRemote
  }

  /// A drive whose folder is available: its root known, its storage
  /// usable now (an unavailable one offers nothing), and accepted when it
  /// is from another server. A mirror is titled as one ("Team · mirror").
  public init?(_ drive: Drive) {
    guard let root = drive.rootId, drive.available, drive.remote?.pending != true else {
      return nil
    }
    self.init(
      id: drive.id, name: drive.title, rootID: root, role: drive.myRole,
      movingTo: drive.movingTo?.name, warning: drive.ownerWarning, isMirror: drive.isMirror,
      isRemote: drive.isRemote)
  }

  /// Being moved, or a mirror: nothing in it offers more than open, and
  /// the pickers leave it out.
  public var readOnly: Bool { movingTo != nil || isMirror }

  /// The top of a drive, as a folder to open.
  public var root: FolderChoice {
    FolderChoice(id: rootID, name: name, drive: self, role: role)
  }
}

/// Where a drive's storage stands (docs/M7C_DESIGN.md §2, §4).
public enum DriveStorage: Sendable, Equatable {
  /// Usable as usual.
  case ready
  /// Cannot be used now: its backend fails (`reason` says how), or its
  /// owner's private credentials are locked until they sign in (`locked`).
  case unavailable(reason: String, locked: Bool)
  /// Being moved to other storage (`to`, its name): read-only meanwhile.
  case moving(to: String)
}

extension Components.Schemas.Drive {
  /// Its storage now: an unavailable one opens not and offers nothing, a
  /// moving one is read-only.
  public var storage: DriveStorage {
    if !available {
      let reason = unavailableReason ?? "Its storage cannot be reached"
      return .unavailable(reason: reason, locked: unavailable == .locked)
    }
    if let to = movingTo { return .moving(to: to.name) }
    return .ready
  }

  /// The line the drives list shows for its storage, when it is not ready:
  /// the server's reason, or where it is moving.
  public var storageLine: String? {
    switch storage {
    case .ready: nil
    case .unavailable(let reason, _): reason
    case .moving(let to): "Moving to \(to)…"
    }
  }

  /// The warning its members see, when it is on someone's own storage
  /// (docs/M7C_DESIGN.md §2): it goes if they remove it, and the server
  /// does not back it up.
  public var ownerWarning: String? {
    // A mirror is always on its user's own storage, a drive from another
    // server on that server's: nothing to warn of.
    guard !isMirror, !isRemote, let owner = backend.owner else { return nil }
    return "Stored on \(owner)'s own storage. If they remove it, this drive goes. "
      + "Not backed up by the server."
  }
}

extension Components.Schemas.MirrorStopped {
  /// Why a mirror stopped copying, in words (the server's
  /// `Stopped::reason`).
  public var reason: String {
    switch self {
    case .driveDeleted: "The drive was deleted"
    case .accessLost: "You no longer have this drive (or may only upload to it)"
    case .forbidden: "The drive's managers do not allow mirroring it"
    case .sharedDrivesOff: "Mirroring shared drives is not allowed here"
    }
  }
}

extension Components.Schemas.Drive: Identifiable {
  public var isShared: Bool { kind == .shared }

  /// A mirror (docs/M7D_DESIGN.md): a read-only copy of a drive in the
  /// user's own storage, listed in a group of its own.
  public var isMirror: Bool { kind == .mirror }

  /// Shared from another server (docs/M7E_DESIGN.md §6): listed in a
  /// group of its own, with whose it is and where (`remote`).
  public var isRemote: Bool { kind == .remote }

  /// Its name as listed: a mirror's says it is one ("Team · mirror").
  public var title: String { isMirror ? "\(name) · mirror" : name }

  /// Where a mirror stands (docs/M7D_DESIGN.md §9): up to date, copying,
  /// waiting for its storage's unlock, failing, or stopped and why; nil for
  /// another drive. By the status's code, never its words.
  public var mirrorLine: String? {
    guard isMirror, let mirror = mirrorOf else { return nil }
    switch mirror.status {
    case .upToDate: return "Up to date"
    case .pending: return "Changes wait for the next copy"
    case .copying: return "Copying…"
    case .waitingForUnlock: return "Waiting for your storage to be unlocked"
    case .failing: return "Copying fails; it is tried again in a few minutes"
    case .stopped: return "Stopped: \(mirror.stopped?.reason ?? "it no longer copies")"
    }
  }

  /// What the Files tab says under a shared drive's name: "3 members ·
  /// 1.2 GB". My drive says nothing.
  public var summary: String? {
    guard isShared else { return nil }
    let people = members == 1 ? "1 member" : "\(members) members"
    return "\(people) · \(ByteCountFormatter.string(fromByteCount: used, countStyle: .file))"
  }
}

extension Components.Schemas.RemoteDrive {
  /// Waiting for the user to accept it: nothing in it is listed until
  /// then (`POST /shared/{root_id}/accept`; declined by `DELETE`).
  public var pending: Bool { state == .pending }

  /// Whose it is and where: "From alice@cloud.example", its server named
  /// apart when the owner's address does not say it.
  public var fromLine: String {
    owner.hasSuffix("@\(server)") ? "From \(owner)" : "From \(owner) on \(server)"
  }

  /// How it stands, when it is not simply there: waiting for Accept, or
  /// its server failing (and why); nil otherwise.
  public var stateLine: String? {
    if pending { return "Waiting for you to accept it" }
    if let error { return "Its server cannot be reached now: \(error)" }
    return nil
  }
}

extension Components.Schemas.SharedItem {
  /// Shared from another server and not accepted yet: Accept or Decline,
  /// and it does not open until accepted.
  public var awaitsAnswer: Bool { remote?.pending == true }

  /// Who shared it, as Shared with me says: their address on another
  /// server ("From alice@cloud.example"), else the user here.
  public var fromLine: String { remote?.fromLine ?? "From \(sharedBy.email)" }
}

extension Client {
  /// Accepts (`POST /shared/{id}/accept`) or declines (`DELETE
  /// /shared/{id}`) something shared from another server: a shared item's
  /// node, or a drive's root.
  public func answerShared(_ id: String, accept: Bool) async throws {
    if accept {
      _ = try await acceptShared(path: .init(id: id)).noContent
    } else {
      _ = try await declineShared(path: .init(id: id)).noContent
    }
  }
}

/// The top of the Files tab and of the folder pickers: the user's drives,
/// My drive first, then the shared drives by name, then the mirrors by
/// name, then the drives from other servers, as the server lists them.
@MainActor
@Observable
public final class DrivesModel {
  public private(set) var drives: [Drive] = []
  public private(set) var loading = false
  public private(set) var loaded = false
  public var error: String?

  public init() {}

  /// My drive and the shared drives: the Files tab's first group.
  public var ownAndShared: [Drive] { drives.filter { !$0.isMirror && !$0.isRemote } }

  /// The mirrors (docs/M7D_DESIGN.md §5): a group of their own, after the
  /// shared drives, read-only.
  public var mirrors: [Drive] { drives.filter(\.isMirror) }

  /// The drives from other servers (docs/M7E_DESIGN.md §6): a group of
  /// their own, after the mirrors.
  public var remotes: [Drive] { drives.filter(\.isRemote) }

  /// The drives whose folder is available: those a folder can be picked
  /// in (the folders one may add to may be anywhere in them, a viewer's
  /// drive too: a folder in it may give more). Not one whose storage is
  /// unavailable, nor one being moved, nor a mirror (read-only), nor one
  /// from another server (nothing new is put there from here).
  public var available: [DrivePlace] {
    drives.compactMap(DrivePlace.init).filter { !$0.readOnly && !$0.isRemote }
  }

  /// Accepts or declines a drive from another server (by its root), then
  /// lists the drives again.
  public func answer(_ drive: Drive, accept: Bool, client: Client) async {
    guard let root = drive.rootId else { return }
    do {
      try await client.answerShared(root, accept: accept)
    } catch {
      self.error = describe(error)
    }
    await load(client)
  }

  public func load(_ client: Client) async {
    loading = true
    defer { loading = false }
    do {
      drives = try await client.listDrives().ok.body.json
      loaded = true
      error = nil
    } catch {
      self.error = describe(error)
    }
  }
}

/// "Shared with me": what others shared with the user (M6b), the newest
/// first. A folder opens as the top of its own tree.
@MainActor
@Observable
public final class SharedModel {
  public private(set) var items: [SharedItem] = []
  public private(set) var loading = false
  public private(set) var loaded = false
  public var error: String?

  public init() {}

  public func load(_ client: Client) async {
    loading = true
    defer { loading = false }
    do {
      items = try await client.listSharedWithMe().ok.body.json
      loaded = true
      error = nil
    } catch {
      self.error = describe(error)
    }
  }

  /// Accepts or declines something shared from another server, then
  /// lists what is shared again.
  public func answer(_ item: SharedItem, accept: Bool, client: Client) async {
    do {
      try await client.answerShared(item.node.id, accept: accept)
    } catch {
      self.error = describe(error)
    }
    await load(client)
  }

  /// A shared folder opened: its tree starts at it.
  public static func place(_ node: FileNode) -> DrivePlace {
    DrivePlace(
      id: node.driveId, name: node.name, rootID: node.id, role: node.myRole,
      isRemote: node.remote != nil)
  }
}
