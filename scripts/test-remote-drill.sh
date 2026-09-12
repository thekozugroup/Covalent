#!/bin/sh
# Opt-in, isolated Mac <-> Atmos Tailnet QUIC drill.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ssh_host=Atmos
revision=''
node_bin="$root/target/debug/covalent-node"
run=false
owner_loss=false

usage() {
  cat <<'EOF'
Usage: scripts/test-remote-drill.sh --source-revision COMMIT [--node-bin PATH] [--ssh HOST] [--owner-loss] --execute
Without --execute this makes no local or remote changes.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --source-revision) revision=$2; shift 2 ;;
    --node-bin) node_bin=$2; shift 2 ;;
    --ssh) ssh_host=$2; shift 2 ;;
    --owner-loss) owner_loss=true; shift ;;
    --execute) run=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
  esac
done
[ "$run" = true ] || { usage; exit 0; }
case "$ssh_host" in ''|-*|*[!A-Za-z0-9_.@-]*) echo "unsafe SSH host" >&2; exit 64 ;; esac
[ -n "$revision" ] || { echo "--source-revision is required" >&2; exit 64; }
for x in git ssh scp python3 openssl shasum netstat; do command -v "$x" >/dev/null || { echo "$x is required" >&2; exit 1; }; done
[ -x "$node_bin" ] || { echo "local node binary is missing: $node_bin" >&2; exit 1; }
commit=$(git -C "$root" rev-parse --verify "$revision^{commit}") || exit 1
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
[ "$(cat "$root/container-owned" 2>/dev/null || true)" = "$nonce" ] &&
  [ "$(docker container inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$container" 2>/dev/null || true)" = "$nonce" ] && container_owned=true
if [ "$container_owned" = true ]; then
  docker rm -f "$container" >/dev/null 2>&1 || { echo "could not remove drill container: $container" >&2; cleanup_status=1; }
  if docker container inspect "$container" >/dev/null 2>&1; then
    echo "drill container remains: $container" >&2; cleanup_status=1
  fi
elif docker container inspect "$container" >/dev/null 2>&1; then
  echo "selected container lacks drill ownership proof and was retained: $container" >&2; cleanup_status=1
fi
builder_owned=false
[ "$(cat "$root/builder-owned" 2>/dev/null || true)" = "$nonce" ] && builder_owned=true
if [ "$builder_owned" = true ] && docker buildx inspect "$builder" >/dev/null 2>&1; then
  docker buildx rm -f "$builder" >/dev/null 2>&1 || { echo "could not remove drill builder: $builder" >&2; cleanup_status=1; }
fi
if [ "$builder_owned" = true ]; then
  if docker buildx inspect "$builder" >/dev/null 2>&1; then
    echo "drill builder remains: $builder" >&2; cleanup_status=1
  fi
  if docker ps -aq --filter "name=buildx_buildkit_$builder" | grep -q .; then
    echo "drill BuildKit container remains for builder: $builder" >&2; cleanup_status=1
  fi
  if docker volume inspect "${buildkit_container}_state" >/dev/null 2>&1; then
    echo "drill BuildKit volume remains: ${buildkit_container}_state" >&2; cleanup_status=1
  fi
  echo "remote drill: shared BuildKit base-image cache intentionally retained"
elif docker buildx inspect "$builder" >/dev/null 2>&1; then
  echo "selected builder lacks drill ownership proof and was retained: $builder" >&2; cleanup_status=1
fi
image_owned=false
[ "$(cat "$root/image-owned" 2>/dev/null || true)" = "$nonce" ] &&
  [ "$(docker image inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$image" 2>/dev/null || true)" = "$nonce" ] && image_owned=true
if [ "$image_owned" = true ]; then
  docker image rm "$image" >/dev/null 2>&1 || { echo "could not remove drill image: $image" >&2; cleanup_status=1; }
  if docker image inspect "$image" >/dev/null 2>&1; then
    echo "drill image tag remains: $image" >&2; cleanup_status=1
  fi
elif docker image inspect "$image" >/dev/null 2>&1; then
  echo "selected image tag lacks drill ownership proof and was retained: $image" >&2; cleanup_status=1
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

