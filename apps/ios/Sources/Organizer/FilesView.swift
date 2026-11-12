import OrganizerKit
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

/// Screen 2: the drives (My drive, the shared drives, the mirrors, the
/// drives from other servers, Shared with me), then their folders in a
/// navigation stack (docs/M7A_DESIGN.md §3).
struct FilesView: View {
  @Environment(AppModel.self) private var app

  var body: some View {
    NavigationStack {
      DrivesView()
        .navigationDestination(for: FolderChoice.self) { folder in
          FolderView(model: FolderModel(folder, encryption: app.encryption))
        }
        .navigationDestination(for: SharedWithMe.self) { _ in
          SharedView()
        }
    }
  }
}

/// The navigation value of "Shared with me".
struct SharedWithMe: Hashable {}

/// The top of the Files tab: each drive opens at its top folder.
struct DrivesView: View {
  @Environment(AppModel.self) private var app
  @State private var model = DrivesModel()

  var body: some View {
    List {
      Section {
        ForEach(model.ownAndShared) { drive in row(drive) }
      }
      // Read-only copies of drives in the user's own storage
      // (docs/M7D_DESIGN.md §5): browsed as a viewer's drive.
      if !model.mirrors.isEmpty {
        Section("Mirrors") {
          ForEach(model.mirrors) { drive in row(drive) }
        }
      }
      // Drives shared from other servers (docs/M7E_DESIGN.md §6): browsed
      // by their role, their bytes through the user's own server; one not
      // accepted yet offers Accept and Decline.
      if !model.remotes.isEmpty {
        Section("From other servers") {
          ForEach(model.remotes) { drive in row(drive) }
        }
      }
      Section {
        NavigationLink(value: SharedWithMe()) {
          Label("Shared with me", systemImage: "person.2.fill")
        }
      }
    }
    .overlay {
      if !model.loaded && model.loading { ProgressView() }
    }
    .navigationTitle("Files")
    .task { await load() }
    .refreshable { await load() }
    // Joining or leaving a drive comes as a change too.
    .onChange(of: app.changes) { Task { await load() } }
    .errorAlert($model.error)
  }

  @ViewBuilder
  private func row(_ drive: Drive) -> some View {
    if let place = DrivePlace(drive) {
      NavigationLink(value: place.root) { DriveLabel(drive: drive) }
    } else if drive.remote?.pending == true {
      VStack(alignment: .leading) {
        DriveLabel(drive: drive)
        AnswerButtons { accept in
          guard let client = app.client else { return }
          await model.answer(drive, accept: accept, client: client)
        }
      }
    } else {
      // Its folder or its storage is not available just now: no
      // actions until it is.
      DriveLabel(drive: drive).foregroundStyle(.secondary)
    }
  }

  private func load() async {
    guard let client = app.client else { return }
    await model.load(client)
  }
}

struct DriveLabel: View {
  let drive: Drive

  var body: some View {
    Label {
      VStack(alignment: .leading) {
        Text(drive.title).lineLimit(1)
        if let remote = drive.remote {
          Text(remote.fromLine).font(.caption).foregroundStyle(.secondary).lineLimit(1)
          if let line = remote.stateLine {
            Text(line).font(.caption).foregroundStyle(.secondary).lineLimit(2)
          }
        }
        if let line = drive.storageLine {
          storage(line)
        } else if let line = drive.mirrorLine {
          Text(line).font(.caption).foregroundStyle(.secondary).lineLimit(2)
        } else if let summary = drive.summary {
          Text(summary).font(.caption).foregroundStyle(.secondary)
        } else if drive.rootId == nil {
          Text("Not available").font(.caption).foregroundStyle(.secondary)
        }
      }
    } icon: {
      Image(systemName: icon).foregroundStyle(Color.accentColor)
    }
  }

  private var icon: String {
    if drive.isMirror { return "externaldrive.fill.badge.timemachine" }
    if drive.isRemote { return "globe" }
    return drive.isShared ? "externaldrive.fill.badge.person.crop" : "externaldrive.fill"
  }

  /// Why its storage is not ready (docs/M7C_DESIGN.md): a lock for a
  /// locked one, a warning for one that fails, arrows for a move.
  private func storage(_ line: String) -> some View {
    let symbol: String =
      switch drive.storage {
      case .unavailable(_, locked: true): "lock.fill"
      case .unavailable: "exclamationmark.icloud"
      case .moving, .ready: "arrow.left.arrow.right"
      }
    return Label(line, systemImage: symbol)
      .font(.caption).foregroundStyle(.secondary).lineLimit(2)
  }
}

/// Accept and Decline, for something shared from another server
/// (docs/M7E_DESIGN.md §5) that waits for the user's answer.
struct AnswerButtons: View {
  let answer: (Bool) async -> Void
  @State private var answering = false

