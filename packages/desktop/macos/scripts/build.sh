#!/usr/bin/env bash
# Builds the File Provider extension and its tool (project.yml) for the
# desktop app's bundle, into build/Products, from where Tauri copies them
# into it (src-tauri/tauri.macos.conf.json, whose beforeBundleCommand runs
# this). Unsigned while project.yml has no team (DEVELOPMENT_TEAM): the
# extension is then in the bundle but does not load (see project.yml).
# Built in build/DerivedData, which is ignored by git.
set -euo pipefail
cd "$(dirname "$0")/.."
../../ios/scripts/spec.sh
xcodegen generate --quiet

signing=()
if grep -q 'DEVELOPMENT_TEAM: ""' project.yml; then
  signing=(CODE_SIGNING_ALLOWED=NO)
fi
xcodebuild build \
  -project OrganizerFileProvider.xcodeproj \
  -scheme OrganizerFileProvider \
  -configuration Release \
  -destination 'generic/platform=macOS' \
  -derivedDataPath build/DerivedData \
  -skipPackagePluginValidation \
  -quiet \
  ${signing[@]+"${signing[@]}"}

products=build/DerivedData/Build/Products/Release
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
