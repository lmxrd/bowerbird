import FileProvider
import Foundation
import OrganizerKit

/// The File Provider extension of the desktop app (docs/M5B_DESIGN.md §2):
/// one domain per on-demand folder (its identifier is the folder's id on
/// the server: a drive's top or a folder in a drive, docs/M7A_DESIGN.md),
/// shown in Finder under Locations. The working set follows only the
/// domain's drive; when its folder or drive is gone for the user, the
/// working set fails with `noSuchItem` and the local files stay until the
/// app removes the domain. The system calls it for a
/// listing, a file's content, or a change made in Finder; the logic is
/// OrganizerKit's `ProviderClient` (tested there), this class only hands
/// the system's calls to it.
///
/// The server and the token are in the Keychain, where the app's helper
/// (`organizer-fileprovider session`) put them: the shared access group of
/// the app's team.
final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
  let domain: NSFileProviderDomain
  let scope: ProviderScopeStore

  required init(domain: NSFileProviderDomain) {
    self.domain = domain
    let support =
      FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
      ?? FileManager.default.temporaryDirectory
    self.scope = ProviderScopeStore(
      url: support.appending(path: "Organizer/\(domain.identifier.rawValue).json"),
      rootID: domain.identifier.rawValue)
    super.init()
  }

  func invalidate() {}

  /// The server's client, as the app signed in; none: signed out.
  func client() throws -> ProviderClient {
    guard let session = DesktopSession.load() else {
      throw NSFileProviderError(.notAuthenticated)
    }
    return ProviderClient(
      server: session.server, token: session.token, rootID: domain.identifier.rawValue,
      deviceName: Host.current().localizedName ?? "Mac")
  }

  /// Runs `work` for one of the system's calls; its errors are the
  /// system's own.
  func run<T: Sendable>(
    _ work: @escaping @Sendable (ProviderClient) async throws -> T,
    then reply: @escaping @Sendable (Result<T, any Error>) -> Void
  ) -> Progress {
    let progress = Progress(totalUnitCount: 1)
    let client: ProviderClient
    do {
      client = try self.client()
    } catch {
      reply(.failure(error))
      return progress
    }
    let done = Unchecked(progress)
    let task = Task {
      do {
        let value = try await work(client)
        done.value.completedUnitCount = 1
        reply(.success(value))
      } catch {
        reply(.failure(providerError(error)))
      }
    }
    progress.cancellationHandler = { task.cancel() }
    return progress
  }

  func item(
    for identifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest,
    completionHandler: @escaping (NSFileProviderItem?, (any Error)?) -> Void
  ) -> Progress {
    let reply = Unchecked(completionHandler)
    return run {
      try await $0.item(identifier)
    } then: {
      switch $0 {
      case .success(let item): reply.value(item, nil)
      case .failure(let error): reply.value(nil, error)
      }
    }
  }

  func fetchContents(
    for itemIdentifier: NSFileProviderItemIdentifier,
    version requestedVersion: NSFileProviderItemVersion?, request: NSFileProviderRequest,
    completionHandler: @escaping (URL?, NSFileProviderItem?, (any Error)?) -> Void
  ) -> Progress {
    let reply = Unchecked(completionHandler)
    let directory =
      (try? NSFileProviderManager(for: domain)?.temporaryDirectoryURL())
      ?? FileManager.default.temporaryDirectory
    return run {
      try await $0.download(itemIdentifier, into: directory)
    } then: {
      switch $0 {
      case .success(let (url, item)): reply.value(url, item, nil)
      case .failure(let error): reply.value(nil, nil, error)
      }
    }
  }

  func createItem(
    basedOn itemTemplate: NSFileProviderItem, fields: NSFileProviderItemFields, contents url: URL?,
    options: NSFileProviderCreateItemOptions = [], request: NSFileProviderRequest,
    completionHandler:
      @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, (any Error)?)
      -> Void
  ) -> Progress {
    let reply = Unchecked(completionHandler)
    let name = itemTemplate.filename
    let parent = itemTemplate.parentItemIdentifier
    let isFolder = itemTemplate.contentType == .folder
    if itemTemplate.contentType == .symbolicLink || itemTemplate.contentType == .aliasFile {
      // The server holds files and folders only.
      completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
      return Progress()
    }
    return run {
      try await $0.create(name: name, in: parent, isFolder: isFolder, contents: url)
    } then: {
      switch $0 {
      case .success(let item): reply.value(item, [], false, nil)
      case .failure(let error): reply.value(nil, [], false, error)
      }
    }
  }

  func modifyItem(
    _ item: NSFileProviderItem, baseVersion version: NSFileProviderItemVersion,
    changedFields: NSFileProviderItemFields, contents newContents: URL?,
    options: NSFileProviderModifyItemOptions = [], request: NSFileProviderRequest,
    completionHandler:
      @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, (any Error)?)
      -> Void
  ) -> Progress {
    let reply = Unchecked(completionHandler)
    let identifier = item.itemIdentifier
    let base = ProviderItem.version(of: version.contentVersion) ?? 0
    let name = changedFields.contains(.filename) ? item.filename : nil
    let parent = changedFields.contains(.parentItemIdentifier) ? item.parentItemIdentifier : nil
    let contents = changedFields.contains(.contents) ? newContents : nil
    return run {
      try await $0.modify(
        identifier, baseVersion: base, name: name, parent: parent, contents: contents)
    } then: {
      switch $0 {
      // Dates, tags and the like are not kept on the server: taken as done.
      case .success(let result): reply.value(result.item, [], result.fetchContent, nil)
      case .failure(let error): reply.value(nil, [], false, error)
      }
    }
  }

  func deleteItem(
    identifier: NSFileProviderItemIdentifier, baseVersion version: NSFileProviderItemVersion,
    options: NSFileProviderDeleteItemOptions = [], request: NSFileProviderRequest,
    completionHandler: @escaping ((any Error)?) -> Void
  ) -> Progress {
    let reply = Unchecked(completionHandler)
    let base = ProviderItem.version(of: version.contentVersion) ?? 0
    return run {
      try await $0.delete(identifier, baseVersion: base)
    } then: {
      switch $0 {
      case .success: reply.value(nil)
      case .failure(let error): reply.value(error)
      }
    }
  }

  func enumerator(
    for containerItemIdentifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest
  ) throws -> NSFileProviderEnumerator {
    let client = try client()
    switch containerItemIdentifier {
    case .workingSet:
      return WorkingSetEnumerator(client: client, scope: scope)
    case .trashContainer:
      // The server's trash is the web app's.
      return EmptyEnumerator()
    default:
      return FolderEnumerator(folder: containerItemIdentifier, client: client, scope: scope)
    }
  }
}

/// What the system hands over that is not marked Sendable (a completion
/// handler, an observer, the call's progress): used from the one task
/// that answers the call.
struct Unchecked<T>: @unchecked Sendable {
  let value: T
  init(_ value: T) { self.value = value }
}
