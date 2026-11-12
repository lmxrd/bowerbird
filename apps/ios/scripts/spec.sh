#!/usr/bin/env bash
# Copies the OpenAPI spec (`mise run gen:spec`) to where the generator's
# build plugin reads it: next to its config in the OrganizerKit target. The
# copy is ignored by git, as the spec is.
#
# One change on the way: swift-openapi-generator skips a `null` schema, so
# a nullable reference (`oneOf: [{$ref}, {type: null}]`, as utoipa writes an
# `Option<T>` of a schema) would lose its property entirely (an item's
# proposal, for one). Such a property becomes the reference itself, and
# optional (out of `required`), which is what Swift makes of null anyway.
set -euo pipefail
cd "$(dirname "$0")/.."

spec=../backend/dist/openapi.json
target=Sources/OrganizerKit/OrganizerKit/openapi.json
if [[ ! -f $spec ]]; then
  echo "No $spec: run \`mise run gen:spec\` first" >&2
  exit 1
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
jq '
  def nullable_one_of:
    type == "object" and (.oneOf | type == "array") and (.oneOf | length == 2)
    and any(.oneOf[]; . == {"type": "null"});
  def unwrap:
    (.oneOf | map(select(. != {"type": "null"})) | .[0])
    + (if .description then {description} else {} end);
  # First the required lists (while the properties still say they are
  # nullable), then the schemas themselves.
  walk(
    if type == "object" and (.properties | type == "object") and (.required | type == "array") then
      [.properties | to_entries[] | select(.value | nullable_one_of) | .key] as $nullable
      | .required -= $nullable
    else . end
  )
  | walk(if nullable_one_of then unwrap else . end)
' "$spec" >"$tmp"

# Only when it changed, so that the plugin does not run again for nothing.
if ! cmp -s "$tmp" "$target"; then
  cp "$tmp" "$target"
fi