wait_local_node() {
  local_log=$1
  for n in $(seq 1 90); do
    "$node_bin" healthcheck --url "http://127.0.0.1:$local_api/healthz" >/dev/null 2>&1 && return 0
    kill -0 "$node_pid" >/dev/null 2>&1 || break
    sleep 1
  done
  sed -n '1,120p' "$local_log" >&2
  echo "local drill node did not become ready" >&2
  return 1
}

start_local_serve() {
  local_log="$local_root/node.log"
  "$node_bin" serve --listen "127.0.0.1:$local_api" --peer-listen "$local_ip:$local_peer" \
    --advertised-peer-address "$local_ip:$local_peer" --data-dir "$state_dir" --device-name 'Mac remote drill' \
    --key-encryption-key-file "$local_root/local-kek" --api-token-file "$local_token" > "$local_log" 2>&1 &
  node_pid=$!
  if ! node_identity=$(child_process_identity "$node_pid"); then
    stop_uncaptured_child "$node_pid" 'local drill node' || true
    kill -0 "$node_pid" >/dev/null 2>&1 || node_pid=''
    return 1
  fi
  wait_local_node "$local_log"
}

start_local_recover() {
  local_log="$local_root/recovered-node.log"
  "$node_bin" recover --recovery-kit-file "$recovery_dir/owner.covalent-recovery" \
    --recovery-key-file "$recovery_dir/owner.covalent-recovery-key" \
    --listen "127.0.0.1:$local_api" --peer-listen "$local_ip:$local_peer" \
    --advertised-peer-address "$local_ip:$local_peer" --data-dir "$state_dir" \
    --device-name 'Recovered Mac drill' --key-encryption-key-file "$fresh_kek" \
    --key-encryption-key-version 1 --api-token-file "$local_token" > "$local_log" 2>&1 &
  node_pid=$!
  if ! node_identity=$(child_process_identity "$node_pid"); then
    stop_uncaptured_child "$node_pid" 'recovered local drill node' || true
    kill -0 "$node_pid" >/dev/null 2>&1 || node_pid=''
    return 1
  fi
  wait_local_node "$local_log"
}

remove_owner_fixture_path() {
  fixture_name=$1
  case "$fixture_name" in source|state|local-kek|local-token|node.log) ;;
    *) echo "refusing unexpected owner-loss fixture path: $fixture_name" >&2; return 1 ;;
  esac
  fixture_path="$local_root/$fixture_name"
  [ -e "$fixture_path" ] || { echo "owner-loss fixture path is missing: $fixture_path" >&2; return 1; }
  [ ! -L "$fixture_path" ] || { echo "refusing symlink owner-loss fixture path: $fixture_path" >&2; return 1; }
  rm -rf -- "$fixture_path"
  [ ! -e "$fixture_path" ] && [ ! -L "$fixture_path" ] || {
    echo "owner-loss fixture path remains after deletion: $fixture_path" >&2
    return 1
  }
}

