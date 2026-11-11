import Foundation

public enum UploadError: Error, Equatable, LocalizedError {
  case signedOut
  case nothingToSend

  public var errorDescription: String? {
    switch self {
    case .signedOut: "Sign in to Organizer in the app first."
    case .nothingToSend: "There is nothing to send."
    }
  }
}

/// The uploads of the app and of its share extension (docs/M4_DESIGN.md
/// §2): each a job in the app group's container, sent by a background
/// URLSession, so that it goes on with the app in the background or closed,
/// and continues from its offset when a request breaks off.
///
/// The share extension makes a queue of its own session (one identifier
/// per share) and leaves; the system wakes the app for that session's
/// events (`handleEvents`), and the app carries the jobs on in its own.
public final class UploadQueue: NSObject, @unchecked Sendable {
  public let root: URL
  public let identifier: String
  private let currentSession: @Sendable () -> StoredSession?
  private let configuration: @Sendable (String) -> URLSessionConfiguration
  private let foreground: URLSession
  private let delegateQueue: OperationQueue

  private let lock = NSLock()
  private var sessions: [String: URLSession] = [:]
  private var bodies: [String: Data] = [:]
  private var bases: [String: Int64] = [:]
  private var sent: [UUID: Int64] = [:]
  private var inFlight: Set<UUID> = []
  private var completions: [String: @Sendable () -> Void] = [:]
  private var observers: [UUID: @Sendable (Event) -> Void] = [:]

  public enum Event: Sendable {
    /// Bytes of a job were sent (`sentBytes(of:)`).
    case progress
    /// A job changed: a new phase, added, removed.
    case changed
    /// A job is done: its file is on the server.
    case finished(UploadJob)
  }

  /// - Parameters:
  ///   - root: where the jobs are kept.
  ///   - identifier: this process's background session.
  ///   - session: the signed-in session (read from the Keychain each time,
  ///     as a sign-out may happen in the other process).
  ///   - configuration: the session configuration of an identifier: a
  ///     background one, but for tests.
  ///   - foreground: for the quick requests made while the user waits.
  public init(
    root: URL = Organizer.sharedContainer().appending(path: "Uploads"),
    identifier: String = Organizer.backgroundSession,
    session: @escaping @Sendable () -> StoredSession? = { SessionStore.load() },
    configuration: @escaping @Sendable (String) -> URLSessionConfiguration = UploadQueue.background,
    foreground: URLSession = .shared
  ) {
    self.root = root
    self.identifier = identifier
    self.currentSession = session
    self.configuration = configuration
    self.foreground = foreground
    self.delegateQueue = OperationQueue()
    self.delegateQueue.maxConcurrentOperationCount = 1
    self.delegateQueue.name = "Organizer uploads"
    super.init()
    try? FileManager.default.createDirectory(at: markers, withIntermediateDirectories: true)
  }

  public static func background(_ identifier: String) -> URLSessionConfiguration {
    let configuration = URLSessionConfiguration.background(withIdentifier: identifier)
    configuration.sharedContainerIdentifier = Organizer.appGroup
    configuration.sessionSendsLaunchEvents = true
    configuration.isDiscretionary = false
    return configuration
  }

  // MARK: Observing

  /// Calls `handler` (on any thread) whenever a job changes; keep the token.
  public func observe(_ handler: @escaping @Sendable (Event) -> Void) -> UUID {
    let token = UUID()
    lock.withLock { observers[token] = handler }
    return token
  }

  public func stopObserving(_ token: UUID) {
    _ = lock.withLock { observers.removeValue(forKey: token) }
  }

  private func notify(_ event: Event) {
    for observer in lock.withLock({ Array(observers.values) }) { observer(event) }
  }

  // MARK: Jobs

