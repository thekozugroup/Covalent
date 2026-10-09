#!/bin/sh
# Opt-in, isolated Mac <-> Atmos one-way folder-link drill.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ssh_host=Atmos
revision=''
release_version=''
mac_app=''
mac_build_receipt=''
node_bin=''
run=false
prior_cleanup_confirmed=false

usage() {
  cat <<'EOF'
Usage: scripts/test-remote-drill.sh --source-revision COMMIT --release-version VERSION --mac-app /path/to/Covalent.app --mac-build-receipt /path/to/build-receipt.json [--ssh HOST] --prior-cleanup-confirmed --execute
Without --execute this makes no local or remote changes.
Before --prior-cleanup-confirmed, reconcile the exact resources recorded in
artifacts/validation-2026-09-12/docker-rust-cache-checkpoint48/cleanup-ledger.json.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --source-revision) revision=$2; shift 2 ;;
    --release-version) release_version=$2; shift 2 ;;
    --mac-app) mac_app=$2; shift 2 ;;
    --mac-build-receipt) mac_build_receipt=$2; shift 2 ;;
    --ssh) ssh_host=$2; shift 2 ;;
    --prior-cleanup-confirmed) prior_cleanup_confirmed=true; shift ;;
    --execute) run=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done
[ "$run" = true ] || { usage; exit 0; }
case "$ssh_host" in ''|-*|*[!A-Za-z0-9_.@-]*) echo "unsafe SSH host" >&2; exit 64 ;; esac
[ -n "$revision" ] || { echo "--source-revision is required" >&2; exit 64; }
[ -n "$release_version" ] || { echo "--release-version is required" >&2; exit 64; }
case "$release_version" in *[!0-9A-Za-z._+-]*|'') echo "unsafe release version" >&2; exit 64 ;; esac
[ -n "$mac_app" ] || { echo "--mac-app is required" >&2; exit 64; }
[ -n "$mac_build_receipt" ] || { echo "--mac-build-receipt is required" >&2; exit 64; }
[ "$prior_cleanup_confirmed" = true ] || { echo "--prior-cleanup-confirmed is required before remote work" >&2; exit 64; }
for x in git ssh scp python3 openssl shasum netstat gzip ditto codesign; do command -v "$x" >/dev/null || { echo "$x is required" >&2; exit 1; }; done
"$root/scripts/verify-apple-silicon-bundle.sh" "$mac_app"
packaged_node="$mac_app/Contents/MacOS/covalent-node"
[ -x "$packaged_node" ] && [ ! -L "$packaged_node" ] || { echo "verified Mac app has no packaged node helper" >&2; exit 1; }
app_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$mac_app/Contents/Info.plist")
[ "$app_version" = "$release_version" ] || {
  echo "Mac app version $app_version does not match requested release $release_version" >&2; exit 1
}
app_executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$mac_app/Contents/Info.plist")
app_binary="$mac_app/Contents/MacOS/$app_executable"
commit=$(git -C "$root" rev-parse --verify "$revision^{commit}") || exit 1
[ "$(git -C "$root" rev-parse HEAD)" = "$commit" ] || { echo "source revision is not the checked-out HEAD" >&2; exit 1; }
git -C "$root" diff --quiet && git -C "$root" diff --cached --quiet || {
  echo "release source checkout has tracked changes" >&2; exit 1
}
if [ -n "$(git -C "$root" ls-files --others --exclude-standard)" ]; then
  echo "release source checkout has untracked files" >&2; exit 1
fi
source_fingerprint=$(python3 - "$root" <<'PYFINGERPRINT'
import hashlib
import pathlib
import subprocess
import sys

root = pathlib.Path(sys.argv[1])
manifest = subprocess.check_output([str(root / "scripts/docker-source-fingerprint.sh"), str(root)])
print(hashlib.sha256(manifest).hexdigest())
PYFINGERPRINT
)
case "$source_fingerprint" in *[!0-9a-f]*|'') echo "invalid Docker source fingerprint" >&2; exit 1 ;; esac
[ "${#source_fingerprint}" -eq 64 ] || { echo "invalid Docker source fingerprint" >&2; exit 1; }

# The receipt is produced with the personal app and binds the exact packaged
# bytes to the clean source snapshot. Validate it before the first SSH or Docker
# operation. This remains a test-only engine harness; it does not stand in for
# the production app lifecycle.
[ -f "$mac_build_receipt" ] && [ ! -L "$mac_build_receipt" ] || { echo "Mac build receipt is missing or unsafe" >&2; exit 1; }
receipt_bytes=$(wc -c < "$mac_build_receipt" | tr -d '[:space:]')
[ "$receipt_bytes" -gt 0 ] && [ "$receipt_bytes" -le 65536 ] || { echo "Mac build receipt is outside its size bound" >&2; exit 1; }
python3 - "$mac_build_receipt" "$commit" "$source_fingerprint" "$release_version" \
  "$app_binary" "$packaged_node" \
  "$mac_app/Contents/MacOS/covalent-rclone" \
  "$mac_app/Contents/MacOS/covalent-engine-guardian" \
  "$mac_app/Contents/Resources/CovalentSyncEngine/manifest.json" <<'PYRECEIPT'
import hashlib
import json
import pathlib
import sys

receipt_path, commit, fingerprint, version, app, node, worker, guardian, manifest = sys.argv[1:]

def descriptor(path):
    payload = pathlib.Path(path).read_bytes()
    return {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}

try:
    receipt = json.loads(pathlib.Path(receipt_path).read_bytes())
except (OSError, UnicodeDecodeError, json.JSONDecodeError):
    raise SystemExit("Mac build receipt is unreadable") from None
if set(receipt) != {"schemaVersion", "sourceCommit", "sourceFingerprint", "releaseVersion", "archive", "components"}:
    raise SystemExit("Mac build receipt has an unsupported schema")
if receipt["schemaVersion"] != 1 or receipt["sourceCommit"] != commit or receipt["sourceFingerprint"] != fingerprint or receipt["releaseVersion"] != version:
    raise SystemExit("Mac build receipt does not bind the requested source")
archive = receipt["archive"]
if (not isinstance(archive, dict) or set(archive) != {"bytes", "sha256"}
        or isinstance(archive["bytes"], bool) or not isinstance(archive["bytes"], int)
        or archive["bytes"] <= 0 or not isinstance(archive["sha256"], str)
        or len(archive["sha256"]) != 64
        or any(character not in "0123456789abcdef" for character in archive["sha256"])):
    raise SystemExit("Mac build receipt has an invalid archive descriptor")
components = receipt["components"]
expected_names = {"appExecutable", "covalent-node", "covalent-rclone", "covalent-engine-guardian", "engineManifest"}
if not isinstance(components, dict) or set(components) != expected_names:
    raise SystemExit("Mac build receipt has an invalid component set")
