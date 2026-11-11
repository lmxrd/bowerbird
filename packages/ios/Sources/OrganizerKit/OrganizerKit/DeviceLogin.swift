import CryptoKit
import Foundation
import Security

/// PKCE (RFC 7636), S256: a random verifier and its challenge.
public struct PKCE: Sendable, Equatable {
  public let verifier: String
  public let challenge: String

  public init(verifier: String) {
    self.verifier = verifier
    self.challenge = PKCE.base64url(Data(SHA256.hash(data: Data(verifier.utf8))))
  }

  /// 32 random bytes, base64url: a 43-character verifier.
  public static func generate() -> PKCE {
    var bytes = [UInt8](repeating: 0, count: 32)
    let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
    precondition(status == errSecSuccess, "no random bytes")
    return PKCE(verifier: base64url(Data(bytes)))
  }

  static func base64url(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }
}

/// The device login (ARCHITECTURE.md §4.4): the browser at
/// `/api/auth/device/authorize`, then the one-time code of the redirect
/// exchanged at `/api/auth/device/token`.
public enum DeviceLogin {
  public enum Problem: Error, Equatable, LocalizedError {
    case wrongRedirect
    case noCode
    case refused(String)

    public var errorDescription: String? {
      switch self {
      case .wrongRedirect, .noCode: "The server did not send the app back a sign-in code."
      case .refused(let reason): reason
      }
    }
  }

  /// Where the browser goes to sign the device in.
  public static func authorizeURL(server: URL, pkce: PKCE, deviceName: String) -> URL {
    var components = URLComponents(
      url: server.appending(path: "api/auth/device/authorize"), resolvingAgainstBaseURL: false)!
    components.queryItems = [
      URLQueryItem(name: "redirect_uri", value: Organizer.redirectURI),
      URLQueryItem(name: "code_challenge", value: pkce.challenge),
      URLQueryItem(name: "code_challenge_method", value: "S256"),
      URLQueryItem(name: "device_name", value: deviceName),
    ]
    // `+` would be read as a space by the server.
    components.percentEncodedQuery = components.percentEncodedQuery?
      .replacingOccurrences(of: "+", with: "%2B")
    return components.url!
  }

  /// The one-time code of the redirect `organizer://auth?code=…`.
  public static func code(from callback: URL) throws(Problem) -> String {
    guard callback.scheme == Organizer.callbackScheme, callback.host == "auth" else {
      throw .wrongRedirect
    }
    let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
    if let error = items.first(where: { $0.name == "error" })?.value {
      throw .refused(error)
    }
    guard let code = items.first(where: { $0.name == "code" })?.value, !code.isEmpty else {
      throw .noCode
    }
    return code
  }

  /// A device name the server takes: 1 to 100 characters.
  public static func deviceName(_ raw: String) -> String {
    let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? "iPhone" : String(trimmed.prefix(100))
  }
}