  /// The jobs not yet done, oldest first.
  public func jobs() -> [UploadJob] {
    let folders =
      (try? FileManager.default.contentsOfDirectory(
        at: root, includingPropertiesForKeys: nil, options: .skipsHiddenFiles)) ?? []
    return folders.compactMap { load(from: $0) }.sorted { $0.created < $1.created }
  }

  /// The bytes of a job sent so far, while it is being sent.
  public func sentBytes(of id: UUID) -> Int64? {
    lock.withLock { sent[id] }
  }

  /// A file for a folder: plain under 4 MB, else resumable. The file is
  /// copied (or moved, with `move`) into the queue first. `title` is what
  /// the progress row shows, when not the name (an encrypted one's).
  @discardableResult
  public func enqueueFile(
    at source: URL, name: String, folderID: String, move: Bool = false, title: String? = nil
  ) async throws -> UploadJob {
    guard currentSession() != nil else { throw UploadError.signedOut }
    let id = UUID()
    let folder = try makeFolder(id)
    let length = try size(of: source)
    var job: UploadJob
    if length < Organizer.plainUploadLimit {
      let writer = MultipartWriter()
      let total = try writer.write(
        fields: [], files: [.init(filename: name, source: source)],
        to: folder.appending(path: "body"))
      if move { try? FileManager.default.removeItem(at: source) }
      job = UploadJob(
        id: id, title: title ?? name, folderID: folderID,
        method: .post(path: "api/files/\(folderID)/files", contentType: writer.contentType),
        length: total)
    } else {
      let destination = folder.appending(path: "source")
      if move {
        try FileManager.default.moveItem(at: source, to: destination)
      } else {
        try FileManager.default.copyItem(at: source, to: destination)
      }
      job = UploadJob(
        id: id, title: title ?? name, folderID: folderID,
        method: .resumable(parentID: folderID, name: name), length: length)
      job = await create(job)
      // Refused at once (a name taken, a folder gone): said now, not later.
      if let failure = job.failure {
        try? FileManager.default.removeItem(at: folder)
        throw APIError(status: 0, message: failure)
      }
    }
    try save(job)
    start(job)
    return job
  }

  /// Files for the drop box, with the options of the share sheet (nil: the
  /// user's setting): those under 4 MB together in one plain request, each
  /// larger one through the resumable uploads (`drop_box=true`), so that
  /// no size is too large and a break does not send it all again. A file
  /// named by a path in a dropped folder always goes in the plain request,
  /// which keeps the folder. The larger files are copied (or moved, with
  /// `move`) into the queue first.
  @discardableResult
  public func enqueueDropBox(
    files: [(url: URL, name: String)], note: String?, readContents: Bool?, folders: String?,
    move: Bool = false
  ) async throws -> [UploadJob] {
    guard currentSession() != nil else { throw UploadError.signedOut }
    guard !files.isEmpty else { throw UploadError.nothingToSend }
    let note = note?.trimmingCharacters(in: .whitespacesAndNewlines)
    let cleanNote = note?.isEmpty == false ? note : nil
    var plain: [(url: URL, name: String)] = []
    var large: [(url: URL, name: String, length: Int64)] = []
    for file in files {
      let length = try size(of: file.url)
      if length >= Organizer.plainUploadLimit && !file.name.contains("/") {
        large.append((file.url, file.name, length))
      } else {
        plain.append(file)
      }
    }
    // Each resumable one is created at once, so that a refusal (a bad
    // name, a note too long) is said now, before anything is queued.
    var jobs: [UploadJob] = []
    var made: [URL] = []
    var moved: [(from: URL, to: URL)] = []
    do {
      for file in large {
        let id = UUID()
        let folder = try makeFolder(id)
        made.append(folder)
        let destination = folder.appending(path: "source")
        if move {
          try FileManager.default.moveItem(at: file.url, to: destination)
          moved.append((file.url, destination))
        } else {
          try FileManager.default.copyItem(at: file.url, to: destination)
        }
        var job = UploadJob(
          id: id, title: file.name, folderID: nil,
          method: .resumableToDropBox(name: file.name, note: cleanNote, readContents: readContents),
          length: file.length)
        job = await create(job)
        if let failure = job.failure { throw APIError(status: 0, message: failure) }
        jobs.append(job)
      }
      if !plain.isEmpty {
        let id = UUID()
        let folder = try makeFolder(id)
        made.append(folder)
        var fields: [(String, String)] = []
        if let cleanNote { fields.append(("note", cleanNote)) }
        if let readContents { fields.append(("read_contents", readContents ? "true" : "false")) }
        if let folders { fields.append(("folders", folders)) }
        let writer = MultipartWriter()
        let total = try writer.write(
          fields: fields, files: plain.map { .init(filename: $0.name, source: $0.url) },
          to: folder.appending(path: "body"))
        jobs.append(
          UploadJob(
            id: id, title: plain.count == 1 ? plain[0].name : "\(plain.count) files",
            folderID: nil,
            method: .post(path: "api/drop-box/files", contentType: writer.contentType),
            length: total))
      }
      for job in jobs { try save(job) }
    } catch {
      // All or nothing, as one multipart drop: what was moved goes back
      // (to be sent again), what was made goes.
      for file in moved { try? FileManager.default.moveItem(at: file.to, to: file.from) }
      for folder in made { try? FileManager.default.removeItem(at: folder) }
      throw error
    }
    for job in jobs { start(job) }
    return jobs
  }

