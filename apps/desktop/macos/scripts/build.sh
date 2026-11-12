#!/usr/bin/env bash
# Builds the File Provider extension and its tool (project.yml) for the
# desktop app's bundle, into build/Products, from where Tauri copies them
# into it (src-tauri/tauri.macos.conf.json, whose beforeBundleCommand runs
# this). Unsigned while project.yml has no team (DEVELOPMENT_TEAM): the
# extension is then in the bundle but does not load (see project.yml).
# Built in build/DerivedData, which is ignored by git.
#
#   CONFIGURATION  Release (default: the bundle's, for Arm and Intel Macs)
#                  or Debug (this Mac's architecture only: CI checks that it
#                  builds, in a fraction of the time)
set -euo pipefail
cd "$(dirname "$0")/.."
../../ios/scripts/spec.sh
xcodegen generate --quiet

configuration=${CONFIGURATION:-Release}
settings=()
if [[ $configuration == Debug ]]; then
  settings+=(ARCHS="$(uname -m)")
fi
if grep -q 'DEVELOPMENT_TEAM: ""' project.yml; then
  settings+=(CODE_SIGNING_ALLOWED=NO)
fi
xcodebuild build \
  -project OrganizerFileProvider.xcodeproj \
  -scheme OrganizerFileProvider \
  -configuration "$configuration" \
  -destination 'generic/platform=macOS' \
  -derivedDataPath build/DerivedData \
  -skipPackagePluginValidation \
  -quiet \
  ${settings[@]+"${settings[@]}"}

products=build/DerivedData/Build/Products/$configuration
rm -rf build/Products
mkdir -p build/Products
cp -R "$products/OrganizerFileProvider.appex" build/Products/
cp "$products/organizer-fileprovider" build/Products/

# Swift's back-deployment library of Span, which the Swift packages link
# and macOS before 26 does not have: in the app's Contents/Frameworks,
# where both find it (their rpaths). Xcode copies it into an app it builds;
# the Tauri bundle gets it from here.
mkdir -p build/Products/Frameworks
toolchain=$(dirname "$(xcrun --find swift)")/../lib
span=$(find "$toolchain" -path '*/macosx/libswiftCompatibilitySpan.dylib' | sort | tail -1)
if [[ -z $span ]]; then
  echo "No libswiftCompatibilitySpan.dylib in $toolchain" >&2
  exit 1
fi
cp "$span" build/Products/Frameworks/