  var body: some View {
    HStack {
      Button("Accept") { send(true) }.buttonStyle(.borderedProminent)
      Button("Decline", role: .destructive) { send(false) }.buttonStyle(.bordered)
    }
    .controlSize(.small)
    .disabled(answering)
  }

  private func send(_ accept: Bool) {
    answering = true
    Task {
      await answer(accept)
      answering = false
    }
  }
}

/// "Shared with me": a folder opens as the top of its own tree, a file in
/// Quick Look. Something shared from another server says from whom
/// ("From alice@cloud.example") and opens once accepted.
struct SharedView: View {
  @Environment(AppModel.self) private var app
  @State private var model = SharedModel()
  @State private var preview: URL?
  @State private var opening: String?

  var body: some View {
    List {
      ForEach(model.items, id: \.node.id) { item in
        if item.awaitsAnswer {
          VStack(alignment: .leading) {
            label(item)
            AnswerButtons { accept in
              guard let client = app.client else { return }
              await model.answer(item, accept: accept, client: client)
            }
          }
        } else if item.node.isFolder {
          NavigationLink(value: SharedModel.place(item.node).root) { label(item) }
        } else {
          Button {
            open(item.node)
          } label: {
            HStack {
              label(item)
              if opening == item.node.id { ProgressView() }
            }
          }
          .foregroundStyle(.primary)
        }
      }
    }
    .overlay {
      if model.loaded && model.items.isEmpty {
        ContentUnavailableView(
          "Nothing is shared with you", systemImage: "person.2",
          description: Text("What others share with you, or with a group you are in, shows here."))
      } else if !model.loaded && model.loading {
        ProgressView()
      }
    }
    .navigationTitle("Shared with me")
    .task { await load() }
    .refreshable { await load() }
    .quickLookPreview($preview)
    .errorAlert($model.error)
  }

  private func label(_ item: SharedItem) -> some View {
    VStack(alignment: .leading, spacing: 2) {
      NodeLabel(node: item.node)
      Text(item.fromLine).font(.caption).foregroundStyle(.secondary)
    }
  }

  private func load() async {
    guard let client = app.client else { return }
    await model.load(client)
  }

  private func open(_ node: FileNode) {
    guard let session = app.session, opening == nil else { return }
    opening = node.id
    Task {
      defer { opening = nil }
      do {
        preview = try await FileCache().open(node, server: session.server, token: session.token)
      } catch {
        model.error = describe(error)
      }
    }
  }
}

struct FolderView: View {
  @Environment(AppModel.self) private var app
  @State var model: FolderModel
  @State private var preview: URL?
  @State private var opening: String?
  @State private var naming: Naming?
  @State private var name = ""
  @State private var moving: FileNode?
  @State private var deleting: FileNode?
  @State private var choosingPhotos = false
  @State private var choosingFiles = false
  @State private var askingPassphrase = false
  @State private var passphrase = ""
  @State private var unlocking = false

  enum Naming: Identifiable {
    case newFolder
    case rename(FileNode)
    var id: String {
      switch self {
      case .newFolder: "new"
      case .rename(let node): node.id
      }
    }
  }

  private var uploads: [UploadJob] {
    guard let id = model.folderID else { return [] }
    return app.jobs.filter { $0.folderID == id }
  }

