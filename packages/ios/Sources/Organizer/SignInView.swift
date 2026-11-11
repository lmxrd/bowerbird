import AuthenticationServices
import OrganizerKit
import SwiftUI
import UIKit

/// Screen 1: the server's address, then the sign-in in the browser (the
/// web's own login: password, single sign-on, two-factor), which sends the
/// app back a code for the device's token.
struct SignInView: View {
  @Environment(AppModel.self) private var app
  @Environment(\.webAuthenticationSession) private var webAuthenticationSession
  @State private var model = SignInModel()
  @FocusState private var focused: Bool

  var body: some View {
    NavigationStack {
      Form {
        Section {
          VStack(spacing: 12) {
            Image(systemName: "tray.full")
              .font(.system(size: 48))
              .foregroundStyle(.tint)
            Text("Organizer").font(.largeTitle.bold())
            Text("Your files, and a drop box that puts things in their place.")
              .multilineTextAlignment(.center)
              .foregroundStyle(.secondary)
          }
          .frame(maxWidth: .infinity)
          .padding(.vertical)
          .listRowBackground(Color.clear)
        }
        Section {
          TextField("files.example.com", text: $model.address)
            .keyboardType(.URL)
            .textContentType(.URL)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .focused($focused)
            .submitLabel(.continue)
            .onSubmit(signIn)
        } header: {
          Text("Server address")
        } footer: {
          Text("You sign in on your server's own page, then come back here.")
        }
        if let notice = app.notice {
          Section { Text(notice).foregroundStyle(.secondary) }
        }
        if let error = model.error {
          Section { Text(error).foregroundStyle(.red) }
        }
        Section {
          Button(action: signIn) {
            HStack {
              Spacer()
              if model.working { ProgressView() } else { Text("Continue").bold() }
              Spacer()
            }
          }
          .disabled(model.working || model.address.trimmingCharacters(in: .whitespaces).isEmpty)
        }
      }
      .navigationTitle("Sign in")
      .navigationBarTitleDisplayMode(.inline)
      .onAppear { focused = model.address.isEmpty }
    }
  }

  private func signIn() {
    guard let attempt = model.begin(deviceName: UIDevice.current.name) else { return }
    Task {
      do {
        let callback = try await webAuthenticationSession.authenticate(
          using: attempt.url, callback: .customScheme(Organizer.callbackScheme),
          preferredBrowserSession: .shared, additionalHeaderFields: [:])
        await model.finish(attempt, callback: callback, app: app)
      } catch {
        model.failed(error)
      }
    }
  }
}
