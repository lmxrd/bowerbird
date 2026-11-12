#!/usr/bin/env bash
# Prints the UDID of an available iPhone simulator on the newest iOS
# runtime (18 or later), for xcodebuild's -destination. SIMULATOR=<udid>
# picks one instead.
set -euo pipefail
if [[ -n ${SIMULATOR:-} ]]; then
  echo "$SIMULATOR"
  exit 0
fi
udid=$(xcrun simctl list devices available --json | jq -r '
  .devices
  | to_entries
  | map(select(.key | test("SimRuntime\\.iOS-[0-9]+")))
  | map(.version = (.key | capture("iOS-(?<major>[0-9]+)-(?<minor>[0-9]+)") | [(.major | tonumber), (.minor | tonumber)]))
  | map(select(.version[0] >= 18))
  | sort_by(.version) | reverse
  | map(.value[] | select(.name | startswith("iPhone")) | .udid)
  | first // empty')
if [[ -z $udid ]]; then
  echo "No iPhone simulator with iOS 18 or later: add one in Xcode (Settings, Components)" >&2
  exit 1
fi
echo "$udid"
