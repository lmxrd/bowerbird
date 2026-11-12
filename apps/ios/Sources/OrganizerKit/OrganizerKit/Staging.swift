import Foundation

/// Where picked and shared files are copied before they are sent: the app
/// group's container, so that a background URLSession (of the app or of
/// the share extension) can read them, and nothing large stays in memory.
public enum Staging {
  public static var root: URL {
    Organizer.sharedContainer().appending(path: "Uploads").appending(path: ".staging")
  }

  /// A new folder for one share or pick.
  public static func makeFolder() throws -> URL {
    let folder = root.appending(path: UUID().uuidString)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    return folder
  }

  /// Copies a file into `folder` under `name` (made safe and unique).
  public static func copy(_ source: URL, named name: String, into folder: URL) throws -> SharedFile
  {
    var destination = folder.appending(path: safeName(name))
    var number = 2
    while FileManager.default.fileExists(atPath: destination.path) {
      let base = (safeName(name) as NSString).deletingPathExtension
      let ext = (name as NSString).pathExtension
      destination = folder.appending(
        path: ext.isEmpty ? "\(base) \(number)" : "\(base) \(number).\(ext)")
      number += 1
    }
    try FileManager.default.copyItem(at: source, to: destination)
    let size = try destination.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
    return SharedFile(url: destination, name: destination.lastPathComponent, size: Int64(size))
  }

  /// A name without path separators or line breaks; never empty.
  public static func safeName(_ name: String) -> String {
    let cleaned = name.replacingOccurrences(of: "/", with: "-")
      .replacingOccurrences(of: ":", with: "-")
      .components(separatedBy: .newlines).joined(separator: " ")
      .trimmingCharacters(in: .whitespaces)
    return cleaned.isEmpty || cleaned.hasPrefix(".") ? "File\(cleaned)" : cleaned
  }

  /// Removes what is left of old shares (a day old).
  public static func sweep(olderThan age: TimeInterval = 24 * 3600) {
    let folders =
      (try? FileManager.default.contentsOfDirectory(
        at: root, includingPropertiesForKeys: [.creationDateKey])) ?? []
    for folder in folders {
      let created = (try? folder.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? .now
      if created.timeIntervalSinceNow < -age { try? FileManager.default.removeItem(at: folder) }
    }
  }
}