  /// A link for the drop box.
  @discardableResult
  public func enqueueLink(
    _ url: String, note: String?, readContents: Bool?, mode: Components.Schemas.Links?
  ) throws -> UploadJob {
    guard currentSession() != nil else { throw UploadError.signedOut }
    let id = UUID()
    let folder = try makeFolder(id)
    let trimmed = note?.trimmingCharacters(in: .whitespacesAndNewlines)
    let link = Components.Schemas.DropLink(
      mode: mode, note: trimmed?.isEmpty == false ? trimmed : nil, readContents: readContents,
      url: url)
    let data = try JSONEncoder().encode(link)
    try data.write(to: folder.appending(path: "body"))
    let job = UploadJob(
      id: id, title: URL(string: url)?.host() ?? url, folderID: nil,
      method: .post(path: "api/drop-box/links", contentType: "application/json"),
      length: Int64(data.count))
    try save(job)
    start(job)
    return job
  }

  /// A failed job, tried again from the start.
  public func retry(_ id: UUID) {
    guard var job = load(from: folder(id)) else { return }
    job.phase = .start
    job.retries = 0
    try? save(job)
    start(job)
    notify(.changed)
  }

  /// A job cancelled and forgotten.
  public func remove(_ id: UUID) {
    for session in lock.withLock({ Array(sessions.values) }) {
      session.getAllTasks { tasks in
        for task in tasks where task.taskDescription == id.uuidString { task.cancel() }
      }
    }
    try? FileManager.default.removeItem(at: folder(id))
    _ = lock.withLock { sent.removeValue(forKey: id) }
    notify(.changed)
  }

  // MARK: The app's side

  /// The system woke the app for a background session's events: the
  /// session is made again (so that they are delivered), and `completion`
  /// is called once they all are.
  public func handleEvents(for identifier: String, completion: @escaping @Sendable () -> Void) {
    lock.withLock { completions[identifier] = completion }
    _ = session(identifier)
  }

