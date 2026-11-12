import Foundation

/// What the user types as the server's address, made a URL: https unless
/// http is spelled out, no path, query or fragment.
public enum ServerAddress {
  public enum Problem: Error, Equatable, LocalizedError {
    case empty
    case invalid

    public var errorDescription: String? {
      switch self {
      case .empty: "Enter the address of your Organizer server."
      case .invalid: "That is not a server address, such as files.example.com."
      }
    }
  }

  public static func parse(_ input: String) throws(Problem) -> URL {
    var text = input.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !text.isEmpty else { throw .empty }
    if !text.contains("://") {
      text = "https://" + text
    }
    guard var components = URLComponents(string: text),
      let scheme = components.scheme?.lowercased(),
      scheme == "https" || scheme == "http",
      let host = components.host, !host.isEmpty, !host.contains(" ")
    else { throw .invalid }
    components.scheme = scheme
    components.host = host.lowercased()
    // A server under a path (behind a proxy) keeps it; a trailing slash goes.
    while components.path.hasSuffix("/") { components.path.removeLast() }
    components.query = nil
    components.fragment = nil
    components.user = nil
    components.password = nil
    guard let url = components.url else { throw .invalid }
    return url
  }
}
