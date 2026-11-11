import Foundation
import HTTPTypes
import OpenAPIRuntime
import OpenAPIURLSession

public typealias FileNode = Components.Schemas.Node
public typealias DropItem = Components.Schemas.ItemView
public typealias DropStatus = Components.Schemas.Status
public typealias DropSettings = Components.Schemas.DropBoxSettingsView
public typealias Drive = Components.Schemas.Drive
public typealias SharedItem = Components.Schemas.SharedItem

/// An answer of the server that is not a success, with the message of its
/// `{"error": "...", "code": "..."}` body, fit to show, and its code when
/// it has one.
public struct APIError: Error, Equatable, LocalizedError {
  public let status: Int
  public let message: String
  /// What a client acts on (`storage_unavailable`, ...); nil for most.
  public let code: String?

  public init(status: Int, message: String, code: String? = nil) {
    self.status = status
    self.message = message
    self.code = code
  }

  /// The error of an answer's body (`ErrorBody`), or of its status alone.
  public init(status: Int, body: Data) {
    if let decoded = try? JSONDecoder().decode(Components.Schemas.ErrorBody.self, from: body) {
      self.init(status: status, message: decoded.error, code: decoded.code)
    } else {
      self.init(
        status: status,
        message: HTTPURLResponse.localizedString(forStatusCode: status).capitalized)
    }
  }

  /// The `code` of a 503 whose drive's storage cannot be used now.
  public static let storageUnavailableCode = "storage_unavailable"

  /// The `code` of a 409 refusing a write to a drive being moved to other
  /// storage: it must not be taken for a version conflict (which makes a
  /// conflict copy).
  public static let readOnlyCode = "read_only"

  /// The `code` of a 502: the other server of a share from another server
  /// (docs/M7E_DESIGN.md §10) cannot be reached now. Worth a retry.
  public static let remoteUnreachableCode = "remote_unreachable"

  /// The `code` of a 502: the other server refused the request.
  public static let remoteRefusedCode = "remote_refused"

  /// Federation is off on this server.
  public static let federationOffCode = "federation_off"

  /// This server refuses the other one.
  public static let serverRefusedCode = "server_refused"

  /// The drive's storage cannot be used now (docs/M7C_DESIGN.md §4): its
  /// backend fails, or its owner's private credentials are locked. The
  /// message is the server's reason.
  public var isStorageUnavailable: Bool {
    status == 503 && code == Self.storageUnavailableCode
  }

  /// The drive is being moved to other storage: read-only until done.
  public var isReadOnly: Bool { status == 409 && code == Self.readOnlyCode }

  /// The other server of a share from another server cannot be reached
  /// now: the system or the queue tries again.
  public var isRemoteUnreachable: Bool { code == Self.remoteUnreachableCode }

  /// The other server refused the request (a 502 all the same): trying
  /// again does not help.
  public var isRemoteRefused: Bool { code == Self.remoteRefusedCode }

  /// Changed on the server since that version, or the name is taken.
  public var isConflict: Bool { status == 409 && !isReadOnly }

  public var errorDescription: String? {
    if isStorageUnavailable { return "This drive is not available now: \(message)." }
    return federationMessage ?? message
  }

  /// What the app says for a refusal about another server, by its code.
  /// The others (`contact_required`, `federation_forbidden`,
  /// `e2e_unsupported`, `drives_unsupported`, `invite_invalid`,
  /// `invite_used`) come only from sharing and invites, which are on the
  /// web: the server's words.
  private var federationMessage: String? {
    switch code {
    case Self.remoteUnreachableCode: "The other server cannot be reached now. Try again later."
    case Self.remoteRefusedCode: "The other server refused this."
    case Self.federationOffCode: "Sharing with other servers is off on this server."
    case Self.serverRefusedCode: "This server no longer accepts the other one."
    default: nil
    }
  }

  /// The APIError behind an error of the generated client, if any.
  public static func of(_ error: any Error) -> APIError? {
    if let error = error as? APIError { return error }
    if let error = error as? ClientError { return of(error.underlyingError) }
    return nil
  }
}

/// A message for any error: the server's own, or the system's.
public func describe(_ error: any Error) -> String {
  if let api = APIError.of(error) { return api.errorDescription ?? api.message }
  if let client = error as? ClientError {
    return client.underlyingError.localizedDescription
  }
  return error.localizedDescription
}

/// Adds the device token to every request, and turns an error answer into
/// an `APIError` (a 401 also tells the app, which signs out).
public struct AuthMiddleware: ClientMiddleware {
  let token: String?
  let onUnauthorized: @Sendable () -> Void

  public init(token: String?, onUnauthorized: @escaping @Sendable () -> Void = {}) {
    self.token = token
    self.onUnauthorized = onUnauthorized
  }

  public func intercept(
    _ request: HTTPRequest, body: HTTPBody?, baseURL: URL, operationID: String,
    next: @Sendable (HTTPRequest, HTTPBody?, URL) async throws -> (HTTPResponse, HTTPBody?)
  ) async throws -> (HTTPResponse, HTTPBody?) {
    var request = request
    if let token { request.headerFields[.authorization] = "Bearer \(token)" }
    let (response, responseBody) = try await next(request, body, baseURL)
    guard response.status.code >= 400 else { return (response, responseBody) }
    if response.status.code == 401 && token != nil { onUnauthorized() }
    var message = response.status.reasonPhrase
    var code: String?
    if let responseBody,
      let data = try? await Data(collecting: responseBody, upTo: 64 * 1024),
      let decoded = try? JSONDecoder().decode(Components.Schemas.ErrorBody.self, from: data)
    {
      message = decoded.error
      code = decoded.code
    }
    throw APIError(status: response.status.code, message: message, code: code)
  }
}

extension Client {
  /// A client of the server, with the device token when there is one.
  public static func organizer(
    server: URL, token: String?, session: URLSession = .shared,
    onUnauthorized: @escaping @Sendable () -> Void = {}
  ) -> Client {
    Client(
      serverURL: server,
      transport: URLSessionTransport(configuration: .init(session: session)),
      middlewares: [AuthMiddleware(token: token, onUnauthorized: onUnauthorized)]
    )
  }

  /// A client on any transport (tests).
  public static func organizer(
    server: URL, token: String?, transport: any ClientTransport,
    onUnauthorized: @escaping @Sendable () -> Void = {}
  ) -> Client {
    Client(
      serverURL: server, transport: transport,
      middlewares: [AuthMiddleware(token: token, onUnauthorized: onUnauthorized)])
  }
}

extension Components.Schemas.ItemView: Identifiable {
  public var id: String { value1.id }
  public var item: Components.Schemas.InboxItem { value1 }
  public var proposal: Components.Schemas.ProposalView? { value2.proposal }
  public var placement: Components.Schemas.PlacementView? { value2.placement }
}

extension Components.Schemas.Node: Identifiable {
  public var isFolder: Bool { kind == .folder }
}

extension Components.Schemas.DropBoxSettingsView {
  public var settings: Components.Schemas.DropBoxSettings { value1 }
}