actual = {
    "appExecutable": descriptor(app),
    "covalent-node": descriptor(node),
    "covalent-rclone": descriptor(worker),
    "covalent-engine-guardian": descriptor(guardian),
    "engineManifest": descriptor(manifest),
}
if components != actual:
    raise SystemExit("Mac bundle bytes do not match the source-bound build receipt")
PYRECEIPT
printf '%s\n' "remote drill source commit: $commit"
printf '%s\n' "remote drill Docker source fingerprint: $source_fingerprint"
printf '%s\n' "remote drill Mac app version: $app_version"
printf '%s\n' "remote drill Mac build receipt SHA-256: $(shasum -a 256 "$mac_build_receipt" | awk '{print $1}')"
printf '%s\n' "remote drill Mac app executable SHA-256: $(shasum -a 256 "$app_binary" | awk '{print $1}')"
printf '%s\n' "remote drill Mac node SHA-256: $(shasum -a 256 "$packaged_node" | awk '{print $1}')"
printf '%s\n' "remote drill Mac rclone SHA-256: $(shasum -a 256 "$mac_app/Contents/MacOS/covalent-rclone" | awk '{print $1}')"
printf '%s\n' "remote drill Mac guardian SHA-256: $(shasum -a 256 "$mac_app/Contents/MacOS/covalent-engine-guardian" | awk '{print $1}')"
opts='-o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=10'
local_root='' remote_root='' nonce='' builder='' image='' container=''
node_pid='' node_identity='' tunnel_pid='' tunnel_identity=''
child_shutdown_checks=100
preexisting_containers=''
preexisting_images=''

valid_remote_root() {
  case "$1" in
    /tmp/covalent-remote-drill.[A-Za-z0-9]*) ;;
    *) return 1 ;;
  esac
  suffix=${1#/tmp/covalent-remote-drill.}
  case "$suffix" in ''|*[!A-Za-z0-9]*) return 1 ;; esac
}

assert_preexisting_ids_survive() {
  [ -n "$preexisting_containers" ] || return 0
  after_containers=$(ssh $opts "$ssh_host" 'docker ps -aq | sort -u') || return 1
  after_images=$(ssh $opts "$ssh_host" 'docker image ls -aq | sort -u') || return 1
  while IFS= read -r identifier; do
    [ -z "$identifier" ] || printf '%s\n' "$after_containers" | grep -Fqx -- "$identifier" || {
      echo "a pre-existing Atmos container disappeared during the drill; refusing a clean result" >&2
      return 1
    }
  done < "$preexisting_containers"
  while IFS= read -r identifier; do
    [ -z "$identifier" ] || printf '%s\n' "$after_images" | grep -Fqx -- "$identifier" || {
      echo "a pre-existing Atmos image disappeared during the drill; refusing a clean result" >&2
      return 1
    }
  done < "$preexisting_images"
}

remote_cleanup() {
  [ -n "$remote_root" ] || return 0
  if ! valid_remote_root "$remote_root"; then
    echo "remote drill cleanup refused unsafe temporary path: $remote_root" >&2
    return 1
  fi
  if ! ssh $opts "$ssh_host" sh -s -- "$remote_root" "$nonce" "$builder" "$image" "$container" <<'SH'
set -u
root=$1 nonce=$2 builder=$3 image=$4 container=$5
case "$root" in /tmp/covalent-remote-drill.[A-Za-z0-9]*) ;; *) echo "unsafe remote drill path" >&2; exit 2 ;; esac
suffix=${root#/tmp/covalent-remote-drill.}
case "$suffix" in ''|*[!A-Za-z0-9]*) echo "unsafe remote drill suffix" >&2; exit 2 ;; esac
case "$nonce" in ''|*[!0-9a-f]*) echo "unsafe remote drill nonce" >&2; exit 2 ;; esac
[ "${#nonce}" -eq 32 ] && [ "$builder" = "covalent-remote-drill-$nonce" ] &&
  [ "$image" = "covalent-remote-drill:$nonce" ] && [ "$container" = "covalent-remote-drill-$nonce" ] || {
    echo "unsafe remote drill resource relationship" >&2; exit 2
  }
docker info >/dev/null 2>&1 || { echo "cannot inspect Docker while cleaning the drill" >&2; exit 1; }
cleanup_status=0
buildkit_container="buildx_buildkit_${builder}0"
container_owned=false
container_inventory_ok=true
container_ids=$(docker ps -aq --no-trunc --filter "name=^/${container}$") || {
  echo "could not inventory the drill container during cleanup" >&2
  cleanup_status=1; container_inventory_ok=false; container_ids=''
}
if [ "$container_inventory_ok" = true ] && [ -n "$container_ids" ]; then
  container_label=$(docker container inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$container") || {
    echo "could not inspect the drill container during cleanup" >&2
    cleanup_status=1; container_inventory_ok=false; container_label=''
  }
  if [ "$container_inventory_ok" = true ] && [ "$(cat "$root/container-owned" 2>/dev/null || true)" = "$nonce" ] && [ "$container_label" = "$nonce" ]; then
    container_owned=true
    docker rm -f "$container" >/dev/null 2>&1 || { echo "could not remove drill container: $container" >&2; cleanup_status=1; }
  elif [ "$container_inventory_ok" = true ]; then
    echo "selected container lacks drill ownership proof and was retained: $container" >&2; cleanup_status=1
  fi
fi
if [ "$container_owned" = true ]; then
  remaining=$(docker ps -aq --no-trunc --filter "name=^/${container}$") || {
    echo "could not verify drill container removal" >&2; cleanup_status=1; remaining=unknown
  }
  [ -z "$remaining" ] || { echo "drill container remains: $container" >&2; cleanup_status=1; }
fi
builder_owned=false
[ "$(cat "$root/builder-owned" 2>/dev/null || true)" = "$nonce" ] && builder_owned=true
builder_inventory_ok=true
builder_names=$(docker buildx ls --format '{{.Name}}') || {
  echo "could not inventory BuildKit builders during cleanup" >&2
  cleanup_status=1; builder_inventory_ok=false; builder_names=''
}
builder_present=false
for name in $builder_names; do [ "$name" = "$builder" ] && builder_present=true; done
if [ "$builder_inventory_ok" = true ] && [ "$builder_owned" = true ] && [ "$builder_present" = true ]; then
  docker buildx rm -f "$builder" >/dev/null 2>&1 || { echo "could not remove drill builder: $builder" >&2; cleanup_status=1; }
elif [ "$builder_inventory_ok" = true ] && [ "$builder_owned" != true ] && [ "$builder_present" = true ]; then
  echo "selected builder lacks drill ownership proof and was retained: $builder" >&2; cleanup_status=1