assert_private_file() {
  private_path=$1
  [ -f "$private_path" ] && [ ! -L "$private_path" ] || {
    echo "private drill file is not a regular file: $private_path" >&2
    return 1
  }
  if private_mode=$(stat -c '%a' "$private_path" 2>/dev/null); then :
  else private_mode=$(stat -f '%Lp' "$private_path") || return 1
  fi
  [ "$private_mode" = 600 ] || {
    echo "private drill file mode is $private_mode, expected 600: $private_path" >&2
    return 1
  }
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
  else
    [ -z "$local_root" ] || rm -rf -- "$local_root"
  fi
  [ "$status" -ne 0 ] || return "$cleanup_status"
  return "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

local_api=28085 local_peer=28086 remote_peer=28087 remote_api=28088 forward=28089
tmp_base=$(printenv TMPDIR 2>/dev/null || true)
[ -n "$tmp_base" ] || tmp_base=/tmp
local_root=$(mktemp -d "$tmp_base/covalent-remote-drill.XXXXXX")
chmod 700 "$local_root"
local_root=$(CDPATH='' cd -- "$local_root" && pwd -P)
archive="$local_root/source.tar.gz" manifest="$local_root/source-manifest.sha256"

# Snapshot only tracked and non-ignored files; this records working-tree
# evaluation inputs, not release provenance.
git -C "$root" ls-files -co --exclude-standard -z |
  REPO="$root" ARCHIVE="$archive" MANIFEST="$manifest" python3 -c '
import hashlib, os, sys, tarfile
from pathlib import Path
base = Path(os.environ["REPO"])
names = sorted(x.decode() for x in sys.stdin.buffer.read().split(b"\0") if x)
with tarfile.open(os.environ["ARCHIVE"], "w:gz") as tar, open(os.environ["MANIFEST"], "w") as out:
  for name in names:
    path = base / name
    if path.is_file() and not path.is_symlink():
      out.write(hashlib.sha256(path.read_bytes()).hexdigest() + "  " + name + "\n")
      tar.add(path, arcname=name, recursive=False)
'
test -s "$archive" && test -s "$manifest"
printf '%s\n' "remote drill source commit: $commit"
printf '%s\n' "remote drill source manifest SHA-256: $(shasum -a 256 "$manifest" | awk '{print $1}')"
printf '%s\n' "remote drill node SHA-256: $(shasum -a 256 "$node_bin" | awk '{print $1}')"
printf '%s\n' 'remote drill target: Ubuntu Atmos (separate from the Unraid hardware gate)'

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
if ssh $opts "$ssh_host" "ss -H -lntu '( sport = :$remote_peer or sport = :$remote_api )'" | grep -q .; then
  echo "selected remote drill port is occupied" >&2; exit 1
fi
if netstat -an -p tcp 2>/dev/null | grep -E "[.:]($local_api|$forward)[[:space:]]" >/dev/null ||
   netstat -an -p udp 2>/dev/null | grep -E "[.:]$local_peer[[:space:]]" >/dev/null; then
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
ssh $opts "$ssh_host" "mkdir -p '$remote_root/source' '$remote_root/config' '$remote_root/data' '$remote_root/secrets'"
cat "$archive" | ssh $opts "$ssh_host" "tar -xzf - -C '$remote_root/source'"
scp -q $opts "$manifest" "$ssh_host:$remote_root/source-manifest.sha256"

# A dedicated docker-container BuildKit instance prevents this build from
# sharing the daemon builder/cache. Unsupported resource options must fail.
ssh $opts "$ssh_host" sh -s -- "$remote_root" "$nonce" "$builder" "$image" "$container" <<'SH'
set -eu
root=$1 nonce=$2 builder=$3 image=$4 container=$5
case "$nonce" in ''|*[!0-9a-f]*) echo "unsafe remote drill nonce" >&2; exit 2 ;; esac
[ "${#nonce}" -eq 32 ] && [ "$builder" = "covalent-remote-drill-$nonce" ] &&
  [ "$image" = "covalent-remote-drill:$nonce" ] && [ "$container" = "covalent-remote-drill-$nonce" ] || {
    echo "unsafe remote drill resource relationship" >&2; exit 2
  }
! docker buildx inspect "$builder" >/dev/null 2>&1 || { echo "selected drill builder already exists" >&2; exit 1; }
! docker image inspect "$image" >/dev/null 2>&1 || { echo "selected drill image tag already exists" >&2; exit 1; }
! docker container inspect "$container" >/dev/null 2>&1 || { echo "selected drill container already exists" >&2; exit 1; }
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
(cd "$root/source" && sha256sum -c "$root/source-manifest.sha256")
docker buildx build --builder "$builder" --load --tag "$image" \
  --label "life.michaelwong.covalent.remote-drill=$nonce" \
  --file "$root/source/packaging/docker/Dockerfile" "$root/source"
[ "$(docker image inspect --format '{{index .Config.Labels "life.michaelwong.covalent.remote-drill"}}' "$image")" = "$nonce" ]
printf '%s\n' "$nonce" > "$root/image-owned"
uid=$(id -u)
gid=$(id -g)
docker run --rm --user "$uid:$gid" --mount "type=bind,source=$root/secrets,target=/secrets" \
  "$image" provision-key --key-file /secrets/key-encryption-key >/dev/null
SH

