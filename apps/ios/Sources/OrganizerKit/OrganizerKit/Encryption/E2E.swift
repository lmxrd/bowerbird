import CommonCrypto
import CryptoKit
import Foundation

/// What can go wrong with encrypted data. `decrypt` is a failed tag: a
/// wrong key or passphrase, or bytes that were changed.
public enum E2EError: Error, Equatable, LocalizedError {
  case decrypt
  case malformed(String)
  case version(UInt8)
  case noKey(UInt32)

  public var errorDescription: String? {
    switch self {
    case .decrypt: "It could not be decrypted: a wrong key, or changed data."
    case .malformed(let what): "Not encrypted data Organizer can read: \(what)."
    case .version(let version): "Encrypted with a newer format (\(version)): update Organizer."
    case .noKey(let generation): "No key of generation \(generation) for this folder."
    }
  }
}

/// The format of end-to-end-encrypted folders, docs/E2E_SPEC.md, with
/// CryptoKit (X25519, HKDF-SHA256, AES-256-GCM) and CommonCrypto
/// (PBKDF2-HMAC-SHA256). The reference implementation is
/// packages/e2e-crypto; the tests check this one against its vectors.
public enum E2E {
  /// The format's version: the first byte of every wrapped key, encrypted
  /// name and encrypted file.
  public static let version: UInt8 = 1
  /// The fewest PBKDF2 iterations accepted: stored parameters are the
  /// server's to change, and a low count would make a guess cheap.
  public static let minIterations = 100_000
  /// The most bytes (UTF-8, NFC) a name may have, so that encrypted and
  /// encoded it still fits the 255 bytes of most filesystems.
  public static let maxNameBytes = 158
  /// The plaintext bytes of a segment; the last one has 0 to this many.
  public static let segment = 65_536
  public static let tag = 16
  public static let sealedSegment = segment + tag
  /// Version, generation, file nonce.
  public static let headerLength = 1 + 4 + 32
  /// A wrapped folder key: version, ephemeral public key, nonce, the key
  /// and its tag.
  public static let wrappedFolderKeyLength = 1 + 32 + 12 + 32 + 16

  static let privateKeyAD = Data("organizer-e2e/1/private-key".utf8)
  static let recoveryInfo = Data("organizer-e2e/1/recovery".utf8)
  static let folderKeyInfo = Data("organizer-e2e/1/folder-key".utf8)
  static let nameInfo = Data("organizer-e2e/1/name".utf8)
  static let fileInfo = Data("organizer-e2e/1/file".utf8)

  // MARK: Primitives

  /// Random bytes (the system's generator, which is cryptographically
  /// secure on Apple's platforms).
  public static func random(_ count: Int) -> Data {
    var generator = SystemRandomNumberGenerator()
    return Data((0..<count).map { _ in UInt8.random(in: .min ... .max, using: &generator) })
  }

  static func hkdf(_ ikm: Data, salt: Data, info: Data) -> Data {
    HKDF<SHA256>.deriveKey(
      inputKeyMaterial: SymmetricKey(data: ikm), salt: salt, info: info, outputByteCount: 32
    ).withUnsafeBytes { Data($0) }
  }

  /// AES-256-GCM: the ciphertext, then the tag.
  static func seal(_ key: Data, nonce: Data, _ plain: Data, ad: Data) throws -> Data {
    let box = try AES.GCM.seal(
      plain, using: SymmetricKey(data: key), nonce: AES.GCM.Nonce(data: nonce),
      authenticating: ad)
    return box.ciphertext + box.tag
  }

  static func open(_ key: Data, nonce: Data, _ sealed: Data, ad: Data) throws -> Data {
    guard sealed.count >= tag else { throw E2EError.malformed("shorter than a tag") }
    let sealed = Data(sealed)
    do {
      let box = try AES.GCM.SealedBox(
        nonce: AES.GCM.Nonce(data: nonce), ciphertext: sealed.prefix(sealed.count - tag),
        tag: sealed.suffix(tag))
      return try AES.GCM.open(box, using: SymmetricKey(data: key), authenticating: ad)
    } catch {
      throw E2EError.decrypt
    }
  }

  /// The bytes after the version, which must be this format's.
  static func checkVersion(_ bytes: Data) throws -> Data {
    guard let first = bytes.first else { throw E2EError.malformed("empty") }
    guard first == version else { throw E2EError.version(first) }
    return Data(bytes.dropFirst())
  }