fi
if [ "$builder_owned" = true ]; then
  builder_names=$(docker buildx ls --format '{{.Name}}') || { echo "could not verify drill builder removal" >&2; cleanup_status=1; builder_names=$builder; }
  for name in $builder_names; do [ "$name" != "$builder" ] || { echo "drill builder remains: $builder" >&2; cleanup_status=1; }; done
  buildkit_ids=$(docker ps -aq --no-trunc --filter "name=^/${buildkit_container}$") || { echo "could not verify drill BuildKit container removal" >&2; cleanup_status=1; buildkit_ids=unknown; }
  [ -z "$buildkit_ids" ] || { echo "drill BuildKit container remains for builder: $builder" >&2; cleanup_status=1; }
  buildkit_volumes=$(docker volume ls -q --filter "name=^${buildkit_container}_state$") || { echo "could not verify drill BuildKit volume removal" >&2; cleanup_status=1; buildkit_volumes=unknown; }
  [ -z "$buildkit_volumes" ] || { echo "drill BuildKit volume remains: ${buildkit_container}_state" >&2; cleanup_status=1; }
  echo "remote drill: pre-existing pinned BuildKit image retained"
fi
image_owned=false
image_inventory_ok=true
image_ids=$(docker image ls -q --no-trunc "$image") || {
  echo "could not inventory the drill image during cleanup" >&2
  cleanup_status=1; image_inventory_ok=false; image_ids=''
}
if [ "$image_inventory_ok" = true ] && [ -n "$image_ids" ]; then
  image_label=$(docker image inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$image") || {
    echo "could not inspect the drill image during cleanup" >&2
    cleanup_status=1; image_inventory_ok=false; image_label=''
  }
  if [ "$image_inventory_ok" = true ] && [ "$(cat "$root/image-owned" 2>/dev/null || true)" = "$nonce" ] && [ "$image_label" = "$nonce" ]; then
    image_owned=true
    docker image rm "$image" >/dev/null 2>&1 || { echo "could not remove drill image: $image" >&2; cleanup_status=1; }
  elif [ "$image_inventory_ok" = true ]; then
    echo "selected image tag lacks drill ownership proof and was retained: $image" >&2; cleanup_status=1
  fi
fi
if [ "$image_owned" = true ]; then
  remaining=$(docker image ls -q --no-trunc "$image") || { echo "could not verify drill image removal" >&2; cleanup_status=1; remaining=unknown; }
  [ -z "$remaining" ] || { echo "drill image tag remains: $image" >&2; cleanup_status=1; }
fi
if [ "$cleanup_status" -eq 0 ]; then
  if [ -e "$root" ]; then
    rm -rf -- "$root" || { echo "could not remove drill temporary path: $root" >&2; cleanup_status=1; }
  fi
  if [ -e "$root" ]; then
    echo "drill temporary path remains: $root" >&2; cleanup_status=1
  fi
else
  echo "remote cleanup failed; retaining drill root for ownership diagnosis: $root" >&2
fi
exit "$cleanup_status"
SH
  then
    echo "remote drill cleanup failed; possible residue: path=$remote_root container=$container builder=$builder image=$image" >&2
    return 1
  fi
}

child_process_identity() {
  child_pid=$1
  child_ppid=$(ps -o ppid= -p "$child_pid" 2>/dev/null | tr -d '[:space:]')
  child_started=$(ps -o lstart= -p "$child_pid" 2>/dev/null | sed 's/^ *//;s/ *$//')
  [ "$child_ppid" = "$$" ] && [ -n "$child_started" ] || return 1
  printf '%s:%s\n' "$child_ppid" "$child_started"
}

uncaptured_child_is_running() {
  child_pid=$1
  kill -0 "$child_pid" >/dev/null 2>&1 || return 1
  child_state=$(ps -o stat= -p "$child_pid" 2>/dev/null | sed 's/^ *//')
  case "$child_state" in Z*) return 1 ;; *) return 0 ;; esac
}

stop_uncaptured_child() {
  child_pid=$1 child_name=$2
  kill -TERM "$child_pid" >/dev/null 2>&1 || true
  checks=0
  while uncaptured_child_is_running "$child_pid" && [ "$checks" -lt "$child_shutdown_checks" ]; do
    sleep 0.1
    checks=$((checks + 1))
  done
  if uncaptured_child_is_running "$child_pid"; then
    if ! kill -KILL "$child_pid" >/dev/null 2>&1; then
      echo "$child_name identity capture and bounded KILL both failed pid=$child_pid" >&2
      return 1
    fi
    checks=0
    while uncaptured_child_is_running "$child_pid" && [ "$checks" -lt 20 ]; do
      sleep 0.1
      checks=$((checks + 1))
    done
    if uncaptured_child_is_running "$child_pid"; then
      echo "$child_name remained alive after bounded identity-capture cleanup pid=$child_pid" >&2
      return 1
    fi
  fi
  wait "$child_pid" >/dev/null 2>&1 || true
  echo "$child_name identity capture failed; exact newly spawned child was reaped" >&2
  return 1
}

owned_child_is_running() {
  child_pid=$1 expected_identity=$2
  [ "$(child_process_identity "$child_pid" 2>/dev/null || true)" = "$expected_identity" ] || return 1
  child_state=$(ps -o stat= -p "$child_pid" 2>/dev/null | sed 's/^ *//')
  case "$child_state" in ''|Z*) return 1 ;; *) return 0 ;; esac
}

stop_owned_child() {
  child_pid=$1 expected_identity=$2 child_name=$3 require_clean=$4
  [ -n "$child_pid" ] || return 0
  if [ "$(child_process_identity "$child_pid" 2>/dev/null || true)" != "$expected_identity" ]; then
    echo "$child_name identity no longer matches captured child pid=$child_pid" >&2
    return 1
  fi
  was_running=false
  if owned_child_is_running "$child_pid" "$expected_identity"; then
    was_running=true
    kill -TERM "$child_pid" >/dev/null 2>&1 || true
  fi
  checks=0
  while owned_child_is_running "$child_pid" "$expected_identity" && [ "$checks" -lt "$child_shutdown_checks" ]; do
    sleep 0.1
    checks=$((checks + 1))
  done
  forced=false
  if owned_child_is_running "$child_pid" "$expected_identity"; then
    forced=true
    if ! kill -KILL "$child_pid" >/dev/null 2>&1; then
      echo "$child_name could not KILL captured child pid=$child_pid" >&2
      return 1
    fi
    checks=0
    while owned_child_is_running "$child_pid" "$expected_identity" && [ "$checks" -lt 20 ]; do
      sleep 0.1
      checks=$((checks + 1))
    done
    if owned_child_is_running "$child_pid" "$expected_identity"; then
      echo "$child_name remained alive after KILL pid=$child_pid" >&2
      return 1
    fi
  fi
  wait_status=0
  wait "$child_pid" || wait_status=$?
  if [ "$forced" = true ]; then
    echo "$child_name ignored TERM and required KILL (pid=$child_pid)" >&2
    return 1
  fi
  if [ "$require_clean" = true ] && { [ "$was_running" != true ] || [ "$wait_status" -ne 0 ]; }; then
    echo "$child_name did not stop cleanly (pid=$child_pid status=$wait_status)" >&2
    return 1
  fi
}