umask 077
local_token="$local_root/local-token" remote_token="$local_root/remote-token"
openssl rand -hex 32 > "$local_token"; openssl rand -hex 32 > "$remote_token"
chmod 600 "$local_token" "$remote_token"
scp -q $opts "$remote_token" "$ssh_host:$remote_root/secrets/api-token"
# This host's SFTP server creates copied files as 0644 even when the source is
# 0600. Set and prove the secret contract explicitly before starting the node.
ssh $opts "$ssh_host" "chmod 600 '$remote_root/secrets/api-token' && test \"\$(stat -c %a '$remote_root/secrets/api-token')\" = 600"

ssh $opts "$ssh_host" sh -s -- "$remote_root" "$nonce" "$container" "$image" "$remote_ip" "$remote_peer" "$remote_api" <<'SH'
set -eu
root=$1 nonce=$2 container=$3 image=$4 ip=$5 peer=$6 api=$7
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
  --mount "type=bind,source=$root/secrets/key-encryption-key,target=/run/secrets/covalent-kek,readonly" \
  --mount "type=bind,source=$root/secrets/api-token,target=/run/secrets/covalent-api-token,readonly" \
  --publish "127.0.0.1:$api:8443/tcp" --publish "$ip:$peer:8787/udp" \
  --env COVALENT_LISTEN=127.0.0.1:8787 --env COVALENT_PEER_LISTEN=0.0.0.0:8787 \
  --env COVALENT_HTTPS_HOST=localhost --env COVALENT_ADVERTISED_PEER_ADDRESS="$ip:$peer" \
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

source_dir="$local_root/source" state_dir="$local_root/state" restore_dir="$local_root/restore"
recovery_dir="$local_root/recovery-export" handoff="$local_root/owner-loss-handoff.json"
mkdir -p "$source_dir/nested/empty" "$state_dir" "$restore_dir"
[ "$owner_loss" != true ] || { mkdir -m 700 "$recovery_dir"; }
printf 'Covalent remote drill payload\n' > "$source_dir/nested/payload.txt"
python3 - "$source_dir/nested/stream.bin" <<'PYDATA'
import os,sys
with open(sys.argv[1], "wb") as out:
  for _ in range(64):
    out.write(os.urandom(1024 * 1024))
PYDATA
cp "$source_dir/nested/payload.txt" "$local_root/expected.txt"
shasum -a 256 "$source_dir/nested/stream.bin" | awk '{print $1}' > "$local_root/stream.sha256"
"$node_bin" provision-key --key-file "$local_root/local-kek" >/dev/null
start_local_serve

SSH_HOST=$ssh_host REMOTE_CONTAINER=$container \
LOCAL_API=$local_api REMOTE_API=$forward LOCAL_TOKEN=$local_token REMOTE_TOKEN=$remote_token REMOTE_CA=$remote_ca \
SOURCE_DIR=$source_dir RESTORE_DIR=$restore_dir LOCAL_ROOT=$local_root LOCAL_PEER=$local_ip:$local_peer REMOTE_PEER=$remote_ip:$remote_peer \
SCRIPT_ROOT=$root/scripts OWNER_LOSS=$owner_loss RECOVERY_DIR=$recovery_dir HANDOFF=$handoff \
PYTHONDONTWRITEBYTECODE=1 python3 - <<'PY'
import concurrent.futures,hashlib,json,os,ssl,subprocess,sys,time,urllib.error,uuid
from pathlib import Path
sys.path.insert(0,os.environ["SCRIPT_ROOT"])
from remote_drill_api import DrillClient,NodeError,RECOVERY_MAXIMUM_BYTES,backup_failure_diagnostics,decode_recovery_export,write_private
client=DrillClient({
 "local":("http://127.0.0.1:"+os.environ["LOCAL_API"],os.environ["LOCAL_TOKEN"],None),
 "remote":("https://localhost:"+os.environ["REMOTE_API"],os.environ["REMOTE_TOKEN"],ssl.create_default_context(cafile=os.environ["REMOTE_CA"])),
})
call=client.call
for _ in range(30):
 try:
  if call("local","/api/v1/status")["state"]=="ready" and call("remote","/api/v1/status")["state"]=="ready":break
 except (OSError,urllib.error.URLError,RuntimeError):time.sleep(1)
