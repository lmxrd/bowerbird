import CoreTransferable
import OrganizerKit
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// A photo or video from the Photos picker, as a file copied into the
/// staging folder (never into memory).
struct PickedFile: Transferable {
  let file: SharedFile

  static var transferRepresentation: some TransferRepresentation {
    FileRepresentation(importedContentType: .item) { received in
      let folder = try Staging.makeFolder()
      return PickedFile(
        file: try Staging.copy(received.file, named: received.file.lastPathComponent, into: folder))
    }
  }
}

enum Picking {
  /// The picked photos, copied.
  static func photos(_ items: [PhotosPickerItem]) async throws -> [SharedFile] {
    var files: [SharedFile] = []
    for item in items {
      if let picked = try await item.loadTransferable(type: PickedFile.self) {
        files.append(picked.file)
      }
    }
    return files
  }

  /// Files from the document picker, copied while access is granted.
  static func files(_ urls: [URL]) throws -> [SharedFile] {
    let folder = try Staging.makeFolder()
    return try urls.map { url in
      let scoped = url.startAccessingSecurityScopedResource()
      defer { if scoped { url.stopAccessingSecurityScopedResource() } }
      return try Staging.copy(url, named: url.lastPathComponent, into: folder)
    }
  }
}

/// The "+" menu's two sources: Photos and Files.
struct AddMenuSources: ViewModifier {
  @Binding var choosingPhotos: Bool
  @Binding var choosingFiles: Bool
  let picked: ([SharedFile]) -> Void
  let failed: (String) -> Void
  @State private var photoItems: [PhotosPickerItem] = []

  func body(content: Content) -> some View {
    content
      .photosPicker(isPresented: $choosingPhotos, selection: $photoItems, matching: nil)
      .onChange(of: photoItems) { _, items in
        guard !items.isEmpty else { return }
        photoItems = []
        Task {
          do {
            picked(try await Picking.photos(items))
          } catch {
            failed(error.localizedDescription)
          }
        }
      }
      .fileImporter(
        isPresented: $choosingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true
      ) {
        result in
        do {
          picked(try Picking.files(try result.get()))
        } catch {
          failed(error.localizedDescription)
        }
      }
  }
}