stop_local_node() {
  require_clean=${1:-false}
  [ -n "$node_pid" ] || return 0
  owned_pid=$node_pid
  owned_identity=$node_identity
  stop_status=0
  stop_owned_child "$owned_pid" "$owned_identity" 'local drill node' "$require_clean" || stop_status=$?
  if [ "$stop_status" -eq 0 ] || ! kill -0 "$owned_pid" >/dev/null 2>&1; then
    node_pid=''
    node_identity=''
  fi
  return "$stop_status"
}

stop_tunnel() {
  [ -n "$tunnel_pid" ] || return 0
  owned_pid=$tunnel_pid
  owned_identity=$tunnel_identity
  stop_status=0
  stop_owned_child "$owned_pid" "$owned_identity" 'SSH management tunnel' false || stop_status=$?
  if [ "$stop_status" -eq 0 ] || ! kill -0 "$owned_pid" >/dev/null 2>&1; then
    tunnel_pid=''
    tunnel_identity=''
  fi
  return "$stop_status"
}

start_local_serve() {
  local_log="$local_root/node.log"
  COVALENT_SYNC_RUNTIME_DIR="$runtime_dir" "$node_bin" serve \
    --listen "127.0.0.1:$local_api" --peer-listen "$local_ip:$local_peer" \
    --advertised-peer-address "$local_ip:$local_peer" --data-dir "$state_dir" --device-name 'Mac remote drill' \
    --key-encryption-key-file "$local_root/local-kek" --api-token-file "$local_token" > "$local_log" 2>&1 &
  node_pid=$!
  if ! node_identity=$(child_process_identity "$node_pid"); then
    stop_uncaptured_child "$node_pid" 'local drill node' || true
    kill -0 "$node_pid" >/dev/null 2>&1 || node_pid=''
    return 1
  fi
  for n in $(seq 1 90); do
    "$node_bin" healthcheck --url "http://127.0.0.1:$local_api/healthz" >/dev/null 2>&1 && return 0
    kill -0 "$node_pid" >/dev/null 2>&1 || break
    sleep 1
  done
  sed -n '1,120p' "$local_log" >&2
  echo "local drill node did not become ready" >&2
  return 1
}

cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  cleanup_status=0
  stop_tunnel || cleanup_status=1
  stop_local_node false || cleanup_status=1
  remote_cleanup || cleanup_status=1
  assert_preexisting_ids_survive || cleanup_status=1
  if [ -n "$node_pid" ] || [ -n "$tunnel_pid" ]; then
    echo "local drill child remains; retaining private fixture for safe diagnosis: $local_root" >&2
    cleanup_status=1
  elif [ "$cleanup_status" -eq 0 ]; then
    [ -z "$local_root" ] || rm -rf -- "$local_root"
  elif [ -n "$local_root" ]; then
    echo "drill cleanup was incomplete; retaining private fixture for safe diagnosis: $local_root" >&2
  fi
  [ "$status" -ne 0 ] || return "$cleanup_status"
  return "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

local_api=28085 local_peer=28086 remote_peer=28087 remote_api=28088 forward=28089
local_sync=8789 remote_sync=28090
tmp_base=$(printenv TMPDIR 2>/dev/null || true)
[ -n "$tmp_base" ] || tmp_base=/tmp
local_root=$(mktemp -d "$tmp_base/covalent-remote-drill.XXXXXX")
chmod 700 "$local_root"
local_root=$(CDPATH='' cd -- "$local_root" && pwd -P)

# Production helpers inherit the app sandbox. Copy and re-sign only this owned
# fixture, then update only the copied manifest's signed helper digests. The
# verified input app remains untouched.
runtime_contents="$local_root/mac-runtime/Covalent.app/Contents"
mkdir -p "$runtime_contents/MacOS" "$runtime_contents/Resources"
ditto "$packaged_node" "$runtime_contents/MacOS/covalent-node"
ditto "$mac_app/Contents/MacOS/covalent-rclone" "$runtime_contents/MacOS/covalent-rclone"
ditto "$mac_app/Contents/MacOS/covalent-engine-guardian" "$runtime_contents/MacOS/covalent-engine-guardian"
ditto "$mac_app/Contents/Resources/CovalentSyncEngine" "$runtime_contents/Resources/CovalentSyncEngine"
node_bin="$runtime_contents/MacOS/covalent-node"
worker_bin="$runtime_contents/MacOS/covalent-rclone"
guardian_bin="$runtime_contents/MacOS/covalent-engine-guardian"
chmod 755 "$node_bin" "$worker_bin" "$guardian_bin"
codesign --force --sign - --identifier life.michaelwong.covalent.remote-drill.node \
  --options runtime --timestamp=none "$node_bin"
codesign --force --sign - --identifier life.michaelwong.covalent.remote-drill.rclone \
  --options runtime --timestamp=none "$worker_bin"
codesign --force --sign - --identifier life.michaelwong.covalent.remote-drill.engine-guardian \
  --options runtime --timestamp=none "$guardian_bin"
for binary in "$node_bin" "$worker_bin" "$guardian_bin"; do
  codesign --verify --strict "$binary"
  fixture_entitlements=$(codesign -d --entitlements - "$binary" 2>/dev/null) || {
    echo "could not inspect test fixture helper entitlements" >&2; exit 1
  }
  [ -z "$fixture_entitlements" ] || { echo "test fixture helper retained entitlements" >&2; exit 1; }
done
guardian_sha=$(shasum -a 256 "$guardian_bin" | awk '{print $1}')
worker_sha=$(shasum -a 256 "$worker_bin" | awk '{print $1}')
python3 - "$runtime_contents/Resources/CovalentSyncEngine/manifest.json" "$guardian_sha" "$worker_sha" <<'PYMANIFESTREWRITE'
import copy
import json
import os
import sys

path, guardian_sha, worker_sha = sys.argv[1:]
if any(len(value) != 64 or any(character not in "0123456789abcdef" for character in value)
       for value in (guardian_sha, worker_sha)):
    raise SystemExit("invalid test-only signed executable digest")
