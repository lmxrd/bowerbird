#!/bin/sh
# The end-to-end smoke tests (packages/e2e) against the built image: a fresh
# container with an empty data volume, on 127.0.0.1:$E2E_PORT (default
# 8090), removed afterwards. `mise run test:e2e` builds the image first.
# CONTAINER picks the engine (podman locally, docker in CI); IMAGE the image.
set -eu
cd "$(dirname "$0")/.."
engine=${CONTAINER:-podman}
image=${IMAGE:-organizer}
port=${E2E_PORT:-8090}
name=organizer-e2e-$$

cleanup() {
  $engine rm -f "$name" >/dev/null 2>&1 || true
  $engine volume rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

$engine run -d --name "$name" -p "127.0.0.1:$port:8000" \
  -v "$name:/data" "$image" >/dev/null
i=0
until curl -fs "http://127.0.0.1:$port/api/health" >/dev/null; do
  i=$((i + 1))
  if [ "$i" -ge 60 ]; then
    $engine logs "$name"
    echo "the image did not answer its health check" >&2
    exit 1
  fi
  sleep 1
done

E2E_BASE_URL="http://127.0.0.1:$port" pnpm --filter organizer-e2e exec playwright test "$@"
