import Foundation

/// The resumable upload protocol as the server speaks it:
/// draft-ietf-httpbis-resumable-upload, interop version 6, without the 104
/// (`routes/uploads.rs`). An upload is created with an empty POST, which
/// answers its `Location`; its bytes go in PATCHes at the offset the server
/// has, and a HEAD asks for that offset after a request broke off.
///
/// The requests and the reading of the answers, without any sending: the
/// upload queue sends them (in a background URLSession).
public enum ResumableUpload {
  static let offset = "Upload-Offset"
  static let complete = "Upload-Complete"
  static let length = "Upload-Length"
  static let interop = "Upload-Draft-Interop-Version"

  /// Creates an upload of a new file in a folder, its bytes to follow.
  public static func create(
    server: URL, token: String, parentID: String, name: String, length: Int64
  ) -> URLRequest {
    create(
      server: server, token: token,
      query: [
        URLQueryItem(name: "parent_id", value: parentID),
        URLQueryItem(name: "name", value: name),
      ], length: length)
  }

  /// Creates an upload of a file's new content, at the version it had (a
  /// file changed since is a conflict), its bytes to follow.
  public static func createReplacement(
    server: URL, token: String, id: String, ifVersion: Int64, length: Int64
  ) -> URLRequest {
    create(
      server: server, token: token,
      query: [
        URLQueryItem(name: "replaces", value: id),
        URLQueryItem(name: "if_version", value: String(ifVersion)),
      ], length: length)
  }

  /// Creates an upload of a file for the drop box, its bytes to follow:
  /// it becomes an item with the note and the read-contents choice (nil:
  /// the user's setting) once complete.
  public static func createInDropBox(
    server: URL, token: String, name: String, note: String?, readContents: Bool?, length: Int64
  ) -> URLRequest {
    var query = [
      URLQueryItem(name: "drop_box", value: "true"),
      URLQueryItem(name: "name", value: name),
    ]
    if let note { query.append(URLQueryItem(name: "note", value: note)) }
    if let readContents {
      query.append(URLQueryItem(name: "read_contents", value: readContents ? "true" : "false"))
    }
    return create(server: server, token: token, query: query, length: length)
  }

  private static func create(
    server: URL, token: String, query: [URLQueryItem], length: Int64
  ) -> URLRequest {
    var components = URLComponents(
      url: server.appending(path: "api/uploads"), resolvingAgainstBaseURL: false)!
    components.queryItems = query
    components.percentEncodedQuery = components.percentEncodedQuery?
      .replacingOccurrences(of: "+", with: "%2B")
    var request = URLRequest(url: components.url!)
    request.httpMethod = "POST"
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
    request.setValue("?0", forHTTPHeaderField: complete)
    request.setValue(String(length), forHTTPHeaderField: Self.length)
    request.setValue(String(Organizer.uploadInteropVersion), forHTTPHeaderField: interop)
    return request
  }

  /// Sends bytes from `offset`; `last` when they reach the end.
  public static func append(upload: URL, token: String, offset: Int64, last: Bool) -> URLRequest {
    var request = URLRequest(url: upload)
    request.httpMethod = "PATCH"
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    request.setValue("application/partial-upload", forHTTPHeaderField: "Content-Type")
    request.setValue(String(offset), forHTTPHeaderField: Self.offset)
    request.setValue(last ? "?1" : "?0", forHTTPHeaderField: complete)
    request.setValue(String(Organizer.uploadInteropVersion), forHTTPHeaderField: interop)
    return request
  }

  /// Asks where an upload is.
  public static func offset(upload: URL, token: String) -> URLRequest {
    var request = URLRequest(url: upload)
    request.httpMethod = "HEAD"
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    request.setValue(String(Organizer.uploadInteropVersion), forHTTPHeaderField: interop)
    request.cachePolicy = .reloadIgnoringLocalCacheData
    return request
  }

  /// What an answer says.
  public enum Answer: Equatable, Sendable {
    /// Created (with where the upload is), or bytes received: go on from
    /// `offset`.
    case progress(upload: URL?, offset: Int64)
    /// The upload is complete and the file is in the tree.
    case complete
    /// The server has another offset than the one sent: go on from it.
    case conflict(offset: Int64)
    /// The upload is gone (expired, cancelled): start again.
    case gone
    /// Not signed in any more.
    case unauthorized
    /// Refused for good, with the server's message.
    case refused(status: Int, message: String)
    /// A server error: try again later.
    case retry(status: Int)
  }

  public static func read(_ response: HTTPURLResponse, body: Data, server: URL) -> Answer {
    let offset = response.value(forHTTPHeaderField: Self.offset).flatMap { Int64($0) }
    let isComplete = response.value(forHTTPHeaderField: complete) == "?1"
    switch response.statusCode {
    case 200, 201, 204:
      if isComplete { return .complete }
      let location = response.value(forHTTPHeaderField: "Location")
        .flatMap { URL(string: $0, relativeTo: server)?.absoluteURL }
      return .progress(upload: location, offset: offset ?? 0)
    case 401:
      return .unauthorized
    case 404, 410:
      return .gone
    case 409 where offset != nil:
      return .conflict(offset: offset!)
    // The other server of a share from another server refused it: not
    // worth a retry.
    case 502 where APIError(status: 502, body: body).isRemoteRefused:
      return .refused(status: 502, message: describe(APIError(status: 502, body: body)))
    case 408, 429, 500...:
      return .retry(status: response.statusCode)
    default:
      return .refused(status: response.statusCode, message: message(body, response.statusCode))
    }
  }

  /// The `{"error": "..."}` of a body, or the status's own words.
  static func message(_ body: Data, _ status: Int) -> String {
    if let decoded = try? JSONDecoder().decode(Components.Schemas.ErrorBody.self, from: body) {
      return decoded.error
    }
    return HTTPURLResponse.localizedString(forStatusCode: status).capitalized
  }
}