else:raise SystemExit("node readiness failed")
owner_identity=call("local","/api/v1/transport/identity")
invite=call("local","/api/v1/pair/invitations",{"lifetimeMs":600000,"endpoints":[os.environ["LOCAL_PEER"]]})
session=call("remote","/api/v1/pair/accept",{"invitation":invite,"responderName":"Atmos drill provider","responderRoles":["storage_provider","backup_reader"],"inviterRoles":["backup_writer","backup_reader"]})
code=session["authenticationString"]
session=call("remote","/api/v1/pair/confirm/responder",{"session":session,"displayedCode":code})
session=call("local","/api/v1/pair/confirm/inviter",{"session":session,"displayedCode":code})
final=call("local","/api/v1/pair/finalize/inviter",{"session":session});call("remote","/api/v1/pair/finalize/responder",{"session":session})
transport=final["peerTransport"]
if not transport or transport["address"]!=os.environ["REMOTE_PEER"]:raise SystemExit("unexpected remote QUIC endpoint")
identity=call("remote","/api/v1/transport/identity")
if transport["peerId"]!=identity["deviceId"] or transport["certificateFingerprint"]!=identity["certificateFingerprint"]:raise SystemExit("signed remote transport does not match the live provider")
call("local","/api/v1/providers/connect",{"peerTransport":transport})
request={"sourceRoot":os.environ["SOURCE_DIR"],"backupId":str(uuid.uuid4()),"displayName":"Mac to Atmos drill","snapshotId":"remote-drill-0001","jobId":"remote-drill-backup","selectedProviderIds":[transport["peerId"]]}
started=time.monotonic()
with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
 pending=executor.submit(call,"local","/api/v1/backups",request)
 for _ in range(400):
  jobs=call("local","/api/v1/jobs")
  if any(j["jobId"]==request["jobId"] and j["active"] for j in jobs):break
  if pending.done():raise SystemExit("backup completed before interruption could be verified")
  time.sleep(0.025)
 else:raise SystemExit("backup job never became active")
 paused=call("local","/api/v1/jobs/control",{"jobId":request["jobId"],"action":"pause"})
 if paused["state"]!="paused":raise SystemExit("backup pause did not take effect")
 try:
  pending.result(timeout=120)
 except NodeError as error:
  if error.code!="job_paused":raise
 else:raise SystemExit("interrupted backup unexpectedly completed")
call("local","/api/v1/jobs/control",{"jobId":request["jobId"],"action":"resume"})
backup=call("local","/api/v1/backups",request)
backup_seconds=time.monotonic()-started
if backup["backupId"]!=request["backupId"]:raise SystemExit("resumed backup identity changed")
print("remote drill: paused and resumed the same backup job: ok")
if backup["selectedProviders"]!=1 or backup["degradedFailures"]!=0:
 diagnostics=backup_failure_diagnostics(
  client,
  Path(os.environ["LOCAL_ROOT"])/"state"/"backup-results"/"remote-drill-backup.json",
  backup,
  request["jobId"],
 )
 print("remote drill: backup failure diagnostics "+json.dumps(diagnostics,sort_keys=True,separators=(",",":")))
 raise SystemExit("replication failed")
check=call("local","/api/v1/backups/verify",{"backupId":backup["backupId"],"snapshotId":"remote-drill-0001","verifyProviders":True})
if not check["intact"] or check["providerAvailability"]!={transport["peerId"]:"complete"}:raise SystemExit("provider availability is not exactly complete for the selected Atmos peer")
call("local","/api/v1/jobs/acknowledge",{"jobId":"remote-drill-backup"})
subprocess.run(["ssh","-o","BatchMode=yes","-o","StrictHostKeyChecking=yes","-o","ConnectTimeout=10",os.environ["SSH_HOST"],"docker","restart",os.environ["REMOTE_CONTAINER"]],check=True,capture_output=True,timeout=60)
for _ in range(90):
 try:
  if call("remote","/api/v1/status")["state"]=="ready":break
 except (OSError,urllib.error.URLError,RuntimeError):time.sleep(1)
