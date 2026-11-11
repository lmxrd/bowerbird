import OrganizerKit
import SwiftUI

/// Screen 3: the drop box. What waits for review (with the proposal:
/// folder, reason, confidence), what is being analysed, what needs a
/// place, and what was placed (with Undo); add files, photos or a link.
struct DropBoxView: View {
  @Environment(AppModel.self) private var app
  @State private var model = DropBoxModel()
  @State private var elsewhere: DropItem?
  @State private var choosingPhotos = false
  @State private var choosingFiles = false
  @State private var addingLink = false
  @State private var link = ""
  @State private var sharing: ShareModel?

  var body: some View {
    NavigationStack {
      List {
        if !model.toReview.isEmpty {
          Section("To review") {
            ForEach(model.toReview) { item in
              ReviewRow(
                item: item, busy: model.busy.contains(item.id),
                confirm: { act { await model.confirm(item, $0) } },
                elsewhere: { elsewhere = item },
                reject: { act { await model.reject(item, $0) } })
            }
          }
        }
        let dropping = app.jobs.filter { $0.folderID == nil }
        if !model.analysing.isEmpty || !dropping.isEmpty {
          Section("Analysing") {
            ForEach(dropping) { job in
              UploadRow(job: job, sent: app.sent[job.id])
            }
            ForEach(model.analysing) { item in
              HStack {
                ItemTitle(item: item)
                Spacer()
                ProgressView()
              }
            }
          }
        }
        if !model.needsPlace.isEmpty {
          Section("Needs a place") {
            ForEach(model.needsPlace) { item in
              VStack(alignment: .leading, spacing: 8) {
                ItemTitle(item: item)
                HStack {
                  Button("Place…") { elsewhere = item }
                  if item.item.status == .failed {
                    Button("Analyse again") { act { await model.retry(item, $0) } }
                  }
                }
                .buttonStyle(.bordered)
                .disabled(model.busy.contains(item.id))
              }
            }
          }
        }
        if !model.placed.isEmpty {
          Section("Placed") {
            ForEach(model.placed) { item in
              HStack {
                VStack(alignment: .leading) {
                  Text(item.item.name).lineLimit(1)
                  if let path = item.placement?.path {
                    Label(path, systemImage: "folder").font(.caption).foregroundStyle(.secondary)
                  }
                }
                Spacer()
                if item.placement != nil {
                  Button("Undo") { act { await model.undo(item, $0) } }
                    .buttonStyle(.bordered)
                    .disabled(model.busy.contains(item.id))
                }
              }
            }
          }
        }
      }
      .overlay {
        if model.loaded && model.items.isEmpty && app.jobs.allSatisfy({ $0.folderID != nil }) {
          ContentUnavailableView(
            "The drop box is empty", systemImage: "tray",
            description: Text(
              "Share files, photos or links to Organizer from any app, or add them with +."))
        } else if !model.loaded && model.loading {
          ProgressView()
        }
      }
      .navigationTitle("Drop box")
      .toolbar {
        ToolbarItem(placement: .primaryAction) {
          Menu {
            Button("Add Photos", systemImage: "photo.on.rectangle") { choosingPhotos = true }
            Button("Add Files", systemImage: "doc") { choosingFiles = true }
            Button("Add Link", systemImage: "link") {
              link = ""
              addingLink = true
            }
          } label: {
            Label("Add", systemImage: "plus")
          }
        }
      }
      .modifier(
        AddMenuSources(
          choosingPhotos: $choosingPhotos, choosingFiles: $choosingFiles,
          picked: { files in
            guard !files.isEmpty else { return }
            let share = ShareModel(files: files, link: nil)
            if let options = model.options { share.apply(options) }
            sharing = share
          }, failed: { model.error = $0 })
      )
      .sheet(item: $sharing) { share in
        if let client = app.client {
          ShareForm(model: share, client: client, queue: app.uploads) { _ in
            sharing = nil
            app.refreshJobs()
          }
        }
      }
      .sheet(item: $elsewhere) { item in
        if let client = app.client {
          FolderPicker(client: client, title: "Place “\(item.item.name)”") { folder in
            act { await model.placeElsewhere(item, in: folder, $0) }
          }
        }
      }
      .alert("Add a link", isPresented: $addingLink) {
        TextField("https://", text: $link)
          .keyboardType(.URL)
          .textInputAutocapitalization(.never)
        Button("Cancel", role: .cancel) {}
        Button("Add") { act { await model.addLink(link, note: nil, $0) } }
      }
      .task { await load() }
      .refreshable { await load() }
      .onChange(of: app.dropBoxChanges) { Task { await load() } }
      .errorAlert($model.error)
    }
  }

  private func load() async {
    guard let client = app.client else { return }
    await model.load(client)
    await app.refreshBadge()
  }

  private func act(_ action: @escaping (Client) async -> Void) {
    guard let client = app.client else { return }
    Task {
      await action(client)
      await app.refreshBadge()
    }
  }
}

struct ItemTitle: View {
  let item: DropItem

  var body: some View {
    VStack(alignment: .leading, spacing: 2) {
      Label(item.item.name, systemImage: item.item.kind == .link ? "link" : "doc")
        .lineLimit(1)
      Text(item.stateText).font(.caption).foregroundStyle(.secondary)
    }
  }
}

/// An item to review: its name, the proposed folder, the reason and the
/// confidence, and Confirm / Elsewhere / Reject.
struct ReviewRow: View {
  let item: DropItem
  let busy: Bool
  let confirm: () -> Void
  let elsewhere: () -> Void
  let reject: () -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      HStack(alignment: .firstTextBaseline) {
        Text(item.proposal?.name ?? item.item.name).font(.headline).lineLimit(2)
        Spacer()
        if let confidence = item.confidenceText {
          Text(confidence)
            .font(.caption.monospacedDigit())
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(.tint.opacity(0.15), in: Capsule())
        }
      }
      if let proposal = item.proposal {
        Label(proposal.folderPath.isEmpty ? "/" : proposal.folderPath, systemImage: "folder")
          .font(.subheadline)
        if !proposal.newFolders.isEmpty {
          Text("New folder: \(proposal.newFolders.joined(separator: " / "))")
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        Text(proposal.reason).font(.callout).foregroundStyle(.secondary)
      }
      if item.proposal?.name != nil {
        Text("Was “\(item.item.name)”").font(.caption).foregroundStyle(.secondary)
      }
      HStack {
        Button("Confirm", action: confirm).buttonStyle(.borderedProminent)
        Button("Elsewhere", action: elsewhere).buttonStyle(.bordered)
        Spacer()
        Button("Reject", role: .destructive, action: reject).buttonStyle(.bordered)
      }
      .disabled(busy)
    }
    .padding(.vertical, 4)
  }
}
