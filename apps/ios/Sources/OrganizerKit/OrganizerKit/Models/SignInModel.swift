import Foundation
import Observation

/// The sign-in: the server's address, then the device login in the
/// browser, then the token.
@MainActor
@Observable
public final class SignInModel {
  public var address = ""
  public private(set) var working = false
  public var error: String?

  /// The browser sign-in under way.
  public struct Attempt: Sendable, Equatable {
    public let server: URL
    public let pkce: PKCE
    public let deviceName: String
    public let url: URL
  }

  public init(address: String = "") {
    self.address = address
  }

  /// Checks the address and makes the browser's URL.
  public func begin(deviceName: String, pkce: PKCE = .generate()) -> Attempt? {
    error = nil
    do {
      let server = try ServerAddress.parse(address)
      let name = DeviceLogin.deviceName(deviceName)
      return Attempt(
        server: server, pkce: pkce, deviceName: name,
        url: DeviceLogin.authorizeURL(server: server, pkce: pkce, deviceName: name))
    } catch {
      self.error = error.localizedDescription
      return nil
    }
  }

  /// The browser came back: the code is exchanged for the device's token.
  public func finish(_ attempt: Attempt, callback: URL, app: AppModel) async {
    working = true
    defer { working = false }
    do {
      let code = try DeviceLogin.code(from: callback)
      let created = try await app.anonymousClient(server: attempt.server)
        .deviceToken(body: .json(.init(code: code, codeVerifier: attempt.pkce.verifier)))
        .ok.body.json
      try app.signedIn(
        StoredSession(
          server: attempt.server, token: created.token, keyID: created.key.id,
          keyPrefix: created.key.prefix, deviceName: created.key.name))
      error = nil
    } catch {
      self.error = describe(error)
    }
  }

  /// The browser sign-in did not finish.
  public func failed(_ error: any Error) {
    let nsError = error as NSError
    // Cancelled by the user (ASWebAuthenticationSessionError.canceledLogin).
    if nsError.domain == "com.apple.AuthenticationServices.WebAuthenticationSession"
      && nsError.code == 1
    {
      return
    }
    self.error = describe(error)
  }
}