else:raise SystemExit("restarted provider did not become ready")
if call("remote","/api/v1/transport/identity")!=identity:raise SystemExit("provider identity changed across restart")
print("remote drill: selected provider restarted with its durable identity: ok")
if os.environ["OWNER_LOSS"]=="true":
 exported=call("local","/api/v1/recovery/kit",{"confirmed":True},maximum_bytes=RECOVERY_MAXIMUM_BYTES)
 kit,key=decode_recovery_export(exported)
 recovery=Path(os.environ["RECOVERY_DIR"])
 write_private(recovery/"owner.covalent-recovery",kit)
 write_private(recovery/"owner.covalent-recovery-key",key)
 handoff={"protocolVersion":exported["protocolVersion"],"ownerDeviceId":owner_identity["deviceId"],"providerId":transport["peerId"],"backupId":backup["backupId"],"snapshotId":"remote-drill-0001","backupSeconds":backup_seconds}
 write_private(os.environ["HANDOFF"],json.dumps(handoff,separators=(",",":"),sort_keys=True).encode())
 print("remote drill: exported a private owner-loss recovery pair: ok")
 raise SystemExit(0)
source=Path(os.environ["SOURCE_DIR"])
for p in sorted(source.rglob("*"),reverse=True):p.unlink() if p.is_file() else p.rmdir()
source.rmdir()
chunks=Path(os.environ["LOCAL_ROOT"])/"state"/"store"/"chunks"
files=[p for p in chunks.rglob("*") if p.is_file()]
if not files:raise SystemExit("no local ciphertext")
for p in files:p.unlink()
plan=call("local","/api/v1/restores/preview",{"backupId":backup["backupId"],"snapshotId":"remote-drill-0001","targetRoot":os.environ["RESTORE_DIR"],"conflictPolicy":"fail","jobId":"remote-drill-restore"})
restore_started=time.monotonic()
result=call("local","/api/v1/restores/execute",{"planId":plan["planId"]})
restore_seconds=time.monotonic()-restore_started
restore=Path(os.environ["RESTORE_DIR"]);root=Path(os.environ["LOCAL_ROOT"])
if result["filesRestored"]<2 or (restore/"nested/payload.txt").read_bytes()!=(root/"expected.txt").read_bytes():raise SystemExit("restore payload failed")
if hashlib.sha256((restore/"nested/stream.bin").read_bytes()).hexdigest()!=(root/"stream.sha256").read_text().strip() or not (restore/"nested/empty").is_dir():raise SystemExit("restore integrity failed")
call("local","/api/v1/jobs/discard",{"jobId":"remote-drill-restore"})
print("remote drill: Tailnet QUIC backup, source loss, provider-only restore: ok")
print("remote drill: 64 MiB incompressible payload; backup including pause/resume {:.2f}s; provider-only restore {:.2f}s ({:.2f} MiB/s)".format(backup_seconds,restore_seconds,64/restore_seconds))
PY

if [ "$owner_loss" = true ]; then
  assert_private_file "$recovery_dir/owner.covalent-recovery"
  assert_private_file "$recovery_dir/owner.covalent-recovery-key"
  assert_private_file "$handoff"
  stop_local_node true
  remove_owner_fixture_path source
  remove_owner_fixture_path state
  remove_owner_fixture_path local-kek
  remove_owner_fixture_path local-token
  remove_owner_fixture_path node.log

  fresh_kek="$local_root/fresh-kek"
  local_token="$local_root/fresh-token"
  "$node_bin" provision-key --key-file "$fresh_kek" >/dev/null
  openssl rand -hex 32 > "$local_token"
  chmod 600 "$local_token"
  assert_private_file "$fresh_kek"
  assert_private_file "$local_token"
  start_local_recover

  LOCAL_API=$local_api LOCAL_TOKEN=$local_token RESTORE_DIR=$restore_dir LOCAL_ROOT=$local_root \
  SCRIPT_ROOT=$root/scripts HANDOFF=$handoff PYTHONDONTWRITEBYTECODE=1 python3 - <<'PYRECOVERY'
import hashlib,json,os,sys,time,urllib.error
from pathlib import Path
sys.path.insert(0,os.environ["SCRIPT_ROOT"])
from remote_drill_api import DrillClient,RECOVERY_MAXIMUM_BYTES
call=DrillClient({"local":("http://127.0.0.1:"+os.environ["LOCAL_API"],os.environ["LOCAL_TOKEN"],None)}).call
handoff=json.loads(Path(os.environ["HANDOFF"]).read_bytes())
expected_provider=handoff["providerId"]
deadline=time.monotonic()+180
while True:
 remaining=deadline-time.monotonic()
 if remaining<=0:raise SystemExit("automatic owner-loss recovery did not finish within 180 seconds")
 try:status=call("local","/api/v1/recovery/status",maximum_bytes=RECOVERY_MAXIMUM_BYTES,timeout=min(10,remaining))
 except (OSError,urllib.error.URLError):time.sleep(min(1,max(0,remaining)));continue
 phase=status.get("phase")
 if phase=="imported":break
 if phase!="pending":raise SystemExit("automatic owner-loss recovery stopped in phase "+repr(phase))
 time.sleep(min(1,max(0,deadline-time.monotonic())))
