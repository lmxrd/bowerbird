import OrganizerKit
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Screen 4: the share extension. What another app shares (files, photos,
/// a link) is copied into the app group's container, then handed to a
/// background URLSession of its own; the app carries the uploads on when
/// the system wakes it. Nothing large is read into the extension's memory.
final class ShareViewController: UIViewController {
  override func viewDidLoad() {
    super.viewDidLoad()
    let host = UIHostingController(
      rootView: ShareRoot(
        providers: providers,
        finish: { [weak self] sent in
          if sent {
            self?.extensionContext?.completeRequest(returningItems: nil)
          } else {
            self?.extensionContext?.cancelRequest(
              withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
          }
        }))
    addChild(host)
    host.view.frame = view.bounds
    host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    view.addSubview(host.view)
    host.didMove(toParent: self)
  }

  private var providers: [NSItemProvider] {
    (extensionContext?.inputItems as? [NSExtensionItem] ?? []).flatMap { $0.attachments ?? [] }
  }
}

/// What was shared, once copied.
struct Received: Sendable {
  var files: [SharedFile] = []
  var link: String?
}

@MainActor
enum Receiving {
  /// Copies the shared files into a staging folder; a web link is kept as
  /// a link.
  static func receive(_ providers: [NSItemProvider], into folder: URL) async throws -> Received {
    var received = Received()
    for provider in providers {
      if !provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
        provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
        let url = try await loadURL(provider), url.scheme == "http" || url.scheme == "https"
      {
        if received.link == nil { received.link = url.absoluteString }
        continue
      }
      if let file = try await loadFile(provider, into: folder) {
        received.files.append(file)
      }
    }
    // A link with files: the files go, the link is left out.
    if !received.files.isEmpty { received.link = nil }
    return received
  }

  private static func loadURL(_ provider: NSItemProvider) async throws -> URL? {
    try await withCheckedThrowingContinuation { continuation in
      _ = provider.loadObject(ofClass: URL.self) { url, error in
        if let error {
          continuation.resume(throwing: error)
        } else {
          continuation.resume(returning: url)
        }
      }
    }
  }

  /// The provider's file, copied while the system lends it.
  private static func loadFile(_ provider: NSItemProvider, into folder: URL) async throws
    -> SharedFile?
  {
    let types = provider.registeredContentTypes
    guard
      let type = types.first(where: { $0.conforms(to: .data) || $0.conforms(to: .package) })
        ?? types.first
    else { return nil }
    let suggested = provider.suggestedName
    return try await withCheckedThrowingContinuation { continuation in
      _ = provider.loadFileRepresentation(for: type, openInPlace: false) { url, _, error in
        guard let url else {
          continuation.resume(throwing: error ?? CocoaError(.fileReadUnknown))
          return
        }
        var name = suggested ?? url.lastPathComponent
        if (name as NSString).pathExtension.isEmpty, let ext = type.preferredFilenameExtension {
          name += ".\(ext)"
        }
        do {
          continuation.resume(returning: try Staging.copy(url, named: name, into: folder))
        } catch {
          continuation.resume(throwing: error)
        }
      }
    }
  }
}

struct ShareRoot: View {
  let providers: [NSItemProvider]
  let finish: (Bool) -> Void
  @State private var session = SessionStore.load()
  @State private var model: ShareModel?
  @State private var error: String?
  @State private var folder: URL?
  /// This share's background session: its own identifier, listed in the
  /// container for the app to find.
  @State private var queue = UploadQueue(
    identifier: Organizer.extensionSessionPrefix + UUID().uuidString)

  var body: some View {
    Group {
      if let session {
        if let model {
          ShareForm(
            model: model, client: Client.organizer(server: session.server, token: session.token),
            queue: queue
          ) { sent in
            if let folder { try? FileManager.default.removeItem(at: folder) }
            finish(sent)
          }
        } else if let error {
          message(error)
        } else {
          ProgressView("Preparing…").task { await prepare() }
        }
      } else {
        message("Sign in to Organizer in the app first, then share again.")
      }
    }
  }

  private func message(_ text: String) -> some View {
    NavigationStack {
      ContentUnavailableView(
        "Organizer", systemImage: "tray.and.arrow.down", description: Text(text)
      )
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("Close") { finish(false) }
        }
      }
    }
  }

  private func prepare() async {
    do {
      queue.markAsExtensionSession()
      let folder = try Staging.makeFolder()
      self.folder = folder
      let received = try await Receiving.receive(providers, into: folder)
      guard !received.files.isEmpty || received.link != nil else {
        error = "Organizer cannot take what was shared."
        return
      }
      // The private key the app unlocked, from the shared Keychain: an
      // encrypted folder takes files only once it has.
      model = ShareModel(files: received.files, link: received.link, encryption: Encryption())
    } catch {
      self.error = error.localizedDescription
    }
  }
}
