import FileProvider
import Foundation
import OrganizerKit

/// A folder's listing, a page at a time (the page is the server's
/// `after`). Its changes come through the working set, as the replicated
/// model has it: a folder's own enumerator tells of none.
final class FolderEnumerator: NSObject, NSFileProviderEnumerator {
  let folder: NSFileProviderItemIdentifier
  let client: ProviderClient
  let scope: ProviderScopeStore

  init(folder: NSFileProviderItemIdentifier, client: ProviderClient, scope: ProviderScopeStore) {
    self.folder = folder
    self.client = client
    self.scope = scope
  }

  func invalidate() {}

  func enumerateItems(
    for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage
  ) {
    let observer = Unchecked(observer)
    let after = Page.after(page)
    let (folder, client, scope) = (folder, client, scope)
    Task {
      do {
        let (nodes, next) = try await client.list(folder, after: after)
        await scope.listed(nodes, in: ProviderItem.serverID(folder, rootID: client.rootID))
        observer.value.didEnumerate(nodes.map { ProviderItem($0, rootID: client.rootID) })
        observer.value.finishEnumerating(upTo: next.map(Page.of))
      } catch {
        observer.value.finishEnumeratingWithError(providerError(error))
      }
    }
  }

  func enumerateChanges(
    for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor
  ) {
    observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
  }

  func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
    let reply = Unchecked(completionHandler)
    let client = client
    Task { reply.value((try? await client.cursor()).map(Anchor.of)) }
  }
}

/// The working set: what changed in the domain, from the server's change
/// feed, its cursor the sync anchor. The app signals it when the live
/// events say something changed (`organizer-fileprovider signal`); a
/// cursor too old (410) makes the system list everything again.
final class WorkingSetEnumerator: NSObject, NSFileProviderEnumerator {
  let client: ProviderClient
  let scope: ProviderScopeStore

  init(client: ProviderClient, scope: ProviderScopeStore) {
    self.client = client
    self.scope = scope
  }

  func invalidate() {}

  /// Nothing to list up front: what is materialized is known to the
  /// system, and the changes say the rest.
  func enumerateItems(
    for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage
  ) {
    observer.finishEnumerating(upTo: nil)
  }

  func enumerateChanges(
    for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor
  ) {
    let observer = Unchecked(observer)
    let (client, scope) = (client, scope)
    Task {
      do {
        let page = try await client.changes(since: Anchor.cursor(anchor), scope: scope)
        if !page.updated.isEmpty { observer.value.didUpdate(page.updated) }
        if !page.deleted.isEmpty { observer.value.didDeleteItems(withIdentifiers: page.deleted) }
        observer.value.finishEnumeratingChanges(
          upTo: Anchor.of(page.cursor), moreComing: page.hasMore)
      } catch {
        observer.value.finishEnumeratingWithError(providerError(error))
      }
    }
  }

  func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
    let reply = Unchecked(completionHandler)
    let client = client
    Task { reply.value((try? await client.cursor()).map(Anchor.of)) }
  }
}

/// The trash: the server's is the web app's.
final class EmptyEnumerator: NSObject, NSFileProviderEnumerator {
  func invalidate() {}

  func enumerateItems(
    for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage
  ) {
    observer.finishEnumerating(upTo: nil)
  }
}

/// A page of a listing: the server's `after`, or the first page.
enum Page {
  static let prefix = "after:"

  static func of(_ after: String) -> NSFileProviderPage {
    NSFileProviderPage(Data((prefix + after).utf8))
  }

  /// None for the first page (the system's own, sorted by name or date).
  static func after(_ page: NSFileProviderPage) -> String? {
    let text = String(decoding: page.rawValue, as: UTF8.self)
    return text.hasPrefix(prefix) ? String(text.dropFirst(prefix.count)) : nil
  }
}

/// The working set's anchor: the change feed's cursor.
enum Anchor {
  static func of(_ cursor: String) -> NSFileProviderSyncAnchor {
    NSFileProviderSyncAnchor(Data(cursor.utf8))
  }

  static func cursor(_ anchor: NSFileProviderSyncAnchor) -> String {
    String(decoding: anchor.rawValue, as: UTF8.self)
  }
}
