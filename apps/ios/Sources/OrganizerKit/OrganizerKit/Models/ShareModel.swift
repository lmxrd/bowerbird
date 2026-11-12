import Foundation
import Observation

/// A file the share extension (or the app) received, copied into the app
/// group's container already.
public struct SharedFile: Sendable, Equatable, Identifiable {
  public var url: URL
  public var name: String
  public var size: Int64
  public var id: URL { url }

  public init(url: URL, name: String, size: Int64) {
    self.url = url
    self.name = name
    self.size = size
  }
}

/// The share sheet (and the app's own "add to the drop box"): files or a
/// link, to the drop box (an optional note and the options the settings
/// allow) or into a folder.
@MainActor
@Observable
public final class ShareModel: Identifiable {
  public enum Mode: String, Sendable, CaseIterable {
    case dropBox
    case place
  }

  public var mode: Mode = .dropBox
  public var note = ""
  public var readContents = false
  public var linkMode: Components.Schemas.Links = .bookmark
  public var folder: FolderChoice?
  public private(set) var options: DropOptions?
  public private(set) var files: [SharedFile]
  public private(set) var link: String?
  public private(set) var sending = false
  public var error: String?
  /// This device's encryption, for a folder that is encrypted.
  public let encryption: Encryption?

  public init(files: [SharedFile], link: String?, encryption: Encryption? = nil) {
    self.files = files
    self.link = link
    self.encryption = encryption
  }

  /// A link can only go to the drop box.
  public var canPlace: Bool { link == nil && !files.isEmpty }

  /// The chosen folder is encrypted and this device has not unlocked:
  /// nothing can go there until it does, in the app.
  public var folderLocked: Bool {
    mode == .place && folder?.e2eRoot != nil && encryption?.unlocked != true
  }

  public var canSend: Bool {
    guard !sending, !files.isEmpty || link != nil else { return false }
    // The picker offers only folders one may add to: a viewer's never.
    return mode == .dropBox || (folder?.canAdd == true && !folderLocked)
  }

  /// What is shared, as the sheet's title says it.
  public var summary: String {
    if let link { return URL(string: link)?.host() ?? link }
    return files.count == 1 ? files[0].name : "\(files.count) files"
  }

  public func load(_ client: Client) async {
    guard options == nil else { return }
    do {
      let settings = try await client.getDropBoxSettings().ok.body.json.settings
      apply(DropOptions(settings))
    } catch {
      self.error = describe(error)
    }
  }

  public func apply(_ options: DropOptions) {
    self.options = options
    readContents = options.readContentsByDefault
    linkMode = options.linkMode
  }

  /// Hands everything to the upload queue; true when it took it. Into an
  /// encrypted folder, the files are encrypted first (`client` lists the
  /// names there and gives the keys).
  public func send(_ queue: UploadQueue, client: Client? = nil) async -> Bool {
    sending = true
    defer { sending = false }
    let note = self.note.trimmingCharacters(in: .whitespacesAndNewlines)
    let readContents = options?.readContents(self.readContents)
    do {
      switch mode {
      case .dropBox:
        if let link {
          try queue.enqueueLink(
            link, note: note.isEmpty ? nil : note, readContents: readContents,
            mode: linkMode == options?.linkMode ? nil : linkMode)
        } else {
          try await queue.enqueueDropBox(
            files: files.map { ($0.url, $0.name) }, note: note.isEmpty ? nil : note,
            readContents: readContents, folders: nil, move: true)
        }
      case .place:
        guard let folder else { return false }
        if let root = folder.e2eRoot {
          guard let encryption, encryption.unlocked, let client else {
            throw EncryptionError.locked
          }
          let sealing = try await encryption.sealingID(
            of: folder.id, source: folder.sourceID,
            elsewhere: folder.sourceID != nil || folder.drive?.isRemote == true, client)
          try await encryption.enqueue(
            files, into: folder.id, sealedFor: sealing, root: root, queue: queue, client)
          break
        }
        for file in files {
          try await queue.enqueueFile(
            at: file.url, name: file.name, folderID: folder.id, move: true)
        }
      }
      error = nil
      return true
    } catch {
      self.error = describe(error)
      return false
    }
  }
}