  var body: some View {
    List {
      // Where its drive is stored, when its members should know: someone's
      // own storage (docs/M7C_DESIGN.md §2), a move under way.
      if model.storageWarning != nil || model.movingNote != nil {
        Section {
          if let warning = model.storageWarning {
            Label(warning, systemImage: "exclamationmark.triangle")
          }
          if let moving = model.movingNote {
            Label(moving, systemImage: "arrow.left.arrow.right")
          }
        }
        .font(.footnote).foregroundStyle(.secondary)
      }
      if !uploads.isEmpty {
        Section("Uploading") {
          ForEach(uploads) { job in
            UploadRow(job: job, sent: app.sent[job.id])
          }
        }
      }
      Section {
        ForEach(model.nodes) { node in
          row(node)
        }
        if model.hasMore, let client = app.client {
          ProgressView().frame(maxWidth: .infinity).task { await model.loadMore(client) }
        }
      } footer: {
        if model.isUploadOnly && !model.nodes.isEmpty {
          Text("You can add files here, and see only what you added.")
        }
      }
    }
    .overlay {
      if model.locked && model.loaded {
        // An encrypted folder this device has not unlocked yet.
        ContentUnavailableView {
          Label("Unlock to see what's inside", systemImage: "lock.fill")
        } description: {
          Text("Enter your encryption passphrase on this device.")
        } actions: {
          if unlocking {
            ProgressView()
          } else {
            Button("Unlock") {
              passphrase = ""
              askingPassphrase = true
            }
            .buttonStyle(.borderedProminent)
          }
        }
      } else if model.loaded && model.nodes.isEmpty && uploads.isEmpty {
        if model.isUploadOnly {
          ContentUnavailableView(
            "Nothing of yours yet", systemImage: "tray.and.arrow.up",
            description: Text("Upload photos or files with +. You see only what you add here."))
        } else if model.actions.contains(.add) {
          ContentUnavailableView(
            "Empty folder", systemImage: "folder",
            description: Text("Upload photos or files, or make a folder, with +."))
        } else {
          ContentUnavailableView("Empty folder", systemImage: "folder")
        }
      } else if !model.loaded && model.loading {
        ProgressView()
      }
    }
    .navigationTitle(model.title)
    .toolbar {
      // Only where one may add: not in a viewer's folder.
      if model.actions.contains(.add) {
        ToolbarItem(placement: .primaryAction) {
          addMenu
        }
      }
    }
    .modifier(
      AddMenuSources(
        choosingPhotos: $choosingPhotos, choosingFiles: $choosingFiles, picked: upload,
        failed: { model.error = $0 })
    )
    .task { await load() }
    .refreshable { await load() }
    .onChange(of: app.changes) { Task { await load() } }
    .onChange(of: app.encryption.unlocked) { Task { await load() } }
    .quickLookPreview($preview)
    .alert(
      namingTitle, isPresented: Binding(get: { naming != nil }, set: { if !$0 { naming = nil } })
    ) {
      TextField("Name", text: $name)
      Button("Cancel", role: .cancel) {}
      Button(namingAction) { commitName() }
    }
    .alert("Unlock Encrypted Folders", isPresented: $askingPassphrase) {
      SecureField("Encryption passphrase", text: $passphrase)
      Button("Cancel", role: .cancel) {}
      Button("Unlock") { unlock() }
    } message: {
      Text("Your encryption passphrase, not your password.")
    }
    .confirmationDialog(
      "Move “\(deleting?.name ?? "")” to the trash?",
      isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
      titleVisibility: .visible
    ) {
      Button("Move to Trash", role: .destructive) {
        if let node = deleting, let client = app.client {
          Task { await model.delete(node, client) }
        }
      }
    } message: {
      Text("You can restore it from the trash on the web.")
    }
    .sheet(item: $moving) { node in
      if let client = app.client {
        // In the same drive: the server refuses a move between drives.
        FolderPicker(
          client: client, title: "Move “\(node.name)”", encryption: app.encryption,
          drive: model.drive
        ) { folder in
          Task { await model.move(node, to: folder.id, client) }
        }
      }
    }
    .errorAlert($model.error)
  }

  /// "+": uploads, and a new folder (upload only adds folders too, and
  /// sees what it adds).
  private var addMenu: some View {
    Menu {
      Button("Upload Photos", systemImage: "photo.on.rectangle") { choosingPhotos = true }
      Button("Upload Files", systemImage: "doc") { choosingFiles = true }
      Divider()
      Button("New Folder", systemImage: "folder.badge.plus") {
        name = ""
        naming = .newFolder
      }
    } label: {
      Label("Add", systemImage: "plus")
    }
    .disabled(model.locked)
  }

  /// Rename, move and delete, each only where the role allows it (an
  /// editor or above): a viewer's and upload only's items offer none.
  @ViewBuilder private func row(_ node: FileNode) -> some View {
    let actions = model.actions(node)
    Group {
      if node.isFolder {
        NavigationLink(value: model.choice(node)) {
          NodeLabel(node: node)
        }
      } else {
        Button {
          open(node)
        } label: {
          HStack {
            NodeLabel(node: node)
            if opening == node.id { ProgressView() }
          }
        }
        .foregroundStyle(.primary)
      }
    }
    .swipeActions(edge: .trailing) {
      if actions.contains(.delete) {
        Button("Delete", systemImage: "trash", role: .destructive) { deleting = node }
      }
      if actions.contains(.move) {
        Button("Move", systemImage: "folder") { moving = node }.tint(.indigo)
      }
    }
    .swipeActions(edge: .leading) {
      if actions.contains(.rename) {
        Button("Rename", systemImage: "pencil") {
          name = node.name
          naming = .rename(node)
        }
        .tint(.orange)
      }
    }
    .contextMenu {
      if actions.contains(.rename) {
        Button("Rename", systemImage: "pencil") {
          name = node.name
          naming = .rename(node)
        }
      }
      if actions.contains(.move) {
        Button("Move", systemImage: "folder") { moving = node }
      }
      if actions.contains(.delete) {
        Button("Delete", systemImage: "trash", role: .destructive) { deleting = node }
      }
    }
  }

