// organizer-fileprovider: what the desktop app asks of File Provider
// (docs/M5B_DESIGN.md §2). The app is Rust and forbids unsafe code, and
// File Provider is an Objective-C API: rather than calling it through FFI,
// the app runs this small tool, which sits in its bundle beside it
// (Contents/MacOS), so that the system takes it for the app that holds the
// extension. One command per run; errors on stderr, exit code 1.
//
//   add <folder-id> <name>   a domain for an on-demand folder; prints its place
//   remove <folder-id>       the domain goes (unsent changes are kept aside)
//   remove-all               every domain goes (signing out)
//   signal <folder-id>...    the working sets look at the server's changes
//   session                  {"server", "token"} on stdin: kept for the extension
//   forget                   the session goes
import FileProvider
import Foundation
import OrganizerKit

struct Failure: Error, CustomStringConvertible {
  let description: String
}

func domain(_ id: String) async throws -> NSFileProviderDomain {
  let domains = try await NSFileProviderManager.domains()
  guard let domain = domains.first(where: { $0.identifier.rawValue == id }) else {
    throw Failure(description: "No domain for \(id)")
  }
  return domain
}

func run(_ arguments: [String]) async throws {
  switch arguments.first {
  case "add" where arguments.count == 3:
    let domain = NSFileProviderDomain(
      identifier: NSFileProviderDomainIdentifier(rawValue: arguments[1]),
      displayName: arguments[2])
    try await NSFileProviderManager.add(domain)
    guard let manager = NSFileProviderManager(for: domain) else {
      throw Failure(description: "The domain was added, but has no manager")
    }
    let place = try await manager.getUserVisibleURL(for: .rootContainer)
    print(place.path)
  case "remove" where arguments.count == 2:
    let preserved = try await NSFileProviderManager.remove(
      try await domain(arguments[1]), mode: .preserveDirtyUserData)
    if let preserved { print(preserved.path) }
  case "remove-all" where arguments.count == 1:
    try await NSFileProviderManager.removeAllDomains()
  case "signal" where arguments.count >= 2:
    for id in arguments.dropFirst() {
      try await NSFileProviderManager(for: try await domain(id))?.signalEnumerator(
        for: .workingSet)
    }
  case "session" where arguments.count == 1:
    let input = FileHandle.standardInput.readDataToEndOfFile()
    try JSONDecoder().decode(DesktopSession.self, from: input).save()
  case "forget" where arguments.count == 1:
    try DesktopSession.clear()
  default:
    throw Failure(
      description:
        "Usage: organizer-fileprovider add <id> <name> | remove <id> | remove-all | signal <id>... | session | forget"
    )
  }
}

do {
  try await run(Array(CommandLine.arguments.dropFirst()))
} catch {
  FileHandle.standardError.write(Data("\(error)\n".utf8))
  exit(1)
}
