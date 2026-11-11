import OrganizerKit
import SwiftUI

/// Screen 5: the server, this device, and Sign out. Everything else is set
/// on the web.
struct SettingsView: View {
  @Environment(AppModel.self) private var app
  @State private var confirming = false
  @State private var signingOut = false

  var body: some View {
    NavigationStack {
      Form {
        if let session = app.session {
          Section("Server") {
            Link(destination: session.server) {
              LabeledContent(
                "Address", value: session.server.host() ?? session.server.absoluteString)
            }
            LabeledContent("Signed in as", value: app.email ?? "…")
          }
          Section {
            LabeledContent("Name", value: session.deviceName)
            LabeledContent("Token", value: "\(session.keyPrefix)…")
            LabeledContent(
              "Signed in", value: session.signedInAt.formatted(date: .abbreviated, time: .shortened)
            )
          } header: {
            Text("This device")
          } footer: {
            Text("Listed on the web under Settings → Devices, where you can revoke it.")
          }
          Section {
            Link(
              "Open the settings on the web",
              destination: session.server.appending(path: "settings"))
          } footer: {
            Text(
              "Your account, the drop box's rules, the AI connection, the trash, versions and backups are set on the web."
            )
          }
          if app.encryption.unlocked {
            Section {
              Button("Lock Encrypted Folders", systemImage: "lock") { app.encryption.lock() }
            } footer: {
              Text(
                "Encrypted folders are unlocked on this iPhone until you lock them or sign out.")
            }
          }
          Section {
            Button(role: .destructive) {
              confirming = true
            } label: {
              HStack {
                Text("Sign Out")
                if signingOut {
                  Spacer()
                  ProgressView()
                }
              }
            }
            .disabled(signingOut)
          }
        }
      }
      .navigationTitle("Settings")
      .confirmationDialog(
        "Sign out of Organizer?", isPresented: $confirming, titleVisibility: .visible
      ) {
        Button("Sign Out", role: .destructive) {
          signingOut = true
          Task {
            await app.signOut()
            signingOut = false
          }
        }
      } message: {
        Text("Uploads not yet sent will stop.")
      }
    }
  }
}
