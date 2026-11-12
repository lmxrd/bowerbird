import Foundation

/// An upload waiting or under way, kept as `job.json` in its own folder of
/// the app group's container, next to what it sends: `source` (the file,
/// for a resumable upload) or `body` (a multipart form or JSON, sent in one
/// request). The share extension writes it; the app (woken by the
/// background URLSession) carries it on.
public struct UploadJob: Codable, Sendable, Identifiable, Equatable {
  public enum Method: Codable, Sendable, Equatable {
    /// One POST of `body` to `path` (under the server), e.g. a small file
    /// to `/api/files/{id}/files`, files to the drop box, a link.
    case post(path: String, contentType: String)
    /// `source` through the resumable uploads, into a folder.
    case resumable(parentID: String, name: String)
    /// `source` through the resumable uploads, into the drop box, with the
    /// item's options (nil: the user's setting).
    case resumableToDropBox(name: String, note: String?, readContents: Bool?)
  }

  public enum Phase: Codable, Sendable, Equatable {
    /// The first request is next: the POST, or the creation of the upload.
    case start
    /// The rest of the file goes next, from `offset`.
    case sending(upload: URL, offset: Int64)
    /// A request broke off: ask the server where the upload is.
    case checking(upload: URL)
    case done
    case failed(String)
  }

  public var id: UUID
  /// What the progress row shows: the file's name, or "3 files".
  public var title: String
  /// The folder it goes into (the Files screen shows its progress there);
  /// nil for the drop box.
  public var folderID: String?
  public var method: Method
  /// The bytes to send in all.
  public var length: Int64
  public var phase: Phase
  /// Failed attempts in a row.
  public var retries: Int
  public var created: Date

  public init(
    id: UUID = UUID(), title: String, folderID: String?, method: Method, length: Int64,
    phase: Phase = .start, retries: Int = 0, created: Date = .now
  ) {
    self.id = id
    self.title = title
    self.folderID = folderID
    self.method = method
    self.length = length
    self.phase = phase
    self.retries = retries
    self.created = created
  }

  public var isFinished: Bool {
    switch phase {
    case .done, .failed: true
    default: false
    }
  }

  public var failure: String? {
    if case .failed(let message) = phase { return message }
    return nil
  }

  /// After this many failed attempts in a row, the job fails.
  public static let maxRetries = 8
}

/// The next request of a job, and what it sends.
public struct UploadStep: Equatable, Sendable {
  public enum Body: Equatable, Sendable {
    /// No body: sent as a download task (a background session has no
    /// data tasks).
    case none
    /// The whole of the job's `body` or `source`.
    case file(String)
    /// `source` from this offset (copied to `tail` first, unless 0).
    case tail(from: Int64)
  }

  public var request: URLRequest
  public var body: Body
}

extension UploadJob {
  /// The request that carries the job on, or nil when it is finished.
  public func nextStep(server: URL, token: String) -> UploadStep? {
    switch (method, phase) {
    case (_, .done), (_, .failed):
      return nil
    case (.post(let path, let contentType), _):
      var request = URLRequest(url: server.appending(path: path))
      request.httpMethod = "POST"
      request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
      request.setValue(contentType, forHTTPHeaderField: "Content-Type")
      return UploadStep(request: request, body: .file("body"))
    case (.resumable(let parentID, let name), .start):
      return UploadStep(
        request: ResumableUpload.create(
          server: server, token: token, parentID: parentID, name: name, length: length),
        body: .none)
    case (.resumableToDropBox(let name, let note, let readContents), .start):
      return UploadStep(
        request: ResumableUpload.createInDropBox(
          server: server, token: token, name: name, note: note, readContents: readContents,
          length: length),
        body: .none)
    case (.resumable, .sending(let upload, let offset)),
      (.resumableToDropBox, .sending(let upload, let offset)):
      // All the rest in one request: if it breaks off, what arrived stays.
      return UploadStep(
        request: ResumableUpload.append(upload: upload, token: token, offset: offset, last: true),
        body: .tail(from: offset))
    case (.resumable, .checking(let upload)), (.resumableToDropBox, .checking(let upload)):
      return UploadStep(request: ResumableUpload.offset(upload: upload, token: token), body: .none)
    }
  }

