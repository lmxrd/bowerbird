#!/bin/sh
# Federation with Nextcloud (docs/M7E_DESIGN.md §9.4): a manual check, never
# in CI. It starts a Nextcloud container (podman, the exact tag below) and an
# Organizer server on this machine, turns OCM on in both, and shares both
# ways: a folder from Organizer to Nextcloud's admin, then one from Nextcloud
# to an Organizer user, each accepted and read on the other side. Both are
# removed afterwards. `mise run interop:nextcloud` builds the frontend first.
#
# Nextcloud reaches this machine at host.containers.internal (podman's name
# for the host), Organizer reaches Nextcloud at 127.0.0.1:$NC_PORT; both
# take each other's http and private addresses (FEDERATION_ALLOW_PRIVATE,
# Nextcloud's allow_local_remote_servers). Which OCM features meet depends
# on the Nextcloud version: Organizer requires RFC 9421 signatures and the
# code flow (must-use-http-sig, must-exchange-token), and says so when a
# share is refused. Needs podman, curl, node and sqlite3.
set -eu
cd "$(dirname "$0")/.."

NEXTCLOUD_IMAGE=${NEXTCLOUD_IMAGE:-docker.io/library/nextcloud:35.0.2-apache}
NC_PORT=${NC_PORT:-8091}
ORG_PORT=${ORG_PORT:-8123}
name=organizer-interop-nc-$$
data=$(mktemp -d)
org_pid=

cleanup() {
  podman rm -f "$name" >/dev/null 2>&1 || true
  if [ -n "$org_pid" ]; then kill "$org_pid" 2>/dev/null || true; fi
  rm -rf "$data"
}
trap cleanup EXIT

nc_url="http://127.0.0.1:$NC_PORT"
nc_host="127.0.0.1:$NC_PORT"
org_public="http://host.containers.internal:$ORG_PORT"
org_url="http://127.0.0.1:$ORG_PORT"
failed=0

say() { printf '\n== %s\n' "$*"; }
fail() {
  printf 'FAIL: %s\n' "$*" >&2
  failed=1
}
# A field of a JSON document on stdin (a dotted path; numbers index arrays).
field() {
  node -e '
    let s = ""; process.stdin.on("data", d => s += d).on("end", () => {
      let v; try { v = JSON.parse(s); } catch { process.exit(1); }
      for (const k of process.argv[1].split(".")) { if (v == null) break; v = v[k]; }
      if (v === undefined || v === null) process.exit(1);
      console.log(typeof v === "object" ? JSON.stringify(v) : v);
    });' "$1"
}
wait_for() {
  i=0
  until curl -fs "$1" >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -ge "${2:-120}" ]; then
      echo "$1 did not answer" >&2
      return 1
    fi
    sleep 1
  done
}

say "Nextcloud ($NEXTCLOUD_IMAGE) on $nc_url"
podman run -d --name "$name" -p "127.0.0.1:$NC_PORT:80" \
  -e SQLITE_DATABASE=nextcloud \
  -e NEXTCLOUD_ADMIN_USER=admin -e NEXTCLOUD_ADMIN_PASSWORD=interop-admin-pass \
  -e NEXTCLOUD_TRUSTED_DOMAINS="127.0.0.1 127.0.0.1:$NC_PORT localhost" \
  -e OVERWRITEHOST="$nc_host" -e OVERWRITEPROTOCOL=http \
  "$NEXTCLOUD_IMAGE" >/dev/null
wait_for "$nc_url/status.php" 300
i=0
until curl -fs "$nc_url/status.php" | grep -q '"installed":true'; do
  i=$((i + 1))
  if [ "$i" -ge 300 ]; then
    podman logs "$name" | tail -40
    echo "Nextcloud did not finish installing" >&2
    exit 1
  fi
  sleep 1
done
occ() { podman exec -u www-data "$name" php occ "$@"; }
occ config:system:set allow_local_remote_servers --value true --type boolean >/dev/null
occ config:system:set sharing.federation.allowHttpFallback --value true --type boolean >/dev/null
occ app:enable federation >/dev/null 2>&1 || true
occ app:enable federatedfilesharing >/dev/null 2>&1 || true
occ config:app:set files_sharing outgoing_server2server_share_enabled --value yes >/dev/null
occ config:app:set files_sharing incoming_server2server_share_enabled --value yes >/dev/null
nc_auth="admin:interop-admin-pass"
ocs() {
  curl -s -u "$nc_auth" -H "OCS-APIRequest: true" -H "Accept: application/json" "$@"
}

say "Organizer on $org_url (public as $org_public)"
DATA_DIR="$data" DATABASE_URL="sqlite:$data/organizer.db" GEOIP_URL= PORT="$ORG_PORT" \
  PUBLIC_URL="$org_public" FEDERATION_ALLOW_PRIVATE=true RUST_LOG=info \
  cargo run --quiet -p organizer >"$data/organizer.log" 2>&1 &