  /// At the app's start: its own session, and those of share extensions
  /// that may have events waiting; then every job that no task carries on
  /// is started again (a request lost with a process).
  public func reconnect() {
    _ = session(identifier)
    let pending =
      (try? FileManager.default.contentsOfDirectory(atPath: markers.path)) ?? []
    for name in pending where name.hasPrefix(Organizer.extensionSessionPrefix) {
      _ = session(name)
    }
    let all = lock.withLock { Array(sessions.values) }
    let group = DispatchGroup()
    let running = Running()
    for session in all {
      group.enter()
      session.getAllTasks { tasks in
        running.add(tasks.compactMap(\.taskDescription))
        group.leave()
      }
    }
    group.notify(queue: .global()) { [self] in
      let busy = running.names.union(lock.withLock { inFlight.map(\.uuidString) })
      for job in jobs() where !job.isFinished && !busy.contains(job.id.uuidString) {
        start(job)
      }
      for session in all where session.configuration.identifier != identifier {
        session.getAllTasks { [self] tasks in
          if tasks.isEmpty, let name = session.configuration.identifier {
            forget(session: name)
          }
        }
      }
    }
  }

  /// In the share extension: its session is listed in the container, so
  /// that the app looks for its events when it starts.
  public func markAsExtensionSession() {
    FileManager.default.createFile(
      atPath: markers.appending(path: identifier).path, contents: nil)
  }

  // MARK: Sending

  /// The creation of a resumable upload, at once while the user waits (an
  /// empty POST), so that the background session sends the bytes straight
  /// away. Left to the background session if it fails.
  func create(_ job: UploadJob) async -> UploadJob {
    guard let stored = currentSession(),
      let step = job.nextStep(server: stored.server, token: stored.token), step.body == .none
    else { return job }
    do {
      let (data, response) = try await foreground.data(for: step.request)
      return job.advanced(
        by: response as? HTTPURLResponse, body: data, error: nil, server: stored.server)
    } catch {
      return job
    }
  }

  func start(_ job: UploadJob) {
    guard let stored = currentSession() else {
      var failed = job
      failed.phase = .failed(UploadError.signedOut.localizedDescription)
      try? save(failed)
      notify(.changed)
      return
    }
    guard let step = job.nextStep(server: stored.server, token: stored.token) else { return }
    let folder = folder(job.id)
    let session = session(identifier)
    let task: URLSessionTask
    var base: Int64 = 0
    do {
      switch step.body {
      case .none:
        task = session.downloadTask(with: step.request)
      case .file(let name):
        task = session.uploadTask(with: step.request, fromFile: folder.appending(path: name))
      case .tail(let offset):
        base = offset
        let file = try tail(of: folder, from: offset)
        task = session.uploadTask(with: step.request, fromFile: file)
      }
    } catch {
      var failed = job
      failed.phase = .failed(describe(error))
      try? save(failed)
      notify(.changed)
      return
    }
    task.taskDescription = job.id.uuidString
    if job.delay > 0 { task.earliestBeginDate = Date(timeIntervalSinceNow: job.delay) }
    lock.withLock {
      bases[key(session, task)] = base
      inFlight.insert(job.id)
    }
    task.resume()
  }

  /// `source` from `offset`: the file itself from 0, else its rest copied
  /// to `tail`.
  private func tail(of folder: URL, from offset: Int64) throws -> URL {
    let source = folder.appending(path: "source")
    if offset == 0 { return source }
    let tail = folder.appending(path: "tail")
    try? FileManager.default.removeItem(at: tail)
    FileManager.default.createFile(atPath: tail.path, contents: nil)
    let input = try FileHandle(forReadingFrom: source)
    defer { try? input.close() }
    let output = try FileHandle(forWritingTo: tail)
    defer { try? output.close() }
    try input.seek(toOffset: UInt64(offset))
    while let chunk = try input.read(upToCount: 1 << 20), !chunk.isEmpty {
      try output.write(contentsOf: chunk)
    }
    return tail
  }

  private func session(_ identifier: String) -> URLSession {
    lock.withLock {
      if let session = sessions[identifier] { return session }
      let session = URLSession(
        configuration: configuration(identifier), delegate: self, delegateQueue: delegateQueue)
      sessions[identifier] = session
      return session
    }
  }

