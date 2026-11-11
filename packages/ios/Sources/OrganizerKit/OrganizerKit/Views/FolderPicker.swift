// iOS only: the macOS File Provider extension has no views.
#if os(iOS)
  import SwiftUI

  /// Picks a folder: the user's drives (My drive and the shared drives),
  /// then their folders in a navigation stack, and "Choose" for the one on
  /// show. Used by the share sheet, Move (from the top of the item's own
  /// drive: the server refuses a move between drives) and the drop box's
  /// "Elsewhere". Only a folder one may add to (upload only and up,
  /// `my_role`) can be chosen; a viewer's still opens, as a folder in it
  /// may give more. An encrypted folder shows a lock; inside one, the
  /// names are decrypted, and nothing is shown or chosen while the device
  /// is locked.
  public struct FolderPicker: View {
    let client: Client
    let title: String
    let encryption: Encryption?
    /// Only this drive's folders, from its top; nil: the drives first.
    let drive: DrivePlace?
    let onChoose: (FolderChoice) -> Void
    @Environment(\.dismiss) private var dismiss

    public init(
      client: Client, title: String = "Choose a folder", encryption: Encryption? = nil,
      drive: DrivePlace? = nil, onChoose: @escaping (FolderChoice) -> Void
    ) {
      self.client = client
      self.title = title
      self.encryption = encryption
      self.drive = drive
      self.onChoose = onChoose
    }

    public var body: some View {
      NavigationStack {
        Group {
          if let drive {
            FolderPickerLevel(
              model: FolderModel(drive.root, encryption: encryption), client: client,
              choose: choose)
          } else {
            DrivePickerLevel(client: client, title: title)
          }
        }
        .navigationDestination(for: FolderChoice.self) { folder in
          FolderPickerLevel(
            model: FolderModel(folder, encryption: encryption), client: client, choose: choose)
        }
        .toolbar {
          ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { dismiss() }
          }
        }
      }
    }

    private func choose(_ choice: FolderChoice) {
      onChoose(choice)
      dismiss()
    }
  }

  /// The picker's top: the drives whose folder is available.
  struct DrivePickerLevel: View {
    @State private var model = DrivesModel()
    let client: Client
    let title: String

    var body: some View {
      List {
        ForEach(model.available, id: \.id) { drive in
          NavigationLink(value: drive.root) {
            Label(drive.name, systemImage: "externaldrive.fill")
          }
        }
      }
      .overlay {
        if !model.loaded && model.loading { ProgressView() }
      }
      .navigationTitle(title)
      .navigationBarTitleDisplayMode(.inline)
      .task { if !model.loaded { await model.load(client) } }
      .refreshable { await model.load(client) }
      .alert(
        "Something went wrong",
        isPresented: .init(get: { model.error != nil }, set: { if !$0 { model.error = nil } })
      ) {
        Button("OK", role: .cancel) {}
      } message: {
        Text(model.error ?? "")
      }
    }
  }

  struct FolderPickerLevel: View {
    @State var model: FolderModel
    let client: Client
    let choose: (FolderChoice) -> Void

    var body: some View {
      List {
        if model.locked {
          Label("Encrypted: unlock it in Organizer first.", systemImage: "lock.fill")
            .foregroundStyle(.secondary)
        }
        if model.loaded && !model.locked && !model.actions.contains(.add) {
          Label("You can only view this folder.", systemImage: "eye")
            .foregroundStyle(.secondary)
        }
        ForEach(model.nodes.filter(\.isFolder)) { node in
          NavigationLink(value: model.choice(node)) {
            Label(node.name, systemImage: node.e2eRoot == nil ? "folder" : "lock.fill")
              .foregroundStyle(node.actions.contains(.add) ? .primary : .secondary)
          }
        }
        if model.loaded && !model.locked && !model.nodes.contains(where: \.isFolder) {
          Text("No folders in here").foregroundStyle(.secondary)
        }
        if model.hasMore {
          ProgressView().task { await model.loadMore(client) }
        }
      }
      .overlay {
        if !model.loaded && model.loading { ProgressView() }
      }
      .navigationTitle(model.title)
      .navigationBarTitleDisplayMode(.inline)
      .task { if !model.loaded { await model.load(client) } }
      .refreshable { await model.load(client) }
      .alert(
        "Something went wrong",
        isPresented: .init(get: { model.error != nil }, set: { if !$0 { model.error = nil } })
      ) {
        Button("OK", role: .cancel) {}
      } message: {
        Text(model.error ?? "")
      }
      .toolbar {
        ToolbarItem(placement: .bottomBar) {
          Button("Choose “\(model.title)”") {
            Task {
              if (try? await model.resolve(client)) != nil, let current = model.current,
                current.canAdd
              {
                choose(current)
              }
            }
          }
          .buttonStyle(.borderedProminent)
          .disabled(model.locked || !model.actions.contains(.add))
        }
      }
    }
  }
#endif
