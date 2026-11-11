import Foundation
import Testing

@testable import OrganizerKit

/// A stand-in for the server's upload endpoints, behind a URLProtocol: the
/// resumable protocol as `routes/uploads.rs` speaks it, the plain uploads,
/// and requests that break off half way.
final class FakeServer: @unchecked Sendable {
  nonisolated(unsafe) static var current = FakeServer()

  private let lock = NSLock()
  private var uploads: [String: (name: String, data: Data, complete: Bool)] = [:]
  private(set) var files: [String: Data] = [:]
  private(set) var posts: [(path: String, contentType: String, body: Data)] = []
  /// The query of each upload created, decoded.
  private(set) var created: [[String: String]] = []
  private(set) var log: [String] = []
  /// The next PATCHes break off after this many bytes (one per entry).
  var breakPatches: [Int] = []
  /// The answer to every request, instead of the protocol's.
  var status: Int?

  enum Outcome {
    case answer(Int, [String: String], Data)
    case breakOff
  }

  func handle(_ request: URLRequest, body: Data) -> Outcome {
    lock.withLock {
      let method = request.httpMethod ?? "GET"
      let url = request.url!
      log.append("\(method) \(url.path)")
      if let status { return .answer(status, [:], Data(#"{"error":"Nope"}"#.utf8)) }
      guard request.value(forHTTPHeaderField: "Authorization") == "Bearer org_token" else {
        return .answer(401, [:], Data(#"{"error":"Not signed in"}"#.utf8))
      }
      let headers = { (offset: Int, complete: Bool) in
        ["Upload-Offset": "\(offset)", "Upload-Complete": complete ? "?1" : "?0"]
      }
      switch (method, url.path) {
      case ("POST", "/api/uploads"):
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let name = query.first { $0.name == "name" }?.value ?? ""
        created.append(
          Dictionary(query.map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { $1 }))
        let id = UUID().uuidString
        uploads[id] = (name, Data(), false)
        var answer = headers(0, false)
        answer["Location"] = "/api/uploads/\(id)"
        return .answer(201, answer, Data())
      case ("HEAD", let path) where path.hasPrefix("/api/uploads/"):
        guard let upload = uploads[url.lastPathComponent] else { return .answer(404, [:], Data()) }
        return .answer(204, headers(upload.data.count, upload.complete), Data())
      case ("PATCH", let path) where path.hasPrefix("/api/uploads/"):
        let id = url.lastPathComponent
        guard var upload = uploads[id] else { return .answer(404, [:], Data()) }
        let offset = Int(request.value(forHTTPHeaderField: "Upload-Offset") ?? "") ?? -1
        guard request.value(forHTTPHeaderField: "Content-Type") == "application/partial-upload"
        else {
          return .answer(415, [:], Data())
        }
        guard offset == upload.data.count else {
          return .answer(
            409, headers(upload.data.count, false), Data(#"{"error":"Wrong offset"}"#.utf8))
        }
        if !breakPatches.isEmpty {
          let cut = breakPatches.removeFirst()
          upload.data.append(body.prefix(cut))
          uploads[id] = upload
          return .breakOff
        }
        upload.data.append(body)
        let last = request.value(forHTTPHeaderField: "Upload-Complete") == "?1"
        if last {
          uploads[id] = nil
          files[upload.name] = upload.data
          return .answer(201, headers(upload.data.count, true), Data(node("n", upload.name).utf8))
        }
        uploads[id] = upload
        return .answer(204, headers(upload.data.count, false), Data())
      case ("POST", let path):
        posts.append((path, request.value(forHTTPHeaderField: "Content-Type") ?? "", body))
        return .answer(201, [:], Data("[]".utf8))
      default:
        return .answer(404, [:], Data())
      }
    }
  }
}

final class StubProtocol: URLProtocol {
  override class func canInit(with request: URLRequest) -> Bool { true }
  override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

  override func startLoading() {
    var body = request.httpBody ?? Data()
    if body.isEmpty, let stream = request.httpBodyStream {
      stream.open()
      var buffer = [UInt8](repeating: 0, count: 64 * 1024)
      while stream.hasBytesAvailable {
        let read = stream.read(&buffer, maxLength: buffer.count)
        if read <= 0 { break }
        body.append(buffer, count: read)
      }
      stream.close()
    }
    switch FakeServer.current.handle(request, body: body) {
    case .breakOff:
      client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost))
    case .answer(let status, let headers, let data):
      let response = HTTPURLResponse(
        url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
      client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
      if !data.isEmpty { client?.urlProtocol(self, didLoad: data) }
      client?.urlProtocolDidFinishLoading(self)
    }
  }

  override func stopLoading() {}
}

func stubConfiguration() -> URLSessionConfiguration {
  let configuration = URLSessionConfiguration.ephemeral
  configuration.protocolClasses = [StubProtocol.self]
  return configuration
}

func makeQueue(signedIn: Bool = true) -> UploadQueue {
  let session = StoredSession(
    server: server, token: "org_token", keyID: "k", keyPrefix: "org_tok", deviceName: "iPhone")
  return UploadQueue(
    root: temporaryFolder(), identifier: "tests-\(UUID().uuidString)",
    session: { signedIn ? session : nil }, configuration: { _ in stubConfiguration() },
    foreground: URLSession(configuration: stubConfiguration()))
}

func file(_ size: Int) throws -> URL {
  let url = temporaryFolder().appending(path: "source.bin")
  try Data((0..<size).map { UInt8($0 % 251) }).write(to: url)
  return url
}

/// Waits until every job of the queue is finished, or `seconds` passed.
func settle(_ queue: UploadQueue, seconds: Double = 10) async throws {
  let deadline = Date(timeIntervalSinceNow: seconds)
  while Date() < deadline {
    if queue.jobs().allSatisfy(\.isFinished) { return }
    try await Task.sleep(nanoseconds: 50_000_000)
  }
  Issue.record("The uploads did not finish: \(queue.jobs())")
}

@Suite(.serialized) struct UploadQueueTests {
  init() { FakeServer.current = FakeServer() }

  @Test func smallFilesGoInOnePlainRequest() async throws {
    let queue = makeQueue()
    let source = try file(1000)
    try await queue.enqueueFile(at: source, name: "note.txt", folderID: "folder-1")
    try await settle(queue)
    #expect(queue.jobs().isEmpty)
    let post = try #require(FakeServer.current.posts.first)
    #expect(post.path == "/api/files/folder-1/files")
    #expect(post.contentType.hasPrefix("multipart/form-data; boundary="))
    let text = String(decoding: post.body, as: UTF8.self)
    #expect(text.contains(#"Content-Disposition: form-data; name="file"; filename="note.txt""#))
    #expect(text.contains("Content-Type: text/plain"))
  }

  @Test func largeFilesGoThroughTheResumableUploads() async throws {
    let queue = makeQueue()
    let size = Int(Organizer.plainUploadLimit) + 12345
    let source = try file(size)
    try await queue.enqueueFile(at: source, name: "video.mov", folderID: "folder-1")
    try await settle(queue)
    #expect(queue.jobs().isEmpty)
    #expect(FakeServer.current.files["video.mov"] == (try Data(contentsOf: source)))
    #expect(FakeServer.current.log == ["POST /api/uploads", "PATCH /api/uploads/\(uploadID())"])
  }

  @Test func aBrokenRequestContinuesFromTheServersOffset() async throws {
    let queue = makeQueue()
    let size = Int(Organizer.plainUploadLimit) + 50_000
    let source = try file(size)
    // Two breaks: the second after the HEAD said where to go on from.
    FakeServer.current.breakPatches = [1_000_000, 700_000]
    try await queue.enqueueFile(at: source, name: "big.bin", folderID: "f")
    try await settle(queue)
    #expect(queue.jobs().isEmpty)
    #expect(FakeServer.current.files["big.bin"] == (try Data(contentsOf: source)))
    let id = uploadID()
    #expect(
      FakeServer.current.log == [
        "POST /api/uploads",
        "PATCH /api/uploads/\(id)", "HEAD /api/uploads/\(id)",
        "PATCH /api/uploads/\(id)", "HEAD /api/uploads/\(id)",
        "PATCH /api/uploads/\(id)",
      ])
  }

  @Test func dropBoxFilesGoWithTheirOptions() async throws {
    let queue = makeQueue()
    let a = try file(10)
    let b = try file(20)
    try await queue.enqueueDropBox(
      files: [(a, "a.pdf"), (b, "b.jpg")], note: " The bill ", readContents: false, folders: nil)
    try await settle(queue)
    let post = try #require(FakeServer.current.posts.first)
    #expect(post.path == "/api/drop-box/files")
    let text = String(decoding: post.body, as: UTF8.self)
    #expect(text.contains("name=\"note\"\r\n\r\nThe bill\r\n"))
    #expect(text.contains("name=\"read_contents\"\r\n\r\nfalse\r\n"))
    #expect(!text.contains("name=\"folders\""))
    #expect(text.contains(#"filename="a.pdf""#))
    #expect(text.contains(#"filename="b.jpg""#))
    #expect(text.hasSuffix("--\r\n"))
    // The fields come before the files, as the server reads them.
    #expect(text.range(of: "name=\"note\"")!.lowerBound < text.range(of: "filename=")!.lowerBound)
  }

  @Test func linksGoAsJSON() async throws {
    let queue = makeQueue()
    try queue.enqueueLink("https://example.com/a", note: nil, readContents: true, mode: .snapshot)
    try await settle(queue)
    let post = try #require(FakeServer.current.posts.first)
    #expect(post.path == "/api/drop-box/links")
    #expect(post.contentType == "application/json")
    let link = try JSONDecoder().decode(Components.Schemas.DropLink.self, from: post.body)
    #expect(link.url == "https://example.com/a")
    #expect(link.mode == .snapshot)
    #expect(link.readContents == true)
  }

  @Test func largeDropBoxFilesGoThroughTheResumableUploads() async throws {
    let queue = makeQueue()
    let small = try file(10)
    let large = temporaryFolder().appending(path: "scan.pdf")
    try Data((0..<(Int(Organizer.plainUploadLimit) + 777)).map { UInt8($0 % 241) }).write(
      to: large)
    let expected = try Data(contentsOf: large)
    // A break: it goes on from the server's offset, as into a folder.
    FakeServer.current.breakPatches = [1_000_000]
    let jobs = try await queue.enqueueDropBox(
      files: [(small, "a.txt"), (large, "scan.pdf")], note: " Tax & fees + more ",
      readContents: false, folders: nil, move: true)
    #expect(jobs.count == 2)
    // Moved into the queue, not copied.
    #expect(!FileManager.default.fileExists(atPath: large.path))
    try await settle(queue)
    #expect(queue.jobs().isEmpty)
    #expect(
      FakeServer.current.created == [
        [
          "drop_box": "true", "name": "scan.pdf", "note": "Tax & fees + more",
          "read_contents": "false",
        ]
      ])
    #expect(FakeServer.current.files["scan.pdf"] == expected)
    // The small one goes in the plain request, with the same options.
    let post = try #require(FakeServer.current.posts.first)
    #expect(post.path == "/api/drop-box/files")
    let text = String(decoding: post.body, as: UTF8.self)
    #expect(text.contains(#"filename="a.txt""#))
    #expect(!text.contains(#"filename="scan.pdf""#))
    #expect(text.contains("name=\"note\"\r\n\r\nTax & fees + more\r\n"))
  }

  @Test func aRefusedDropBoxUploadQueuesNothing() async throws {
    let queue = makeQueue()
    let large = temporaryFolder().appending(path: "big.bin")
    try Data(count: Int(Organizer.plainUploadLimit)).write(to: large)
    FakeServer.current.status = 400
    await #expect(throws: APIError.self) {
      try await queue.enqueueDropBox(
        files: [(large, "big.bin")], note: nil, readContents: nil, folders: nil, move: true)
    }
    #expect(queue.jobs().isEmpty)
    // Moved back, to be sent again.
    #expect(FileManager.default.fileExists(atPath: large.path))
  }

  @Test func nothingIsQueuedSignedOut() async throws {
    let queue = makeQueue(signedIn: false)
    await #expect(throws: UploadError.signedOut) {
      try await queue.enqueueFile(at: try file(10), name: "a", folderID: "f")
    }
  }

  @Test func aRefusedUploadFailsWithTheServersMessage() async throws {
    let queue = makeQueue()
    FakeServer.current.status = 409
    try await queue.enqueueFile(at: try file(10), name: "taken.txt", folderID: "f")
    try await settle(queue)
    let job = try #require(queue.jobs().first)
    #expect(job.failure == "Nope")
    FakeServer.current.status = nil
    queue.retry(job.id)
    try await settle(queue)
    #expect(queue.jobs().isEmpty)
  }

  private func uploadID() -> String {
    FakeServer.current.log.first { $0.hasPrefix("PATCH") }?.split(separator: "/").last.map(
      String.init) ?? ""
  }
}

@Suite struct UploadJobTests {
  let upload = URL(string: "https://files.example.com/api/uploads/u1")!

  func response(_ status: Int, _ headers: [String: String] = [:]) -> HTTPURLResponse {
    HTTPURLResponse(url: upload, statusCode: status, httpVersion: nil, headerFields: headers)!
  }

  var job: UploadJob {
    UploadJob(title: "a", folderID: "f", method: .resumable(parentID: "f", name: "a"), length: 100)
  }

  @Test func createsThenSendsTheRest() throws {
    let step = try #require(job.nextStep(server: server, token: "t"))
    #expect(step.request.httpMethod == "POST")
    #expect(
      step.request.url?.absoluteString == "https://files.example.com/api/uploads?parent_id=f&name=a"
    )
    #expect(step.request.value(forHTTPHeaderField: "Upload-Complete") == "?0")
    #expect(step.request.value(forHTTPHeaderField: "Upload-Draft-Interop-Version") == "6")
    #expect(step.body == .none)
    let created = job.advanced(
      by: response(
        201, ["Location": "/api/uploads/u1", "Upload-Offset": "0", "Upload-Complete": "?0"]),
      body: Data(), error: nil, server: server)
    #expect(created.phase == .sending(upload: upload, offset: 0))
    let patch = try #require(created.nextStep(server: server, token: "t"))
    #expect(patch.request.httpMethod == "PATCH")
    #expect(patch.request.value(forHTTPHeaderField: "Upload-Offset") == "0")
    #expect(patch.request.value(forHTTPHeaderField: "Upload-Complete") == "?1")
    #expect(patch.request.value(forHTTPHeaderField: "Content-Type") == "application/partial-upload")
    #expect(patch.body == .tail(from: 0))
  }

  @Test func createsADropBoxUploadWithItsOptions() throws {
    let toDropBox = UploadJob(
      title: "a b+c.pdf", folderID: nil,
      method: .resumableToDropBox(name: "a b+c.pdf", note: "Q&A", readContents: true),
      length: 100)
    let step = try #require(toDropBox.nextStep(server: server, token: "t"))
    #expect(step.request.httpMethod == "POST")
    #expect(
      step.request.url?.absoluteString
        == "https://files.example.com/api/uploads?drop_box=true&name=a%20b%2Bc.pdf&note=Q%26A&read_contents=true"
    )
    #expect(step.request.value(forHTTPHeaderField: "Upload-Length") == "100")
    let bare = UploadJob(
      title: "a", folderID: nil,
      method: .resumableToDropBox(name: "a", note: nil, readContents: nil), length: 1)
    #expect(
      try #require(bare.nextStep(server: server, token: "t")).request.url?.query
        == "drop_box=true&name=a")
    var sending = toDropBox
    sending.phase = .sending(upload: upload, offset: 10)
    #expect(
      try #require(sending.nextStep(server: server, token: "t")).request.httpMethod == "PATCH")
  }

  @Test func asksForTheOffsetAfterABreak() throws {
    var sending = job
    sending.phase = .sending(upload: upload, offset: 10)
    let broken = sending.advanced(
      by: nil, body: Data(), error: URLError(.networkConnectionLost), server: server)
    #expect(broken.phase == .checking(upload: upload))
    #expect(broken.retries == 1)
    #expect(broken.delay == 2)
    let head = try #require(broken.nextStep(server: server, token: "t"))
    #expect(head.request.httpMethod == "HEAD")
    let resumed = broken.advanced(
      by: response(204, ["Upload-Offset": "60", "Upload-Complete": "?0"]), body: Data(), error: nil,
      server: server)
    #expect(resumed.phase == .sending(upload: upload, offset: 60))
    #expect(resumed.retries == 0)
  }

  @Test func takesTheOffsetOfAConflict() {
    var sending = job
    sending.phase = .sending(upload: upload, offset: 10)
    let next = sending.advanced(
      by: response(409, ["Upload-Offset": "42"]), body: Data(), error: nil, server: server)
    #expect(next.phase == .sending(upload: upload, offset: 42))
  }

  @Test func startsAgainWhenTheUploadIsGone() {
    var checking = job
    checking.phase = .checking(upload: upload)
    #expect(
      checking.advanced(by: response(404), body: Data(), error: nil, server: server).phase == .start
    )
  }

  @Test func failsSignedOutOrRefused() {
    var sending = job
    sending.phase = .sending(upload: upload, offset: 0)
    #expect(
      sending.advanced(by: response(401), body: Data(), error: nil, server: server).failure?
        .hasPrefix("Signed out") == true)
    let refused = job.advanced(
      by: response(409), body: Data(#"{"error":"The name is taken"}"#.utf8), error: nil,
      server: server)
    #expect(refused.failure == "The name is taken")
  }

  @Test func givesUpAfterTooManyFailures() {
    var failing = job
    for _ in 0...UploadJob.maxRetries {
      failing = failing.advanced(by: response(503), body: Data(), error: nil, server: server)
    }
    #expect(failing.failure == "The server answered 503")
  }
}

@Suite struct MultipartTests {
  @Test func writesFieldsThenFiles() throws {
    let folder = temporaryFolder()
    let source = folder.appending(path: "in")
    try Data("hello".utf8).write(to: source)
    let writer = MultipartWriter(boundary: "B")
    let size = try writer.write(
      fields: [("note", "hi")], files: [.init(filename: "a \"b\".png", source: source)],
      to: folder.appending(path: "out"))
    let text = try String(contentsOf: folder.appending(path: "out"), encoding: .utf8)
    #expect(
      text
        == "--B\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n"
        + "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a %22b%22.png\"\r\n"
        + "Content-Type: image/png\r\n\r\nhello\r\n--B--\r\n")
    #expect(size == Int64(text.utf8.count))
    #expect(writer.contentType == "multipart/form-data; boundary=B")
  }
}

@Suite struct EventParserTests {
  @Test func readsTheServersEvents() {
    var parser = EventParser()
    let lines = [
      ": keep-alive", "", "event: changes", "data: {}", "", "event: drop-box", "data:", "",
      "event: other", "data: x", "",
    ]
    let events = lines.compactMap { parser.feed($0) }
    #expect(events == [.changes, .dropBox])
  }
}
