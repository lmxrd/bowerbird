#!/usr/bin/env bash
# The tests (OrganizerTests, hosted by the app) on an iPhone simulator
# (scripts/simulator.sh picks one; SIMULATOR=<udid> another).
set -euo pipefail
# Built in Xcode's own DerivedData (~/Library/Developer/Xcode/DerivedData),
# shared with Xcode and its package cache: nothing in the repository.
cd "$(dirname "$0")/.."
scripts/spec.sh
udid=$(scripts/simulator.sh)

xcodebuild test \
  -project Organizer.xcodeproj \
  -scheme Organizer \
  -configuration Debug \
  -destination "id=$udid" \
  -skipPackagePluginValidation \
  -quiet
