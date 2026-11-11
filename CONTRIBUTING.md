# Contributing to Organizer

Thanks for helping. This guide covers the tools, the layout of the code,
how it is tested and how releases work. The design and the decisions behind
it are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); read it first.

## Tools

Everything is pinned and installed by [mise](https://mise.jdx.dev):

```sh
mise install
```

That gives you Rust (from `rust-toolchain.toml`), Node and pnpm (from the
root `package.json`), and zig, cargo-zigbuild, watchexec, osv-scanner and
Trivy (from `mise.toml`). Building the image needs
[podman](https://podman.io) (or Docker, with `CONTAINER=docker` for the
end-to-end run).

## Tasks

| Task                       | What it does                                                                                  |
| -------------------------- | --------------------------------------------------------------------------------------------- |
| `mise run dev`             | The backend on :8000, restarted on changes, and the Vite dev server (open the Vite URL)       |
| `mise run check`           | rustfmt, clippy, oxfmt, oxlint and tsc: what CI checks                                        |
| `mise run fix`             | Formats and applies the lint fixes                                                            |
| `mise run test`            | Unit and integration tests: the backend's (cargo, axum-test) and the frontend's (Vitest)      |
| `mise run test:e2e`        | The end-to-end smoke tests: builds the image, then a real browser goes through it (Playwright) |
| `mise run build`           | The release binary for this machine, `dist/organizer`                                         |
| `mise run build:linux`     | The static Linux binary, `dist/organizer-linux-<arch>` (`ARCH=amd64` or `arm64`)              |
| `mise run build:docker`    | The image, from that binary                                                                   |
| `mise run audit`           | Known vulnerabilities in the lockfiles                                                        |
| `mise run audit:image`     | Known vulnerabilities in an image's Alpine packages (`IMAGE=organizer` for a local one)       |
| `mise run clean`           | Removes builds, `node_modules`, the generated SDK and the local data                          |

The tasks run what they depend on: the OpenAPI spec, then the SDK
generated from it, then the frontend, then the backend that embeds it.

## Layout

```
packages/backend/                   the Rust server
packages/backend/src/main.rs        start, configuration, graceful stop, `organizer healthcheck`
packages/backend/src/lib.rs         the app: API routes, Swagger UI (SWAGGER_UI), the embedded frontend
packages/backend/src/config.rs      environment variables
packages/backend/src/db.rs          SQLite: one writer, read-only readers, migrations
packages/backend/src/error.rs       the API's error type and body
packages/backend/src/frontend.rs    the embedded frontend and its cache policy
packages/backend/src/openapi.rs     the OpenAPI document's title and tags
packages/backend/src/routes/        the handlers
packages/backend/src/bin/openapi.rs writes the spec (mise run gen:spec)
packages/backend/migrations/        the schema, one numbered file per change
packages/backend/tests/api.rs       the API's integration tests
packages/frontend/                  the React app
packages/sdk/                       the API client, generated from the spec (not committed)
packages/e2e/                       the end-to-end smoke tests (Playwright)
scripts/                            the Linux build and the end-to-end run
docs/                               the architecture and the prior art
```

## Tests

- **Unit tests** next to the code (Rust `#[cfg(test)]`, and Vitest for the
  frontend's pure logic in `src/lib`).
- **Integration tests** of the API (`packages/backend/tests/api.rs`, on
  axum-test, against an in-memory database).
- **End-to-end smoke tests** (`packages/e2e`, Playwright): a few flows a
  real browser runs against the built image.

CI runs the checks and the tests on every pull request, the desktop app's
on Linux, macOS and Windows and the mobile apps', builds the image (from a
dev build), checks that it becomes healthy, scans it, and runs the
end-to-end tests against it. It makes dev builds only, within 15 minutes;
the release builds are made when a release is.

## Releases

Releases are automated with
[release-please](https://github.com/googleapis/release-please), from
[Conventional Commits](https://www.conventionalcommits.org): `feat:` bumps
the minor version (before 1.0), `fix:` and `perf:` the patch, and a `!`
marks a breaking change. Every push to main updates a release pull request;
merging it tags the release, attaches the Linux binaries, the OpenAPI
spec and the desktop app's unsigned bundles, and publishes the multi-arch image to the GitHub Container Registry.

Pull requests are squash merged, so **the title of a pull request must be a
Conventional Commit** (`feat: ...`, `fix(auth): ...`), not the branch name
GitHub fills in; a `PR title` check fails the pull request otherwise.
`CHANGELOG.md` is written by release-please: do not edit it by hand.

Dependabot proposes updates weekly; minor, patch and security updates merge
on their own once CI passes, majors wait for a person. The repository needs
"Allow auto-merge" and "Allow GitHub Actions to create and approve pull
requests" switched on in its settings.

## Conventions

Small commits, one logical change each, with a conventional message. Prefer
a well-known, maintained crate or package over writing the same thing
ourselves, and propose a new dependency before adding it. Dependencies must
not be GPL or AGPL (the project is under the Hippocratic License). The rules
for each part are in the `AGENTS.md` files.