  private func forget(session identifier: String) {
    let session = lock.withLock { sessions.removeValue(forKey: identifier) }
    session?.finishTasksAndInvalidate()
    try? FileManager.default.removeItem(at: markers.appending(path: identifier))
  }

  private func key(_ session: URLSession, _ task: URLSessionTask) -> String {
    "\(session.configuration.identifier ?? "")/\(task.taskIdentifier)"
  }

  // MARK: Storage

  private var markers: URL { root.appending(path: ".sessions") }

  private func folder(_ id: UUID) -> URL { root.appending(path: id.uuidString) }

  private func makeFolder(_ id: UUID) throws -> URL {
    let folder = folder(id)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    return folder
  }

  private func load(from folder: URL) -> UploadJob? {
    guard let data = try? Data(contentsOf: folder.appending(path: "job.json")) else { return nil }
    return try? JSONDecoder().decode(UploadJob.self, from: data)
  }

  func save(_ job: UploadJob) throws {
    let data = try JSONEncoder().encode(job)
    try data.write(to: folder(job.id).appending(path: "job.json"), options: .atomic)
  }

  private func size(of url: URL) throws -> Int64 {
    let values = try url.resourceValues(forKeys: [.fileSizeKey])
    return Int64(values.fileSize ?? 0)
  }
}

/// The task names collected from several sessions at once.
private final class Running: @unchecked Sendable {
  private let lock = NSLock()
  private var all: Set<String> = []
  func add(_ names: [String]) { lock.withLock { all.formUnion(names) } }
  var names: Set<String> { lock.withLock { all } }
}

extension UploadQueue: URLSessionDataDelegate, URLSessionDownloadDelegate {
  public func urlSession(
    _ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data
  ) {
    let key = key(session, dataTask)
    lock.withLock { bodies[key, default: Data()].append(data) }
  }

  public func urlSession(
    _ session: URLSession, downloadTask: URLSessionDownloadTask,
    didFinishDownloadingTo location: URL
  ) {
    // Only answers are downloaded (a HEAD, a creation): small.
    let data = (try? Data(contentsOf: location)) ?? Data()
    let key = key(session, downloadTask)
    lock.withLock { bodies[key] = data.prefix(64 * 1024) }
  }

  public func urlSession(
    _ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64,
    totalBytesSent: Int64, totalBytesExpectedToSend: Int64
  ) {
    guard let id = task.taskDescription.flatMap(UUID.init(uuidString:)) else { return }
    let key = key(session, task)
    lock.withLock { sent[id] = (bases[key] ?? 0) + totalBytesSent }
    notify(.progress)
  }

  public func urlSession(
    _ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?
  ) {
    let key = key(session, task)
    let body = lock.withLock {
      bases.removeValue(forKey: key)
      return bodies.removeValue(forKey: key) ?? Data()
    }
    guard let id = task.taskDescription.flatMap(UUID.init(uuidString:)) else { return }
    lock.withLock {
      inFlight.remove(id)
      sent.removeValue(forKey: id)
    }
    // Removed meanwhile (cancelled by the user).
    guard let job = load(from: folder(id)) else { return }
    guard let stored = currentSession() else {
      var failed = job
      failed.phase = .failed(UploadError.signedOut.localizedDescription)
      try? save(failed)
      notify(.changed)
      return
    }
    let next = job.advanced(
      by: task.response as? HTTPURLResponse, body: body, error: error, server: stored.server)
    if next.phase == .done {
      try? FileManager.default.removeItem(at: folder(id))
      notify(.finished(next))
    } else {
      try? save(next)
      if !next.isFinished { start(next) }
    }
    notify(.changed)
  }

  public func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
    guard let identifier = session.configuration.identifier else { return }
    let completion = lock.withLock { completions.removeValue(forKey: identifier) }
    if let completion { DispatchQueue.main.async { completion() } }
    if identifier != self.identifier {
      session.getAllTasks { [self] tasks in
        if tasks.isEmpty { forget(session: identifier) }
      }
    }
  }
}
