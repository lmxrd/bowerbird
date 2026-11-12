import Foundation
import UniformTypeIdentifiers

/// A multipart/form-data body written to a file, a piece at a time, so
/// that a large file never sits in memory (the share extension has little)
/// and a background URLSession can send it.
public struct MultipartWriter: Sendable {
  public struct Part: Sendable {
    public var field: String
    public var filename: String
    public var source: URL

    public init(field: String = "file", filename: String, source: URL) {
      self.field = field
      self.filename = filename
      self.source = source
    }
  }

  public let boundary: String
  public var contentType: String { "multipart/form-data; boundary=\(boundary)" }

  public init(boundary: String = "organizer-\(UUID().uuidString)") {
    self.boundary = boundary
  }

  /// Writes the fields, then the files, to `destination`; answers its size.
  @discardableResult
  public func write(fields: [(String, String)], files: [Part], to destination: URL) throws -> Int64
  {
    FileManager.default.createFile(atPath: destination.path, contents: nil)
    let out = try FileHandle(forWritingTo: destination)
    defer { try? out.close() }
    for (name, value) in fields {
      try out.write(contentsOf: Data("--\(boundary)\r\n".utf8))
      try out.write(
        contentsOf: Data(
          "Content-Disposition: form-data; name=\"\(Self.quoted(name))\"\r\n\r\n".utf8))
      try out.write(contentsOf: Data("\(value)\r\n".utf8))
    }
    for part in files {
      try out.write(contentsOf: Data("--\(boundary)\r\n".utf8))
      try out.write(
        contentsOf: Data(
          ("Content-Disposition: form-data; name=\"\(Self.quoted(part.field))\"; "
            + "filename=\"\(Self.quoted(part.filename))\"\r\n").utf8))
      try out.write(
        contentsOf: Data("Content-Type: \(Self.mimeType(of: part.filename))\r\n\r\n".utf8))
      let input = try FileHandle(forReadingFrom: part.source)
      defer { try? input.close() }
      while let chunk = try input.read(upToCount: 1 << 20), !chunk.isEmpty {
        try out.write(contentsOf: chunk)
      }
      try out.write(contentsOf: Data("\r\n".utf8))
    }
    try out.write(contentsOf: Data("--\(boundary)--\r\n".utf8))
    return Int64(try out.offset())
  }

  /// A name in a quoted header parameter: no quotes, no line breaks
  /// (as browsers write them).
  static func quoted(_ value: String) -> String {
    value.replacingOccurrences(of: "\"", with: "%22")
      .replacingOccurrences(of: "\r", with: "%0D")
      .replacingOccurrences(of: "\n", with: "%0A")
  }

  static func mimeType(of filename: String) -> String {
    let ext = (filename as NSString).pathExtension
    return UTType(filenameExtension: ext)?.preferredMIMEType ?? "application/octet-stream"
  }
}