if status.get("newerSnapshotMayExist") is not False or status.get("failures")!=[]:raise SystemExit("imported recovery retained incomplete evidence")
if status.get("protocolVersion")!=handoff["protocolVersion"]:raise SystemExit("recovery protocol version changed across owner loss")
if status.get("configuredProviderIds")!=[expected_provider] or status.get("queriedProviderIds")!=[expected_provider]:raise SystemExit("recovery provider sets do not exactly match the selected provider")
backups=status.get("recoveredBackups")
if not isinstance(backups,list) or len(backups)!=1:raise SystemExit("recovery did not import exactly one backup")
recovered=backups[0]
if recovered.get("backupId")!=handoff["backupId"] or recovered.get("snapshotId")!=handoff["snapshotId"] or recovered.get("sourceProviderIds")!=[expected_provider]:raise SystemExit("recovered backup evidence does not match the selected provider")
identity=call("local","/api/v1/transport/identity")
if identity.get("deviceId")!=handoff["ownerDeviceId"]:raise SystemExit("recovery changed the original owner device identity")
listed=call("local","/api/v1/backups")
if not isinstance(listed,list) or [item.get("backupId") for item in listed]!=[handoff["backupId"]]:raise SystemExit("recovered backup list is not exact")
restore=Path(os.environ["RESTORE_DIR"]);root=Path(os.environ["LOCAL_ROOT"])
local_chunks=root/"state/store/chunks"
if local_chunks.exists() and any(path.is_file() for path in local_chunks.rglob("*")):raise SystemExit("fresh owner unexpectedly retained local ciphertext before restore")
plan=call("local","/api/v1/restores/preview",{"backupId":handoff["backupId"],"snapshotId":handoff["snapshotId"],"targetRoot":str(restore),"conflictPolicy":"fail","jobId":"remote-drill-owner-loss-restore"})
started=time.monotonic()
result=call("local","/api/v1/restores/execute",{"planId":plan["planId"]})
restore_seconds=time.monotonic()-started
restored_files=sorted(str(path.relative_to(restore)) for path in restore.rglob("*") if path.is_file())
restored_directories=sorted(str(path.relative_to(restore)) for path in restore.rglob("*") if path.is_dir())
if result.get("filesRestored")!=2 or restored_files!=["nested/payload.txt","nested/stream.bin"] or restored_directories!=["nested","nested/empty"]:raise SystemExit("owner-loss restore tree is not exact")
if (restore/"nested/payload.txt").read_bytes()!=(root/"expected.txt").read_bytes():raise SystemExit("owner-loss restore payload failed")
digest_state=hashlib.sha256()
with open(restore/"nested/stream.bin","rb") as restored_stream:
 while chunk:=restored_stream.read(1024*1024):digest_state.update(chunk)
digest=digest_state.hexdigest()
expected_digest=(root/"stream.sha256").read_text().strip()
if digest!=expected_digest or not (restore/"nested/empty").is_dir():raise SystemExit("owner-loss restore integrity failed")
call("local","/api/v1/jobs/discard",{"jobId":"remote-drill-owner-loss-restore"})
print("remote drill: entire-owner-loss automatic catalog import and provider-only restore: ok")
print("remote drill: ownerDeviceId={} providerId={} backupId={} sourceSha256={}".format(handoff["ownerDeviceId"],expected_provider,handoff["backupId"],expected_digest))
print("remote drill: 64 MiB incompressible payload; backup including pause/resume {:.2f}s; owner-loss restore {:.2f}s ({:.2f} MiB/s)".format(handoff["backupSeconds"],restore_seconds,64/restore_seconds))
PYRECOVERY
fi
echo "remote drill: complete; trap removes only drill-owned resources"