  /// The job after its request ended: with an answer, or broken off.
  public func advanced(
    by response: HTTPURLResponse?, body: Data, error: (any Error)?, server: URL
  ) -> UploadJob {
    var job = self
    // Broken off (even with an answer begun): what the server has is asked
    // for next.
    guard let response, error == nil else {
      return job.retried(error.map(describe) ?? "No answer from the server")
    }
    switch method {
    case .post:
      switch response.statusCode {
      case 200..<300:
        job.phase = .done
      case 401:
        job.phase = .failed("Signed out: sign in again in the app")
      // The other server of a share from another server refused it.
      case 502 where APIError(status: 502, body: body).isRemoteRefused:
        job.phase = .failed(describe(APIError(status: 502, body: body)))
      case 408, 429, 500...:
        return job.retried(Self.retryReason(body, response.statusCode))
      // The drive is being moved: read-only for a while, tried again.
      case 409 where APIError(status: 409, body: body).isReadOnly:
        return job.retried(Self.retryReason(body, response.statusCode))
      default:
        job.phase = .failed(ResumableUpload.message(body, response.statusCode))
      }
      return job
    case .resumable, .resumableToDropBox:
      let answer = ResumableUpload.read(response, body: body, server: server)
      switch (phase, answer) {
      case (_, .complete):
        job.phase = .done
      case (.start, .progress(let upload?, let offset)):
        job.phase = .sending(upload: upload, offset: offset)
        job.retries = 0
      case (.start, .progress(nil, _)):
        job.phase = .failed("The server did not say where the upload goes")
      case (.sending(let upload, _), .progress(_, let offset)),
        (.checking(let upload), .progress(_, let offset)),
        (.sending(let upload, _), .conflict(let offset)),
        (.checking(let upload), .conflict(let offset)):
        job.phase = .sending(upload: upload, offset: offset)
        job.retries = 0
      case (.start, .gone):
        job.phase = .failed(ResumableUpload.message(body, response.statusCode))
      case (_, .gone):
        // Expired or cancelled on the server: from the start again.
        job.phase = .start
      case (_, .unauthorized):
        job.phase = .failed("Signed out: sign in again in the app")
      case (_, .refused(_, let message)):
        job.phase = .failed(message)
      case (_, .retry(let status)):
        return job.retried(Self.retryReason(body, status))
      case (.start, .conflict):
        job.phase = .failed(ResumableUpload.message(body, response.statusCode))
      case (.done, _), (.failed, _):
        break
      }
      return job
    }
  }

  /// One more failed attempt: the same request again (or, after a PATCH
  /// broke off, a HEAD), until too many.
  /// Why a request is tried again: the drive's storage unavailable, the
  /// drive being moved, or the other server of a share from another server
  /// away, in words; else its status.
  static func retryReason(_ body: Data, _ status: Int) -> String {
    let error = APIError(status: status, body: body)
    if error.isStorageUnavailable || error.isReadOnly || error.isRemoteUnreachable {
      return describe(error)
    }
    return "The server answered \(status)"
  }

  func retried(_ reason: String) -> UploadJob {
    var job = self
    job.retries += 1
    if job.retries > Self.maxRetries {
      job.phase = .failed(reason)
      return job
    }
    if case .sending(let upload, _) = phase { job.phase = .checking(upload: upload) }
    return job
  }

  /// How long to wait before the next attempt: none for the first, then
  /// doubling up to five minutes.
  public var delay: TimeInterval {
    retries == 0 ? 0 : min(300, pow(2, Double(retries)))
  }
}