with open(path, "r", encoding="utf-8") as source:
    original = json.load(source)
updated = copy.deepcopy(original)
executables = updated.get("executables")
if not isinstance(executables, dict):
    raise SystemExit("copied sync-engine manifest has no executable records")
for name, digest in (("covalent-engine-guardian", guardian_sha), ("covalent-rclone", worker_sha)):
    record = executables.get(name)
    if not isinstance(record, dict) or not isinstance(record.get("signedSha256"), str):
        raise SystemExit(f"copied sync-engine manifest has no signed hash for {name}")
    record["signedSha256"] = digest
restored = copy.deepcopy(updated)
for name in ("covalent-engine-guardian", "covalent-rclone"):
    if updated["executables"][name]["signedSha256"] == original["executables"][name]["signedSha256"]:
        raise SystemExit("test fixture retained a production helper signature")
    restored["executables"][name]["signedSha256"] = original["executables"][name]["signedSha256"]
if restored != original:
    raise SystemExit("test-only manifest rewrite changed production provenance")
temporary = f"{path}.{os.getpid()}.tmp"
descriptor_fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
try:
    with os.fdopen(descriptor_fd, "w", encoding="utf-8") as output:
        json.dump(updated, output, indent=2)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
finally:
    try:
        os.unlink(temporary)
    except FileNotFoundError:
        pass
PYMANIFESTREWRITE
printf '%s\n' "remote drill test-only Mac node SHA-256: $(shasum -a 256 "$node_bin" | awk '{print $1}')"
printf '%s\n' "remote drill test-only rclone SHA-256: $worker_sha"
printf '%s\n' "remote drill test-only guardian SHA-256: $guardian_sha"

archive="$local_root/source.tar.gz" manifest="$local_root/source-manifest.sha256"
git -C "$root" archive --format=tar "$commit" | gzip -n > "$archive"
ARCHIVE="$archive" MANIFEST="$manifest" python3 - <<'PYMANIFEST'
import hashlib, os, tarfile
with tarfile.open(os.environ["ARCHIVE"], "r:gz") as source, open(os.environ["MANIFEST"], "w") as output:
    members = sorted((member for member in source.getmembers() if member.isfile()), key=lambda item: item.name)
    for member in members:
        payload = source.extractfile(member)
        if payload is None:
            raise SystemExit("could not read archived source member")
        output.write(hashlib.sha256(payload.read()).hexdigest() + "  " + member.name + "\n")
PYMANIFEST
test -s "$archive" && test -s "$manifest"
printf '%s\n' "remote drill source manifest SHA-256: $(shasum -a 256 "$manifest" | awk '{print $1}')"
printf '%s\n' 'remote drill target: Ubuntu Atmos (separate from the Unraid hardware gate)'
printf '%s\n' 'remote drill limit: the private no-entitlement helper copies prove the packaged engine only, not the installable Mac app, Keychain, or security-scoped bookmark lifecycle'

# Read-only remote readiness and capacity. The builder is later independently
# capped to 1 CPU/4 GiB without swap; require 12 GiB available beforehand.
probe=$(ssh $opts "$ssh_host" sh -s <<'SH'
set -eu
command -v docker >/dev/null; command -v tailscale >/dev/null; command -v jq >/dev/null
status=$(tailscale status --json)
printf '%s\n' "$status" | jq -er '.BackendState == "Running"' >/dev/null
printf '%s\n' "$status" | jq -er '.Self.TailscaleIPs[]? | select(test("^[0-9]+(\\.[0-9]+){3}$"))' | head -1
awk '/MemAvailable:/ {print $2}' /proc/meminfo
printf '%s\n' "$SSH_CONNECTION"
SH
)
remote_ip=$(printf '%s\n' "$probe" | sed -n 1p)
available=$(printf '%s\n' "$probe" | sed -n 2p)
connection=$(printf '%s\n' "$probe" | sed -n 3p)
case "$remote_ip:$available" in [0-9]*.[0-9]*.[0-9]*.[0-9]*:[0-9]*) ;; *) echo "Atmos readiness probe failed" >&2; exit 1 ;; esac
[ "$available" -ge 12582912 ] || { echo "Atmos has under 12 GiB available RAM" >&2; exit 1; }
local_ip=$(printf '%s\n' "$connection" | awk '{print $1}')
case "$local_ip" in 100.*) ;; *) echo "SSH source is not Tailnet IPv4: $local_ip" >&2; exit 1 ;; esac
remote_listeners=$(ssh $opts "$ssh_host" "ss -H -lntu '( sport = :$remote_peer or sport = :$remote_api or sport = :$remote_sync )'") || {
  echo "could not inspect selected remote drill ports" >&2; exit 1
}
if [ -n "$remote_listeners" ]; then
  echo "selected remote drill port is occupied" >&2; exit 1
fi
local_tcp=$(netstat -an -p tcp 2>/dev/null) || { echo "could not inspect local TCP ports" >&2; exit 1; }
local_udp=$(netstat -an -p udp 2>/dev/null) || { echo "could not inspect local UDP ports" >&2; exit 1; }
if printf '%s\n' "$local_tcp" | grep -E "[.:]($local_api|$forward|$local_sync)[[:space:]]" >/dev/null ||
   printf '%s\n' "$local_udp" | grep -E "[.:]$local_peer[[:space:]]" >/dev/null; then
  echo "selected local drill port is occupied" >&2; exit 1
fi

nonce=$(openssl rand -hex 16)
case "$nonce" in ''|*[!0-9a-f]*) echo "could not generate a safe drill nonce" >&2; exit 1 ;; esac
[ "${#nonce}" -eq 32 ] || { echo "drill nonce has the wrong length" >&2; exit 1; }
builder=covalent-remote-drill-$nonce image=covalent-remote-drill:$nonce container=covalent-remote-drill-$nonce
remote_root=$(ssh $opts "$ssh_host" 'mktemp -d /tmp/covalent-remote-drill.XXXXXX')
valid_remote_root "$remote_root" || { echo "unsafe remote temporary path" >&2; exit 1; }
preexisting_containers="$local_root/preexisting-container-ids"
preexisting_images="$local_root/preexisting-image-ids"
ssh $opts "$ssh_host" 'docker ps -aq | sort -u' > "$preexisting_containers"
ssh $opts "$ssh_host" 'docker image ls -aq | sort -u' > "$preexisting_images"
ssh $opts "$ssh_host" "mkdir -p '$remote_root/source-tree' '$remote_root/config' '$remote_root/data' '$remote_root/secrets' '$remote_root/sync' '$remote_root/appdata-live' '$remote_root/source' '$remote_root/boot-source'"
cat "$archive" | ssh $opts "$ssh_host" "tar -xzf - -C '$remote_root/source-tree'"
scp -q $opts "$manifest" "$ssh_host:$remote_root/source-manifest.sha256"

