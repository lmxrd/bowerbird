import Foundation

/// The live events of `GET /api/events` (server-sent events): `changes`
/// when files changed, `drop-box` when an item of the drop box did.
public enum LiveEvent: String, Sendable, Equatable {
  case changes
  case dropBox = "drop-box"
}

/// Reads server-sent events line by line (the parts the server sends:
/// `event:`, `data:`, comments, blank lines between events).
public struct EventParser: Sendable {
  private var name: String?
  private var hasData = false

  public init() {}

  /// Feeds one line (without its line break); answers the event it ends.
  public mutating func feed(_ line: String) -> LiveEvent? {
    if line.isEmpty {
      defer {
        name = nil
        hasData = false
      }
      guard name != nil || hasData else { return nil }
      return LiveEvent(rawValue: name ?? "message")
    }
    if line.hasPrefix(":") { return nil }
    let field: Substring
    let value: Substring
    if let colon = line.firstIndex(of: ":") {
      field = line[..<colon]
      var rest = line[line.index(after: colon)...]
      if rest.hasPrefix(" ") { rest = rest.dropFirst() }
      value = rest
    } else {
      field = Substring(line)
      value = ""
    }
    switch field {
    case "event": name = String(value)
    case "data": hasData = true
    default: break
    }
    return nil
  }
}

/// Follows `/api/events` while the app is open, connecting again after a
/// pause when the connection drops. A 401 ends it.
public struct EventStream: Sendable {
  let server: URL
  let token: String
  let session: URLSession

  public init(server: URL, token: String, session: URLSession = .shared) {
    self.server = server
    self.token = token
    self.session = session
  }

  public func events() -> AsyncStream<LiveEvent> {
    AsyncStream { continuation in
      let task = Task {
        var delay: UInt64 = 1
        while !Task.isCancelled {
          var request = URLRequest(url: server.appending(path: "api/events"))
          request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
          request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
          request.timeoutInterval = 120
          do {
            let (bytes, response) = try await session.bytes(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if status == 401 { break }
            if status == 200 {
              delay = 1
              // A missed event while away: refresh once connected.
              continuation.yield(.changes)
              continuation.yield(.dropBox)
              // Split by hand: `bytes.lines` drops the blank lines that end
              // an event.
              var parser = EventParser()
              var line: [UInt8] = []
              for try await byte in bytes {
                guard byte == UInt8(ascii: "\n") else {
                  line.append(byte)
                  continue
                }
                if line.last == UInt8(ascii: "\r") { line.removeLast() }
                if let event = parser.feed(String(decoding: line, as: UTF8.self)) {
                  continuation.yield(event)
                }
                line.removeAll(keepingCapacity: true)
              }
            }
          } catch {
            if Task.isCancelled { break }
          }
          try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
          delay = min(delay * 2, 30)
        }
        continuation.finish()
      }
      continuation.onTermination = { _ in task.cancel() }
    }
  }
}