  private var namingTitle: String {
    if case .rename = naming { return "Rename" }
    return "New Folder"
  }

  private var namingAction: String {
    if case .rename = naming { return "Rename" }
    return "Create"
  }

  private func commitName() {
    guard let client = app.client, let naming else { return }
    let trimmed = name.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty else { return }
    Task {
      switch naming {
      case .newFolder: await model.createFolder(named: trimmed, client)
      case .rename(let node): await model.rename(node, to: trimmed, client)
      }
    }
  }

  private func load() async {
    guard let client = app.client else { return }
    await model.load(client)
  }

  private func unlock() {
    let passphrase = self.passphrase
    self.passphrase = ""
    guard !passphrase.isEmpty else { return }
    unlocking = true
    Task {
      defer { unlocking = false }
      do {
        try await app.unlock(passphrase: passphrase)
      } catch {
        model.error = describe(error)
      }
    }
  }

  private func open(_ node: FileNode) {
    guard let session = app.session, opening == nil else { return }
    // An encrypted file opens with the folder's keys; one whose name did
    // not decrypt has no key of its generation here either.
    if model.isEncrypted && (model.keys == nil || model.undecryptable.contains(node.id)) {
      model.error = "This item could not be decrypted on this iPhone."
      return
    }
    opening = node.id
    Task {
      defer { opening = nil }
      do {
        preview = try await FileCache().open(
          node, server: session.server, token: session.token, keys: model.keys)
      } catch {
        model.error = describe(error)
      }
    }
  }

  private func upload(_ files: [SharedFile]) {
    guard let client = app.client else { return }
    Task {
      do {
        let folderID = try await model.resolve(client)
        if model.isEncrypted {
          let sealing = try await model.sealingID(of: folderID, source: model.sourceID, client)
          try await app.encryption.enqueue(
            files, into: folderID, sealedFor: sealing, root: model.e2eRoot, queue: app.uploads,
            client)
          app.refreshJobs()
          return
        }
        for file in files {
          try await app.uploads.enqueueFile(
            at: file.url, name: file.name, folderID: folderID, move: true)
        }
      } catch {
        model.error = describe(error)
      }
      app.refreshJobs()
    }
  }
}

struct NodeLabel: View {
  let node: FileNode

  var body: some View {
    Label {
      VStack(alignment: .leading) {
        HStack(spacing: 4) {
          Text(node.name).lineLimit(1)
          // An encrypted folder (or one inside it).
          if node.isFolder && node.e2eRoot != nil {
            Image(systemName: "lock.fill")
              .font(.caption)
              .foregroundStyle(.secondary)
              .accessibilityLabel("Encrypted")
          }
        }
        if !node.isFolder {
          Text(ByteCountFormatter.string(fromByteCount: node.size, countStyle: .file))
            .font(.caption)
            .foregroundStyle(.secondary)
        }
      }
    } icon: {
      Image(systemName: node.isFolder ? "folder.fill" : icon(mime))
        .foregroundStyle(node.isFolder ? Color.accentColor : .secondary)
    }
  }

  /// The server leaves an encrypted file's type out: it is guessed here
  /// from the decrypted name.
  private var mime: String? {
    node.mime
      ?? UTType(filenameExtension: (node.name as NSString).pathExtension)?.preferredMIMEType
  }

  private func icon(_ mime: String?) -> String {
    switch mime?.split(separator: "/").first {
    case "image": "photo"
    case "video": "film"
    case "audio": "waveform"
    case "text": "doc.text"
    default: mime == "application/pdf" ? "doc.richtext" : "doc"
    }
  }
}

/// An upload's row: its name and progress, or why it failed.
struct UploadRow: View {
  @Environment(AppModel.self) private var app
  let job: UploadJob
  let sent: Int64?

  var body: some View {
    VStack(alignment: .leading, spacing: 6) {
      HStack {
        Label(job.title, systemImage: "arrow.up.circle").lineLimit(1)
        Spacer()
        if job.failure != nil {
          Button("Retry") { app.uploads.retry(job.id) }.buttonStyle(.borderless)
          Button("Remove", role: .destructive) { app.uploads.remove(job.id) }.buttonStyle(
            .borderless)
        }
      }
      if let failure = job.failure {
        Text(failure).font(.caption).foregroundStyle(.red)
      } else {
        ProgressView(value: Double(sent ?? 0), total: Double(max(job.length, 1)))
        Text(
          "\(ByteCountFormatter.string(fromByteCount: sent ?? 0, countStyle: .file)) of "
            + ByteCountFormatter.string(fromByteCount: job.length, countStyle: .file)
        )
        .font(.caption)
        .foregroundStyle(.secondary)
      }
    }
  }
}
