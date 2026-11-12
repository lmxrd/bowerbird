// iOS only: the macOS File Provider extension has no views.
#if os(iOS)
  import SwiftUI

  /// The share sheet's form: "Drop box" (an optional note and the options
  /// the user's settings allow) or "Place manually" (a folder), then Send.
  /// The share extension shows it, and the app for files added to the drop
  /// box.
  public struct ShareForm: View {
    @Bindable var model: ShareModel
    let client: Client
    let queue: UploadQueue
    let onDone: (Bool) -> Void
    @State private var pickingFolder = false

    public init(
      model: ShareModel, client: Client, queue: UploadQueue, onDone: @escaping (Bool) -> Void
    ) {
      self.model = model
      self.client = client
      self.queue = queue
      self.onDone = onDone
    }

    public var body: some View {
      NavigationStack {
        Form {
          Section {
            Picker("Send to", selection: $model.mode) {
              Text("Drop box").tag(ShareModel.Mode.dropBox)
              Text("Place manually").tag(ShareModel.Mode.place)
            }
            .pickerStyle(.segmented)
            .disabled(!model.canPlace)
            .listRowBackground(Color.clear)
            .listRowInsets(EdgeInsets())
          } footer: {
            if !model.canPlace {
              Text("A link goes to the drop box.")
            }
          }

          Section("Sharing") {
            if let link = model.link {
              Label(link, systemImage: "link").lineLimit(2)
            }
            ForEach(model.files) { file in
              LabeledContent {
                Text(ByteCountFormatter.string(fromByteCount: file.size, countStyle: .file))
              } label: {
                Label(file.name, systemImage: "doc").lineLimit(1)
              }
            }
          }

          switch model.mode {
          case .dropBox: dropBoxSection
          case .place: placeSection
          }

          if let error = model.error {
            Section { Text(error).foregroundStyle(.red) }
          }
        }
        .navigationTitle(model.summary)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
          ToolbarItem(placement: .cancellationAction) {
            Button("Cancel") { onDone(false) }
          }
          ToolbarItem(placement: .confirmationAction) {
            if model.sending {
              ProgressView()
            } else {
              Button("Send") {
                Task { if await model.send(queue, client: client) { onDone(true) } }
              }
              .disabled(!model.canSend)
            }
          }
        }
        .task { await model.load(client) }
        .sheet(isPresented: $pickingFolder) {
          FolderPicker(client: client, encryption: model.encryption) { model.folder = $0 }
        }
      }
    }

    @ViewBuilder private var dropBoxSection: some View {
      Section {
        TextField("Note (optional)", text: $model.note, axis: .vertical)
          .lineLimit(2...5)
      } footer: {
        Text("Tell the drop box what this is or where it belongs.")
      }
      if let options = model.options {
        Section {
          if options.offerReadContents {
            Toggle("Read the contents", isOn: $model.readContents)
          }
          if model.link != nil {
            Picker("Save the link as", selection: $model.linkMode) {
              Text("Bookmark").tag(Components.Schemas.Links.bookmark)
              Text("Snapshot of the page").tag(Components.Schemas.Links.snapshot)
              Text("Download").tag(Components.Schemas.Links.download)
            }
          }
        } footer: {
          if options.offerReadContents {
            Text("Off: only the name and your note are used to place it.")
          }
        }
      }
    }

    @ViewBuilder private var placeSection: some View {
      Section {
        Button {
          pickingFolder = true
        } label: {
          LabeledContent("Folder") {
            if model.folder?.e2eRoot != nil {
              Label(model.folder?.name ?? "", systemImage: "lock.fill")
            } else {
              Text(model.folder?.name ?? "Choose…")
            }
          }
        }
        .foregroundStyle(.primary)
      } header: {
        Text("Folder")
      } footer: {
        if model.folderLocked {
          Text("This folder is encrypted: unlock it in Organizer first, then share again.")
        } else if model.folder?.e2eRoot != nil {
          Text("Encrypted on this iPhone before it is sent.")
        }
      }
    }
  }
#endif