# A dedicated docker-container BuildKit instance prevents this build from
# sharing the daemon builder/cache. Unsupported resource options must fail.
ssh $opts "$ssh_host" sh -s -- "$remote_root" "$nonce" "$builder" "$image" "$container" "$commit" "$release_version" "$source_fingerprint" <<'SH'
set -eu
root=$1 nonce=$2 builder=$3 image=$4 container=$5 commit=$6 version=$7 fingerprint=$8
case "$nonce" in ''|*[!0-9a-f]*) echo "unsafe remote drill nonce" >&2; exit 2 ;; esac
[ "${#nonce}" -eq 32 ] && [ "$builder" = "covalent-remote-drill-$nonce" ] &&
  [ "$image" = "covalent-remote-drill:$nonce" ] && [ "$container" = "covalent-remote-drill-$nonce" ] || {
    echo "unsafe remote drill resource relationship" >&2; exit 2
  }
! docker buildx inspect "$builder" >/dev/null 2>&1 || { echo "selected drill builder already exists" >&2; exit 1; }
! docker image inspect "$image" >/dev/null 2>&1 || { echo "selected drill image tag already exists" >&2; exit 1; }
! docker container inspect "$container" >/dev/null 2>&1 || { echo "selected drill container already exists" >&2; exit 1; }
docker image inspect moby/buildkit:v0.22.0 >/dev/null 2>&1 || {
  echo "pinned BuildKit image is not pre-existing; refusing to introduce an unowned shared image" >&2; exit 1
}
docker buildx create --name "$builder" --driver docker-container \
  --driver-opt image=moby/buildkit:v0.22.0 --driver-opt memory=4294967296 --driver-opt memory-swap=4294967296 \
  --driver-opt cpu-quota=100000 --driver-opt cpu-period=100000 --driver-opt cpuset-cpus=0 >/dev/null
printf '%s\n' "$nonce" > "$root/builder-owned"
docker buildx inspect --bootstrap "$builder" >/dev/null
buildkit=$(docker ps -aq --filter "name=buildx_buildkit_$builder")
[ -n "$buildkit" ]
test "$(docker inspect --format '{{.HostConfig.Memory}}' "$buildkit")" = 4294967296
test "$(docker inspect --format '{{.HostConfig.MemorySwap}}' "$buildkit")" = 4294967296
test "$(docker inspect --format '{{.HostConfig.CpuQuota}}' "$buildkit")" = 100000
test "$(docker inspect --format '{{.HostConfig.CpuPeriod}}' "$buildkit")" = 100000
test "$(docker inspect --format '{{.HostConfig.CpusetCpus}}' "$buildkit")" = 0
(cd "$root/source-tree" && sha256sum -c "$root/source-manifest.sha256")
docker buildx build --builder "$builder" --load --tag "$image" \
  --label "life.michaelwong.covalent.remote-drill=$nonce" \
  --build-arg "VCS_REF=$commit" --build-arg "RELEASE_VERSION=$version" \
  --build-arg "COVALENT_SOURCE_FINGERPRINT=$fingerprint" \
  --file "$root/source-tree/packaging/docker/Dockerfile" "$root/source-tree"
[ "$(docker image inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$image")" = "$nonce" ]
[ "$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image")" = "$commit" ]
[ "$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "$image")" = "$version" ]
[ "$(docker image inspect --format '{{index .Config.Labels "io.covalent.source.fingerprint"}}' "$image")" = "$fingerprint" ]
printf '%s\n' "$nonce" > "$root/image-owned"
uid=$(id -u)
gid=$(id -g)
docker run --rm --user "$uid:$gid" --mount "type=bind,source=$root/secrets,target=/secrets" \
  "$image" provision-key --key-file /secrets/key-encryption-key >/dev/null

