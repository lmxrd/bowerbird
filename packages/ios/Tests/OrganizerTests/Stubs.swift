import Foundation
import HTTPTypes
import OpenAPIRuntime
import OrganizerKit

/// A transport for the generated client that answers from a closure:
/// the view models' tests run against it.
struct StubTransport: ClientTransport {
  struct Request: Sendable {
    var method: HTTPRequest.Method
    var path: String
    var body: Data?
    var authorization: String?
  }

  let handler: @Sendable (Request) async throws -> (Int, String)

  func send(
    _ request: HTTPRequest, body: HTTPBody?, baseURL: URL, operationID: String
  ) async throws -> (HTTPResponse, HTTPBody?) {
    var data: Data?
    if let body { data = try await Data(collecting: body, upTo: 10 << 20) }
    let (status, json) = try await handler(
      Request(
        method: request.method, path: request.path ?? "", body: data,
        authorization: request.headerFields[.authorization]))
    var response = HTTPResponse(status: .init(code: status))
    response.headerFields[.contentType] = "application/json"
    return (response, json.isEmpty ? nil : HTTPBody(json))
  }
}

/// Records what a stub was asked, across tasks.
final class Log: @unchecked Sendable {
  private let lock = NSLock()
  private var entries: [String] = []
  func add(_ entry: String) { lock.withLock { entries.append(entry) } }
  var all: [String] { lock.withLock { entries } }
}

let server = URL(string: "https://files.example.com")!

/// A node as the server lists it, with the user's role on it (`my_role`)
/// and, in a mirror, the node it copies (`source_id`).
func node(
  _ id: String, _ name: String, folder: Bool = false, version: Int64 = 1, role: String = "owner",
  source: String? = nil, remote: String? = nil
) -> String {
  """
  {"id":"\(id)","drive_id":"d1","parent_id":"root","name":"\(name)","kind":"\(folder ? "folder" : "file")",\
  "size":\(folder ? 0 : 12),"mime":\(folder ? "null" : "\"text/plain\""),"version":\(version),\
  "portable":true,"created_at":"2026-11-07T10:00:00Z","updated_at":"2026-11-07T10:00:00Z",\
  "e2e_root":null,"source_id":\(source.map { "\"\($0)\"" } ?? "null"),"my_role":"\(role)",\
  "remote":\(remote.map { "\"\($0)\"" } ?? "null")}
  """
}

func item(_ id: String, status: String, proposal: Bool = false, placement: Bool = false) -> String {
  let proposalJSON =
    proposal
    ? """
    ,"proposal":{"folder_id":"f1","folder_path":"/Taxes/2026","new_folders":[],"path":"/Taxes/2026/bill.pdf",\
    "reason":"A tax bill","confidence":0.87,"model":"m","created_at":"2026-11-07T10:00:00Z"}
    """ : ""
  let placementJSON =
    placement
    ? #","placement":{"id":"p-\#(id)","node_id":"n1","applied_at":"2026-11-07T10:00:00Z","path":"/Taxes/bill.pdf"}"#
    : ""
  return """
    {"id":"\(id)","kind":"file","name":"bill.pdf","read_contents":true,"status":"\(status)",\
    "created_at":"2026-11-07T10:00:00Z","updated_at":"2026-11-07T10:00:00Z"\(proposalJSON)\(placementJSON)}
    """
}

let settingsJSON = """
  {"pending_mode":"stay","read_contents":"per_item","folder_freedom":"balanced","folders":"whole",\
  "links":"snapshot","recipient":{"enabled":false,"provider":"","endpoint":"","model":"",\
  "embeddings_model":"","embeddings_endpoint":""}}
  """

/// A JSON body without its whitespace (the client pretty-prints).
func compact(_ body: Data?) -> String {
  String(decoding: body ?? Data(), as: UTF8.self).filter { !$0.isWhitespace }
}
