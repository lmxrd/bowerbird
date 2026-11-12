// swift-tools-version: 6.0
// OrganizerKit: what the app and its share extension share, and on macOS
// the desktop app's File Provider extension (docs/M5B_DESIGN.md). The API client
// generated from the backend's OpenAPI spec (copied here as openapi.json by
// scripts/spec.sh, never committed), the session in the Keychain, and the
// upload client. The versions are exact (docs/M4_DESIGN.md §4).
import PackageDescription

let package = Package(
  name: "OrganizerKit",
  platforms: [.iOS(.v18), .macOS(.v14)],
  products: [
    // Dynamic: one copy for the app, its extension and the tests.
    .library(name: "OrganizerKit", type: .dynamic, targets: ["OrganizerKit"]),
    // Static: for the desktop app's File Provider extension and its tool
    // on macOS (apps/desktop/macos), which have no Frameworks folder
    // to share a dynamic one in.
    .library(name: "OrganizerKitStatic", type: .static, targets: ["OrganizerKit"]),
  ],
  dependencies: [
    .package(url: "https://github.com/apple/swift-openapi-generator", exact: "1.14.4"),
    .package(url: "https://github.com/apple/swift-openapi-runtime", exact: "1.13.4"),
    .package(url: "https://github.com/apple/swift-openapi-urlsession", exact: "1.4.0"),
    // The runtime's own, named so that it is linked: the middleware uses it.
    .package(url: "https://github.com/apple/swift-http-types", exact: "1.8.0"),
  ],
  targets: [
    .target(
      name: "OrganizerKit",
      dependencies: [
        .product(name: "OpenAPIRuntime", package: "swift-openapi-runtime"),
        .product(name: "OpenAPIURLSession", package: "swift-openapi-urlsession"),
        .product(name: "HTTPTypes", package: "swift-http-types"),
      ],
      path: "OrganizerKit",
      plugins: [.plugin(name: "OpenAPIGenerator", package: "swift-openapi-generator")]
    )
  ]
)