  static func bigEndian(_ value: UInt32) -> Data {
    withUnsafeBytes(of: value.bigEndian) { Data($0) }
  }

  static func uint32(_ bytes: Data) -> UInt32 {
    bytes.prefix(4).reduce(0) { $0 << 8 | UInt32($1) }
  }

  /// A UUID's 16 bytes, as associated data.
  static func bytes(of id: UUID) -> Data {
    withUnsafeBytes(of: id.uuid) { Data($0) }
  }

  /// Unpadded base64url (RFC 4648 §5), as the API sends binary values.
  public static func base64url(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }

  /// The bytes of unpadded base64url; anything else is refused.
  public static func unbase64url(_ text: String) throws -> Data {
    let allowed = Set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_")
    guard text.allSatisfy(allowed.contains), text.count % 4 != 1 else {
      throw E2EError.malformed("not base64url")
    }
    var standard = text.replacingOccurrences(of: "-", with: "+")
      .replacingOccurrences(of: "_", with: "/")
    standard += String(repeating: "=", count: (4 - standard.count % 4) % 4)
    guard let data = Data(base64Encoded: standard) else {
      throw E2EError.malformed("not base64url")
    }
    return data
  }

  // MARK: Identity keys (§2)

  /// X25519's public key of a private key.
  public static func publicKey(of privateKey: Data) throws -> Data {
    try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: privateKey).publicKey
      .rawRepresentation
  }

  /// X25519; an all-zero result (a public key of small order) is refused.
  static func x25519(_ privateKey: Data, _ publicKey: Data) throws -> Data {
    let shared: Data
    do {
      let ours = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: privateKey)
      let theirs = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: publicKey)
      shared = try ours.sharedSecretFromKeyAgreement(with: theirs).withUnsafeBytes { Data($0) }
    } catch {
      throw E2EError.malformed("not an X25519 key")
    }
    guard shared.contains(where: { $0 != 0 }) else {
      throw E2EError.malformed("a public key of small order")
    }
    return shared
  }

  /// PBKDF2-HMAC-SHA256 of the passphrase's UTF-8 in NFC, so that the same
  /// passphrase typed on any system is the same key; 32 bytes.
  public static func passphraseKey(_ passphrase: String, salt: Data, iterations: Int) throws
    -> Data
  {
    let password = Data(passphrase.precomposedStringWithCanonicalMapping.utf8)
    guard iterations > 0, iterations <= Int(UInt32.max) else {
      throw E2EError.malformed("PBKDF2 iterations")
    }
    var key = [UInt8](repeating: 0, count: 32)
    let status = password.withUnsafeBytes { password in
      salt.withUnsafeBytes { salt in
        CCKeyDerivationPBKDF(
          CCPBKDFAlgorithm(kCCPBKDF2), password.bindMemory(to: CChar.self).baseAddress,
          password.count, salt.bindMemory(to: UInt8.self).baseAddress, salt.count,
          CCPseudoRandomAlgorithm(kCCPRFHmacAlgSHA256), UInt32(iterations), &key, key.count)
      }
    }
    guard status == kCCSuccess else { throw E2EError.malformed("PBKDF2 refused (\(status))") }
    return Data(key)
  }

  /// The private key sealed under a wrapping key: version, nonce,
  /// ciphertext and tag (61 bytes).
  public static func wrapPrivate(_ privateKey: Data, with wrappingKey: Data, nonce: Data)
    throws -> Data
  {
    try Data([version]) + nonce + seal(wrappingKey, nonce: nonce, privateKey, ad: privateKeyAD)
  }

  public static func unwrapPrivate(_ wrapped: Data, with wrappingKey: Data) throws -> Data {
    let rest = try checkVersion(wrapped)
    guard rest.count == 12 + 32 + 16 else {
      throw E2EError.malformed("a wrapped private key is 61 bytes")
    }
    return try open(wrappingKey, nonce: rest.prefix(12), rest.dropFirst(12), ad: privateKeyAD)
  }

  /// The private key out of its passphrase wrapping, refusing too few
  /// iterations.
  public static func unwrapPrivate(
    _ wrapped: Data, passphrase: String, salt: Data, iterations: Int
  ) throws -> Data {
    guard iterations >= minIterations else {
      throw E2EError.malformed("too few PBKDF2 iterations")
    }
    return try unwrapPrivate(
      wrapped, with: passphraseKey(passphrase, salt: salt, iterations: iterations))
  }

  // MARK: The recovery key (§3)

  static let base32 = Array("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567".utf8)

  public static func recoveryWrappingKey(_ recovery: Data) -> Data {
    hkdf(recovery, salt: Data(), info: recoveryInfo)
  }

  /// The recovery key as shown: 52 base32 characters in 13 groups of 4.
  public static func encodeRecovery(_ key: Data) -> String {
    var characters: [UInt8] = []
    var buffer: UInt32 = 0
    var bits = 0
    for byte in key {
      buffer = buffer << 8 | UInt32(byte)
      bits += 8
      while bits >= 5 {
        bits -= 5
        characters.append(base32[Int(buffer >> UInt32(bits) & 31)])
        buffer &= (1 << UInt32(bits)) - 1
      }
    }
    if bits > 0 { characters.append(base32[Int(buffer << UInt32(5 - bits) & 31)]) }
    return stride(from: 0, to: characters.count, by: 4).map {
      String(decoding: characters[$0..<min($0 + 4, characters.count)], as: UTF8.self)
    }.joined(separator: "-")
  }

  /// The recovery key as typed: dashes and white space ignored, lowercase
  /// read as uppercase; anything else refused, unused bits that are not
  /// zero (a typo) too.
  public static func decodeRecovery(_ text: String) throws -> Data {
    let clean = Array(
      text.unicodeScalars
        .filter { $0 != "-" && !$0.properties.isWhitespace }
        .map { $0.isASCII ? Character($0).uppercased() : String($0) }
        .joined().utf8)
    guard clean.count == 52, clean.allSatisfy(base32.contains) else {
      throw E2EError.malformed("a recovery key is 52 base32 characters")
    }
    var key = Data()
    var buffer: UInt32 = 0
    var bits = 0
    for character in clean {
      buffer = buffer << 5 | UInt32(base32.firstIndex(of: character)!)
      bits += 5
      if bits >= 8 {
        bits -= 8
        key.append(UInt8(buffer >> UInt32(bits) & 0xff))
        buffer &= (1 << UInt32(bits)) - 1
      }
    }
    guard key.count == 32, buffer == 0 else { throw E2EError.malformed("not a recovery key") }
    return key
  }

  // MARK: Folder keys (§4)

  /// The folder's id (16 bytes) and the generation (4 bytes).
  static func wrapAD(folder: UUID, generation: UInt32) -> Data {
    bytes(of: folder) + bigEndian(generation)
  }

  /// The salt is both public keys, the ephemeral one first.
  static func wrappingKey(shared: Data, ephemeralPublic: Data, recipient: Data) -> Data {
    hkdf(shared, salt: ephemeralPublic + recipient, info: folderKeyInfo)
  }

  /// A folder key wrapped for the member whose public key is `recipient`
  /// (93 bytes), with new random values unless they are given (tests).
  public static func wrapFolderKey(
    _ folderKey: Data, folder: UUID, generation: UInt32, for recipient: Data,
    ephemeral: Data = random(32), nonce: Data = random(12)
  ) throws -> Data {
    let ephemeralPublic = try publicKey(of: ephemeral)
    let shared = try x25519(ephemeral, recipient)
    let key = wrappingKey(shared: shared, ephemeralPublic: ephemeralPublic, recipient: recipient)
    let sealed = try seal(
      key, nonce: nonce, folderKey, ad: wrapAD(folder: folder, generation: generation))
    return Data([version]) + ephemeralPublic + nonce + sealed
  }

  /// The folder key out of a wrapping for the holder of `privateKey`; it
  /// fails for another folder or generation.
  public static func unwrapFolderKey(
    _ wrapped: Data, folder: UUID, generation: UInt32, privateKey: Data
  ) throws -> Data {
    let rest = try checkVersion(wrapped)
    guard wrapped.count == wrappedFolderKeyLength else {
      throw E2EError.malformed("a wrapped folder key is 93 bytes")
    }
    let ephemeralPublic = Data(rest.prefix(32))
    let nonce = Data(rest.dropFirst(32).prefix(12))
    let shared = try x25519(privateKey, ephemeralPublic)
    let key = wrappingKey(
      shared: shared, ephemeralPublic: ephemeralPublic, recipient: try publicKey(of: privateKey))
    return try open(
      key, nonce: nonce, Data(rest.dropFirst(44)),
      ad: wrapAD(folder: folder, generation: generation))
  }

  // MARK: Names (§6)

  public static func nameKey(_ folderKey: Data) -> Data {
    hkdf(folderKey, salt: Data(), info: nameInfo)
  }

  /// A name sealed for the folder `parent`: unpadded base64url of version,
  /// generation, nonce, ciphertext and tag.
  public static func encryptName(
    _ name: String, folderKey: Data, generation: UInt32, parent: UUID,
    nonce: Data = random(12)
  ) throws -> String {
    let plain = Data(name.precomposedStringWithCanonicalMapping.utf8)
    guard !plain.isEmpty else { throw E2EError.malformed("an empty name") }
    guard plain.count <= maxNameBytes else {
      throw E2EError.malformed("a name of at most \(maxNameBytes) bytes")
    }
    let sealed = try seal(nameKey(folderKey), nonce: nonce, plain, ad: bytes(of: parent))
    return base64url(Data([version]) + bigEndian(generation) + nonce + sealed)
  }

  /// The generation of the folder key a name was sealed with.
  public static func nameGeneration(_ encrypted: String) throws -> UInt32 {
    let rest = try checkVersion(unbase64url(encrypted))
    guard rest.count >= 4 + 12 + 16 else { throw E2EError.malformed("too short for a name") }
    return uint32(rest)
  }

  /// The name out of `encryptName`, with the key of its generation; it
  /// fails in another parent.
  public static func decryptName(_ encrypted: String, folderKey: Data, parent: UUID) throws
    -> String
  {
    let rest = try checkVersion(unbase64url(encrypted))
    guard rest.count >= 4 + 12 + 16 else { throw E2EError.malformed("too short for a name") }
    let plain = try open(
      nameKey(folderKey), nonce: Data(rest.dropFirst(4).prefix(12)), Data(rest.dropFirst(16)),
      ad: bytes(of: parent))
    guard let name = String(data: plain, encoding: .utf8) else {
      throw E2EError.malformed("not UTF-8")
    }
    return name
  }

  // MARK: Contents (§5)

  public static func fileKey(_ folderKey: Data, fileNonce: Data) -> Data {
    hkdf(folderKey, salt: fileNonce, info: fileInfo)
  }

  /// Seven zero bytes, the segment's number and 1 for the last, else 0.
  static func segmentNonce(_ index: UInt32, last: Bool) -> Data {
    Data(repeating: 0, count: 7) + bigEndian(index) + Data([last ? 1 : 0])
  }

  /// The size of the encrypted file of a plaintext of `length` bytes.
  public static func encryptedSize(_ length: Int64) -> Int64 {
    let segments = max(1, (length + Int64(segment) - 1) / Int64(segment))
    return Int64(headerLength) + length + Int64(tag) * segments
  }

  /// The size of the plaintext in an encrypted file of `length` bytes.
  public static func plainSize(_ length: Int64) throws -> Int64 {
    guard length >= Int64(headerLength + tag) else {
      throw E2EError.malformed("shorter than a header and a segment")
    }
    let body = length - Int64(headerLength)
    let full = body / Int64(sealedSegment)
    let rest = body % Int64(sealedSegment)
    if rest == 0 { return full * Int64(segment) }
    guard rest >= Int64(tag) else { throw E2EError.malformed("a segment shorter than its tag") }
    return full * Int64(segment) + rest - Int64(tag)
  }

  /// Encrypts what `read` gives (up to the count asked, less only at the
  /// end) to `write`, a segment at a time. Answers the bytes written.
  @discardableResult
  public static func encrypt(
    folderKey: Data, generation: UInt32, fileNonce: Data = random(32),
    read: (Int) throws -> Data, write: (Data) throws -> Void
  ) throws -> Int64 {
    let header = Data([version]) + bigEndian(generation) + fileNonce
    try write(header)
    let key = fileKey(folderKey, fileNonce: fileNonce)
    var written = Int64(header.count)
    var current = try read(segment)
    var index: UInt32 = 0
    while true {
      // A full segment is the last one only when nothing follows it.
      let next = try current.count == segment ? read(segment) : Data()
      let last = next.isEmpty
      let sealed = try seal(key, nonce: segmentNonce(index, last: last), current, ad: Data())
      try write(sealed)
      written += Int64(sealed.count)
      if last { return written }
      guard index < UInt32.max else { throw E2EError.malformed("too many segments") }
      index += 1
      current = next
    }
  }

  /// Decrypts what `read` gives to `write`, a segment at a time, with the
  /// key of the file's generation. `write` may have had segments before a
  /// failure: write to a temporary file.
  @discardableResult
  public static func decrypt(
    keys: (UInt32) -> Data?, read: (Int) throws -> Data, write: (Data) throws -> Void
  ) throws -> Int64 {
    let header = try read(headerLength)
    guard header.count == headerLength else { throw E2EError.malformed("shorter than the header") }
    let rest = try checkVersion(header)
    let generation = uint32(rest)
    guard let folderKey = keys(generation) else { throw E2EError.noKey(generation) }
    let key = fileKey(folderKey, fileNonce: Data(rest.dropFirst(4)))
    var written: Int64 = 0
    var current = try read(sealedSegment)
    var index: UInt32 = 0
    while true {
      guard current.count >= tag else { throw E2EError.malformed("a segment shorter than its tag") }
      let next = try current.count == sealedSegment ? read(sealedSegment) : Data()
      let last = next.isEmpty
      let plain = try open(key, nonce: segmentNonce(index, last: last), current, ad: Data())
      try write(plain)
      written += Int64(plain.count)
      if last { return written }
      guard index < UInt32.max else { throw E2EError.malformed("too many segments") }
      index += 1
      current = next
    }
  }

  /// A whole plaintext encrypted in memory.
  public static func encrypt(
    _ plain: Data, folderKey: Data, generation: UInt32, fileNonce: Data = random(32)
  ) throws -> Data {
    var reader = DataReader(plain)
    var out = Data()
    try encrypt(
      folderKey: folderKey, generation: generation, fileNonce: fileNonce,
      read: { reader.read($0) }, write: { out.append($0) })
    return out
  }

  /// A whole file decrypted in memory.
  public static func decrypt(_ encrypted: Data, keys: (UInt32) -> Data?) throws -> Data {
    var reader = DataReader(encrypted)
    var out = Data()
    try decrypt(keys: keys, read: { reader.read($0) }, write: { out.append($0) })
    return out
  }

  /// A file encrypted to another, streamed. Answers the bytes written.
  @discardableResult
  public static func encryptFile(
    at source: URL, to destination: URL, folderKey: Data, generation: UInt32,
    fileNonce: Data = random(32)
  ) throws -> Int64 {
    let input = try FileHandle(forReadingFrom: source)
    defer { try? input.close() }
    FileManager.default.createFile(atPath: destination.path, contents: nil)
    let output = try FileHandle(forWritingTo: destination)
    defer { try? output.close() }
    return try encrypt(
      folderKey: folderKey, generation: generation, fileNonce: fileNonce,
      read: { try readFull(input, $0) }, write: { try output.write(contentsOf: $0) })
  }

  /// A file decrypted to another, streamed through a temporary file that
  /// takes `destination`'s place only once the last segment checked.
  @discardableResult
  public static func decryptFile(at source: URL, to destination: URL, keys: (UInt32) -> Data?)
    throws -> Int64
  {
    let input = try FileHandle(forReadingFrom: source)
    defer { try? input.close() }
    let temporary = destination.deletingLastPathComponent()
      .appending(path: ".\(UUID().uuidString).part")
    FileManager.default.createFile(atPath: temporary.path, contents: nil)
    do {
      let output = try FileHandle(forWritingTo: temporary)
      defer { try? output.close() }
      let written = try decrypt(
        keys: keys, read: { try readFull(input, $0) }, write: { try output.write(contentsOf: $0) })
      try? FileManager.default.removeItem(at: destination)
      try FileManager.default.moveItem(at: temporary, to: destination)
      return written
    } catch {
      try? FileManager.default.removeItem(at: temporary)
      throw error
    }
  }

  /// Up to `count` bytes, fewer only at the end of the file.
  static func readFull(_ handle: FileHandle, _ count: Int) throws -> Data {
    var data = Data()
    while data.count < count {
      guard let chunk = try handle.read(upToCount: count - data.count), !chunk.isEmpty else {
        break
      }
      data.append(chunk)
    }
    return data
  }
}

/// Reads a Data in pieces, as a file would be read.
struct DataReader {
  let data: Data
  var offset = 0

  init(_ data: Data) { self.data = Data(data) }

  mutating func read(_ count: Int) -> Data {
    let end = min(offset + count, data.count)
    defer { offset = end }
    return data.subdata(in: offset..<end)
  }
}