# These are test-owned synthetic files. The appdata writer is absent before the
# export is copied, so the drill never reads live application state.
printf 'synthetic application database after clean stop\n' > "$root/appdata-live/database.bin"
printf '{"exportedAfterStop":true,"schema":1}\n' > "$root/appdata-live/export.json"
cp -p "$root/appdata-live/database.bin" "$root/appdata-live/export.json" "$root/source/"
printf 'boot-mode=normal\n' > "$root/boot-source/go"
printf 'server-name=covalent-drill\n' > "$root/boot-source/ident.cfg"
chmod 700 "$root/config" "$root/data" "$root/secrets" "$root/sync" "$root/appdata-live" "$root/source" "$root/boot-source"
chmod 600 "$root/appdata-live"/* "$root/source"/* "$root/boot-source"/*
SH

umask 077
local_token="$local_root/local-token" remote_token="$local_root/remote-token"
openssl rand -hex 32 > "$local_token"; openssl rand -hex 32 > "$remote_token"
chmod 600 "$local_token" "$remote_token"
scp -q $opts "$remote_token" "$ssh_host:$remote_root/secrets/api-token"
ssh $opts "$ssh_host" "chmod 600 '$remote_root/secrets/api-token' && test \"\$(stat -c %a '$remote_root/secrets/api-token')\" = 600"

ssh $opts "$ssh_host" sh -s -- "$remote_root" "$nonce" "$container" "$image" "$remote_ip" "$remote_peer" "$remote_api" "$remote_sync" <<'SH'
set -eu
root=$1 nonce=$2 container=$3 image=$4 ip=$5 peer=$6 api=$7 sync=$8
case "$nonce" in ''|*[!0-9a-f]*) echo "unsafe remote drill nonce" >&2; exit 2 ;; esac
[ "${#nonce}" -eq 32 ] && [ "$container" = "covalent-remote-drill-$nonce" ] &&
  [ "$image" = "covalent-remote-drill:$nonce" ] || { echo "unsafe remote drill resource relationship" >&2; exit 2; }
! docker container inspect "$container" >/dev/null 2>&1 || { echo "selected drill container already exists" >&2; exit 1; }
uid=$(id -u)
gid=$(id -g)
docker run -d --name "$container" --user "$uid:$gid" --read-only --cap-drop ALL --security-opt no-new-privileges \
  --label "life.michaelwong.covalent.remote-drill=$nonce" \
  --cpus 0.75 --memory 768m --pids-limit 128 --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --mount "type=bind,source=$root/config,target=/config" --mount "type=bind,source=$root/data,target=/data" \
  --mount "type=bind,source=$root/sync,target=/sync" \
  --mount "type=bind,source=$root/source,target=/source,readonly" \
  --mount "type=bind,source=$root/boot-source,target=/boot-source,readonly" \
  --mount "type=bind,source=$root/secrets/key-encryption-key,target=/run/secrets/covalent-kek,readonly" \
  --mount "type=bind,source=$root/secrets/api-token,target=/run/secrets/covalent-api-token,readonly" \
  --publish "127.0.0.1:$api:8443/tcp" --publish "$ip:$peer:8787/udp" --publish "$ip:$sync:8789/tcp" \
  --env COVALENT_LISTEN=127.0.0.1:8787 --env COVALENT_PEER_LISTEN=0.0.0.0:8787 \
  --env COVALENT_HTTPS_HOST=localhost --env COVALENT_ADVERTISED_PEER_ADDRESS="$ip:$peer" \
  --env COVALENT_SYNC_ADVERTISED_ADDRESS="$ip:$sync" \
  --env COVALENT_DATA_DIR=/data --env COVALENT_KEY_ENCRYPTION_KEY_FILE=/run/secrets/covalent-kek \
  --env COVALENT_KEY_ENCRYPTION_KEY_VERSION=1 --env COVALENT_LAN_DISCOVERY=false \
  "$image" serve --api-token-file /run/secrets/covalent-api-token >/dev/null
[ "$(docker container inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$container")" = "$nonce" ]
printf '%s\n' "$nonce" > "$root/container-owned"
for n in $(seq 1 90); do
  docker exec "$container" covalent-node healthcheck --url http://127.0.0.1:8787/healthz >/dev/null 2>&1 &&
    test -f "$root/config/caddy/data/caddy/pki/authorities/local/root.crt" && exit 0
  sleep 1
done
docker logs "$container" >&2; exit 1
SH
remote_ca="$local_root/remote-root.crt"
scp -q $opts "$ssh_host:$remote_root/config/caddy/data/caddy/pki/authorities/local/root.crt" "$remote_ca"
ssh $opts -o ExitOnForwardFailure=yes -N -L "127.0.0.1:$forward:127.0.0.1:$remote_api" "$ssh_host" &
tunnel_pid=$!
if ! tunnel_identity=$(child_process_identity "$tunnel_pid"); then
  stop_uncaptured_child "$tunnel_pid" 'SSH management tunnel' || true
  kill -0 "$tunnel_pid" >/dev/null 2>&1 || tunnel_pid=''
  exit 1
fi
sleep 1; kill -0 "$tunnel_pid" >/dev/null || { echo "SSH management tunnel failed" >&2; exit 1; }

source_dir="$local_root/source" state_dir="$local_root/state" runtime_dir="$local_root/runtime"
appdata_destination="$local_root/appdata-destination" boot_destination="$local_root/boot-destination"
mkdir -p "$source_dir/nested" "$state_dir" "$runtime_dir" "$appdata_destination" "$boot_destination"
printf 'Mac to Atmos one-way payload\n' > "$source_dir/nested/payload.txt"
python3 - "$source_dir/nested/stream.bin" <<'PYDATA'
import os,sys
with open(sys.argv[1], "wb") as out:
    out.write(os.urandom(4 * 1024 * 1024))
PYDATA
"$node_bin" provision-key --key-file "$local_root/local-kek" >/dev/null
start_local_serve

SSH_HOST=$ssh_host REMOTE_CONTAINER=$container \
LOCAL_API=$local_api REMOTE_API=$forward LOCAL_TOKEN=$local_token REMOTE_TOKEN=$remote_token REMOTE_CA=$remote_ca \
LOCAL_SOURCE=$source_dir APPDATA_DESTINATION=$appdata_destination BOOT_DESTINATION=$boot_destination \
LOCAL_PEER=$local_ip:$local_peer REMOTE_PEER=$remote_ip:$remote_peer \
PYTHONPATH=$root/scripts PYTHONDONTWRITEBYTECODE=1 python3 - <<'PY'
import os,ssl,subprocess,time,urllib.error,uuid
from pathlib import Path
from remote_drill_api import DrillClient,NodeError

client=DrillClient({
    "local":("http://127.0.0.1:"+os.environ["LOCAL_API"],os.environ["LOCAL_TOKEN"],None),
    "remote":("https://localhost:"+os.environ["REMOTE_API"],os.environ["REMOTE_TOKEN"],ssl.create_default_context(cafile=os.environ["REMOTE_CA"])),
})
call=client.call

def wait(predicate,label,seconds=180):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        try:
            value=predicate()
            if value:return value
        except (OSError,urllib.error.URLError,RuntimeError,KeyError,TypeError,ValueError):
            pass
        time.sleep(.25)
    raise SystemExit(label+" did not converge")

def remote(*arguments,input_bytes=None,check=True):
    return subprocess.run([
        "ssh","-o","BatchMode=yes","-o","StrictHostKeyChecking=yes","-o","ConnectTimeout=10",
        os.environ["SSH_HOST"],"docker","exec",*(["-i"] if input_bytes is not None else []),
        os.environ["REMOTE_CONTAINER"],*arguments,
    ],input=input_bytes,capture_output=True,check=check,timeout=30)

def remote_read(path):
    result=remote("cat",path,check=False)
    return result.stdout if result.returncode==0 else None

def remote_write(path,payload):
    remote("tee",path,input_bytes=payload)

def remote_absent(path):
    return remote("test","!","-e",path,check=False).returncode==0

def status(node):return call(node,"/api/v1/sync/status")

def share(node,folder):
    rows=[row for row in status(node).get("shares",[]) if row.get("folderId")==folder]
    if len(rows)!=1:raise RuntimeError("folder share is not singular")
    return rows[0]

def pair():
    local_identity=call("local","/api/v1/transport/identity")
    remote_identity=call("remote","/api/v1/transport/identity")
    invite=call("local","/api/v1/pair/invitations",{"lifetimeMs":600000,"endpoints":[os.environ["LOCAL_PEER"]]})
    session=call("remote","/api/v1/pair/accept",{
        "invitation":invite,"responderName":"Atmos folder-link drill",
        "responderRoles":["storage_provider","backup_reader"],
        "inviterRoles":["backup_writer","backup_reader"],
    })
    code=session["authenticationString"]
    session=call("remote","/api/v1/pair/confirm/responder",{"session":session,"displayedCode":code})
    session=call("local","/api/v1/pair/confirm/inviter",{"session":session,"displayedCode":code})
    finalized=call("local","/api/v1/pair/finalize/inviter",{"session":session})
    call("remote","/api/v1/pair/finalize/responder",{"session":session})
    transport=finalized.get("peerTransport")
    if not isinstance(transport,dict) or transport.get("address")!=os.environ["REMOTE_PEER"]:
        raise SystemExit("pairing did not retain the exact Atmos QUIC route")
    if transport.get("peerId")!=remote_identity.get("deviceId") or transport.get("certificateFingerprint")!=remote_identity.get("certificateFingerprint"):
        raise SystemExit("signed Atmos binding does not match its live identity")
    return local_identity["deviceId"],remote_identity["deviceId"]

def offer(source,destination,peer,source_root,destination_root,label):
    folder=str(uuid.uuid4())
    offered=call(source,"/api/v1/sync/folders",{
        "peerId":peer,"folderId":folder,"label":label,"selectedRoot":source_root,
        "cadence":{"mode":"manual"},
        "linkPolicy":{"propagateSourceDeletions":False,"restoreLocalDeletions":False},
    })
    offer_id=offered["offerId"]
    wait(lambda:any(row.get("offerId")==offer_id and row.get("incoming") is True for row in status(destination).get("shares",[])),label+" offer delivery")
    call(destination,"/api/v1/sync/accept",{"offerId":offer_id,"selectedRoot":destination_root})
    def ready():
        rows=[share(source,folder),share(destination,folder)]
        return all(row.get("phase")=="ready" and row["linkSettings"]["settings"]["cadence"]=={"mode":"manual"} for row in rows)
    wait(ready,label+" consent")
    return folder

def run(source,destination,folder,label):
    row=share(source,folder)
    current=row.get("linkRun") or {}
    generation=current.get("generation",0)
    revision=row["linkSettings"]["revision"]
    request={"folderId":folder,"requestId":str(uuid.uuid4()),"expectedGeneration":generation,"settingsRevision":revision}
    deadline=time.monotonic()+180
    while True:
        try:
            call(source,"/api/v1/sync/run",request)
            break
        except NodeError as error:
            if error.code!="folder_sync_busy" or time.monotonic()>=deadline:raise
            time.sleep(.25)
    expected=generation+1
    wait(lambda:all((share(node,folder).get("linkRun") or {}).get("generation")==expected and (share(node,folder).get("linkRun") or {}).get("phase")=="succeeded" for node in (source,destination)),label+" terminal run")
    wait(lambda:status(source).get("lifecycle")=="stopped" and status(destination).get("lifecycle")=="stopped",label+" worker stop")

wait(lambda:call("local","/api/v1/status").get("state")=="ready" and call("remote","/api/v1/status").get("state")=="ready","node readiness",90)
local_id,remote_id=pair()
print("remote drill: mutual signed pairing over the Tailnet: ok")

local_source=Path(os.environ["LOCAL_SOURCE"])
appdata_destination=Path(os.environ["APPDATA_DESTINATION"])
boot_destination=Path(os.environ["BOOT_DESTINATION"])
local_payload=(local_source/"nested/payload.txt").read_bytes()
local_stream=(local_source/"nested/stream.bin").read_bytes()
app_database=b"synthetic application database after clean stop\n"
app_export=b'{"exportedAfterStop":true,"schema":1}\n'
boot_go=b"boot-mode=normal\n"
boot_ident=b"server-name=covalent-drill\n"

mac_to_atmos=offer("local","remote",remote_id,str(local_source),"/sync","Mac to Atmos sync mount")
run("local","remote",mac_to_atmos,"Mac to Atmos")
wait(lambda:remote_read("/sync/nested/payload.txt")==local_payload and remote_read("/sync/nested/stream.bin")==local_stream,"Mac to Atmos exact bytes")
remote_write("/sync/remote-only.txt",b"must stay at the destination\n")
run("local","remote",mac_to_atmos,"Mac to Atmos no reverse")
if (local_source/"remote-only.txt").exists():raise SystemExit("Atmos destination content flowed back to the Mac source")
if remote_read("/sync/remote-only.txt")!=b"must stay at the destination\n":raise SystemExit("Atmos destination-only content was not retained")
if (local_source/"nested/payload.txt").read_bytes()!=local_payload or (local_source/"nested/stream.bin").read_bytes()!=local_stream:
    raise SystemExit("Mac source changed during transfer")
print("remote drill: Mac source to owned Atmos /sync bind, exact bytes and no reverse flow: ok")

appdata=offer("remote","local",local_id,"/source",str(appdata_destination),"Quiesced appdata export")
run("remote","local",appdata,"appdata export")
if (appdata_destination/"database.bin").read_bytes()!=app_database or (appdata_destination/"export.json").read_bytes()!=app_export:
    raise SystemExit("quiesced appdata export bytes differ")
(appdata_destination/"local-only.txt").write_bytes(b"must stay at the destination\n")
run("remote","local",appdata,"appdata no reverse")
if not remote_absent("/source/local-only.txt"):raise SystemExit("Mac destination content flowed into the appdata source")
if (appdata_destination/"local-only.txt").read_bytes()!=b"must stay at the destination\n":raise SystemExit("Mac appdata destination-only content was not retained")
if remote_read("/source/database.bin")!=app_database or remote_read("/source/export.json")!=app_export:
    raise SystemExit("appdata source changed during transfer")
print("remote drill: synthetic quiesced appdata export through /source read-only, exact bytes and no reverse flow: ok")

boot=offer("remote","local",local_id,"/boot-source",str(boot_destination),"Readable boot files")
run("remote","local",boot,"boot files")
if (boot_destination/"go").read_bytes()!=boot_go or (boot_destination/"ident.cfg").read_bytes()!=boot_ident:
    raise SystemExit("readable boot fixture bytes differ")
(boot_destination/"local-only.txt").write_bytes(b"must stay at the destination\n")
run("remote","local",boot,"boot no reverse")
if not remote_absent("/boot-source/local-only.txt"):raise SystemExit("Mac destination content flowed into the boot source")
if (boot_destination/"local-only.txt").read_bytes()!=b"must stay at the destination\n":raise SystemExit("Mac boot destination-only content was not retained")
if remote_read("/boot-source/go")!=boot_go or remote_read("/boot-source/ident.cfg")!=boot_ident:
    raise SystemExit("boot source changed during transfer")
for path in ("/source/database.bin","/boot-source/go"):
    attempt=remote("tee","-a",path,input_bytes=b"x",check=False)
    if attempt.returncode==0:raise SystemExit("read-only source mount accepted a write")
if remote_read("/source/database.bin")!=app_database or remote_read("/source/export.json")!=app_export:
    raise SystemExit("read-only append probe changed the appdata source")
if remote_read("/boot-source/go")!=boot_go or remote_read("/boot-source/ident.cfg")!=boot_ident:
    raise SystemExit("read-only append probe changed the boot source")
print("remote drill: readable boot fixture through /boot-source read-only, exact bytes and no reverse flow: ok")
print("remote drill: appdata evidence is a synthetic export made before container startup; boot evidence proves readable file copy, not application restore or bootability")
PY

echo "remote drill: complete; trap removes only drill-owned resources"
