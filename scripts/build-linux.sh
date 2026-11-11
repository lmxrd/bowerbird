#!/bin/sh
# The static Linux binary (musl) that the image copies:
# dist/organizer-linux-<arch>. It is built here rather than inside the image,
# so it uses this machine's cargo cache and target directory (wherever
# that is configured), not a cold one in a container. cargo-zigbuild uses
# zig as the C compiler and linker for the musl target (SQLite and other C code), so
# the same command builds on a Mac and on Linux, for either architecture.
#
#   ARCH      amd64 or arm64 (default: this machine's)
#   PROFILE   the cargo profile (default: release)
#   FEATURES  cargo features (default: none)
#
# The frontend must be built first (rust-embed embeds packages/frontend/dist):
# `mise run build:linux` does both.
set -eu

arch=${ARCH:-$(uname -m)}
case "$arch" in
  x86_64 | amd64) arch=amd64 triple=x86_64-unknown-linux-musl ;;
  aarch64 | arm64) arch=arm64 triple=aarch64-unknown-linux-musl ;;
  *)
    echo "unknown architecture: $arch (amd64 or arm64)" >&2
    exit 1
    ;;
esac
profile=${PROFILE:-release}

cd "$(dirname "$0")/../packages/backend"
cargo zigbuild --profile "$profile" --features "${FEATURES:-}" --target "$triple"

# Cargo has no stable flag to put one artifact elsewhere (--artifact-dir
# is nightly only, and --target-dir would leave the shared cache behind),
# so the binary is copied out of the target directory, wherever it is.
target_dir=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
name=organizer-linux-$arch
[ "$profile" = release ] || name=$name-$profile
# The dev profile's folder is `debug`; every other profile's is its name.
out=$profile
[ "$profile" = dev ] && out=debug
mkdir -p ../../dist
cp "$target_dir/$triple/$out/organizer" "../../dist/$name"
echo "dist/$name"
