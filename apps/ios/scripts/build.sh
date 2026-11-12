#!/usr/bin/env bash
# Builds the app and its share extension for the simulator, without
# signing (no team). `mise run build:ios` makes the project first.
set -euo pipefail
# Built in Xcode's own DerivedData (~/Library/Developer/Xcode/DerivedData),
# shared with Xcode and its package cache: nothing in the repository.
cd "$(dirname "$0")/.."
scripts/spec.sh

xcodebuild build \
  -project Organizer.xcodeproj \
  -scheme Organizer \
  -configuration Debug \
  -destination 'generic/platform=iOS Simulator' \
  -skipPackagePluginValidation \
  -quiet \
  CODE_SIGNING_ALLOWED=NO