org_pid=$!
wait_for "$org_url/api/health" 600
jar="$data/cookies"
curl -fs -c "$jar" -H 'content-type: application/json' \
  -d '{"email":"alice@example.com","password":"correct horse battery"}' \
  "$org_url/api/auth/setup" >/dev/null
# Federation open, any address allowed (no invite: Nextcloud's invites are
# an app of its own). Straight in the database: the superadmin's endpoints
# want a second factor confirmed, which this script has none of.
sqlite3 "$data/organizer.db" "
  UPDATE federation_settings SET mode = 'open';
  INSERT INTO settings (level, level_id, key, value, mode)
    VALUES ('superadmin', NULL, 'federation.share_with_any', 'true', 'default');"
org() { curl -s -b "$jar" -H 'content-type: application/json' "$@"; }

say "Discovery both ways"
curl -fs "$nc_url/.well-known/ocm" | field apiVersion | sed 's/^/Nextcloud OCM apiVersion /' ||
  fail "Nextcloud answers no OCM discovery"
podman exec "$name" curl -fs "$org_public/.well-known/ocm" | field capabilities ||
  fail "Nextcloud cannot reach Organizer's discovery at $org_public"

say "Organizer → Nextcloud: a folder for admin@$nc_host"
root=$(org "$org_url/api/files/root" | field id)
docs=$(org -d '{"name":"From Organizer"}' "$org_url/api/files/$root/folders" | field id)
printf 'hello from organizer' >"$data/hello.txt"
curl -s -b "$jar" -F "file=@$data/hello.txt;filename=hello.txt" \
  "$org_url/api/files/$docs/files" >/dev/null
answer=$(org -d "{\"address\":\"admin@$nc_host\",\"role\":\"editor\"}" "$org_url/api/files/$docs/access")
if echo "$answer" | field remote.address >/dev/null; then
  echo "shared: $(echo "$answer" | field remote.state)"
  pending=$(ocs "$nc_url/ocs/v2.php/apps/files_sharing/api/v1/remote_shares/pending" | field ocs.data.0.id || true)
  if [ -n "$pending" ]; then
    ocs -X POST "$nc_url/ocs/v2.php/apps/files_sharing/api/v1/remote_shares/pending/$pending" >/dev/null
  fi
  sleep 3
  if curl -s -u "$nc_auth" -X PROPFIND -H 'Depth: 1' \
    "$nc_url/remote.php/dav/files/admin/From%20Organizer/" | grep -q 'hello.txt'; then
    echo "Nextcloud lists hello.txt through Organizer's WebDAV"
    got=$(curl -s -u "$nc_auth" "$nc_url/remote.php/dav/files/admin/From%20Organizer/hello.txt")
    [ "$got" = "hello from organizer" ] || fail "Nextcloud read '$got'"
  else
    fail "Nextcloud does not list the shared folder's file"
  fi
else
  fail "Organizer could not share with Nextcloud: $answer"
fi

say "Nextcloud → Organizer: a folder for alice@example.com@host.containers.internal:$ORG_PORT"
curl -s -u "$nc_auth" -X MKCOL "$nc_url/remote.php/dav/files/admin/From%20Nextcloud" >/dev/null
curl -s -u "$nc_auth" -T - "$nc_url/remote.php/dav/files/admin/From%20Nextcloud/hi.txt" \
  <<EOF >/dev/null
hello from nextcloud
EOF
answer=$(ocs -X POST \
  --data-urlencode "path=/From Nextcloud" --data-urlencode "shareType=6" \
  --data-urlencode "shareWith=alice@example.com@host.containers.internal:$ORG_PORT" \
  --data-urlencode "permissions=1" \
  "$nc_url/ocs/v2.php/apps/files_sharing/api/v1/shares")
echo "$answer" | field ocs.meta.statuscode | sed 's/^/Nextcloud says /' || true
shared=$(org "$org_url/api/shared")
top=$(echo "$shared" | field 0.node.id || true)
if [ -n "$top" ]; then
  echo "in Shared with me, from $(echo "$shared" | field 0.remote.server)"
  org -X POST "$org_url/api/shared/$top/accept" >/dev/null
  file=$(org "$org_url/api/files/$top/children" | field items.0.id || true)
  if [ -n "$file" ]; then
    got=$(curl -s -b "$jar" "$org_url/api/files/$file/content")
    case "$got" in
    "hello from nextcloud"*) echo "Organizer read hi.txt through Nextcloud's WebDAV" ;;
    *) fail "Organizer read '$got'" ;;
    esac
  else
    fail "Organizer lists nothing in the share (see $data/organizer.log)"
    tail -20 "$data/organizer.log"
  fi
else
  fail "the share from Nextcloud did not arrive (Organizer refuses unsigned shares and those without must-exchange-token)"
  tail -20 "$data/organizer.log"
fi

if [ "$failed" -ne 0 ]; then
  echo
  echo "interop: some checks failed" >&2
  exit 1
fi
echo
echo "interop: both ways work"
