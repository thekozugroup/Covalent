#!/bin/sh
# Opt-in, isolated Mac <-> Atmos Tailnet QUIC drill.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ssh_host=Atmos
revision=''
node_bin="$root/target/debug/covalent-node"
run=false

usage() {
  cat <<'EOF'
Usage: scripts/test-remote-drill.sh --source-revision COMMIT [--node-bin PATH] [--ssh HOST] --execute
Without --execute this makes no local or remote changes.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --source-revision) revision=$2; shift 2 ;;
    --node-bin) node_bin=$2; shift 2 ;;
    --ssh) ssh_host=$2; shift 2 ;;
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
local_root='' remote_root='' builder='' image='' container='' node_pid='' tunnel_pid=''
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
  if ! ssh $opts "$ssh_host" sh -s -- "$remote_root" "$builder" "$image" "$container" <<'SH'
set -u
root=$1 builder=$2 image=$3 container=$4
case "$root" in /tmp/covalent-remote-drill.[A-Za-z0-9]*) ;; *) echo "unsafe remote drill path" >&2; exit 2 ;; esac
suffix=${root#/tmp/covalent-remote-drill.}
case "$suffix" in ''|*[!A-Za-z0-9]*) echo "unsafe remote drill suffix" >&2; exit 2 ;; esac
case "$builder:$image:$container" in
  covalent-remote-drill-[0-9]*-[0-9]*:covalent-remote-drill:[0-9]*-[0-9]*:covalent-remote-drill-[0-9]*-[0-9]*) ;;
  *) echo "unsafe remote drill resource name" >&2; exit 2 ;;
esac
docker info >/dev/null 2>&1 || { echo "cannot inspect Docker while cleaning the drill" >&2; exit 1; }
cleanup_status=0
buildkit_container="buildx_buildkit_${builder}0"
buildkit_image=$(docker container inspect --format '{{.Image}}' "$buildkit_container" 2>/dev/null || true)
if docker container inspect "$container" >/dev/null 2>&1; then
  docker rm -f "$container" >/dev/null 2>&1 || { echo "could not remove drill container: $container" >&2; cleanup_status=1; }
fi
if docker container inspect "$container" >/dev/null 2>&1; then
  echo "drill container remains: $container" >&2; cleanup_status=1
fi
if docker buildx inspect "$builder" >/dev/null 2>&1; then
  docker buildx rm -f "$builder" >/dev/null 2>&1 || { echo "could not remove drill builder: $builder" >&2; cleanup_status=1; }
fi
if docker buildx inspect "$builder" >/dev/null 2>&1; then
  echo "drill builder remains: $builder" >&2; cleanup_status=1
fi
if docker ps -aq --filter "name=buildx_buildkit_$builder" | grep -q .; then
  echo "drill BuildKit container remains for builder: $builder" >&2; cleanup_status=1
fi
if docker volume inspect "${buildkit_container}_state" >/dev/null 2>&1; then
  echo "drill BuildKit volume remains: ${buildkit_container}_state" >&2; cleanup_status=1
fi
case "$buildkit_image" in
  sha256:*)
    if [ -f "$root/preexisting-full-image-ids" ] && ! grep -Fqx -- "$buildkit_image" "$root/preexisting-full-image-ids"; then
      if docker ps -aq --filter "ancestor=$buildkit_image" | grep -q .; then
        echo "new drill BuildKit image is now in use; retained: $buildkit_image" >&2; cleanup_status=1
      elif ! docker image rm "$buildkit_image" >/dev/null 2>&1; then
        echo "could not remove drill-only BuildKit image: $buildkit_image" >&2; cleanup_status=1
      fi
      if docker image inspect "$buildkit_image" >/dev/null 2>&1; then
        echo "drill-only BuildKit image remains: $buildkit_image" >&2; cleanup_status=1
      fi
    fi
    ;;
esac
if docker image inspect "$image" >/dev/null 2>&1; then
  docker image rm "$image" >/dev/null 2>&1 || { echo "could not remove drill image: $image" >&2; cleanup_status=1; }
fi
if docker image inspect "$image" >/dev/null 2>&1; then
  echo "drill image remains: $image" >&2; cleanup_status=1
fi
if [ -e "$root" ]; then
  rm -rf -- "$root" || { echo "could not remove drill temporary path: $root" >&2; cleanup_status=1; }
fi
if [ -e "$root" ]; then
  echo "drill temporary path remains: $root" >&2; cleanup_status=1
fi
exit "$cleanup_status"
SH
  then
    echo "remote drill cleanup failed; possible residue: path=$remote_root container=$container builder=$builder image=$image" >&2
    return 1
  fi
}
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  cleanup_status=0
  [ -z "$tunnel_pid" ] || { kill "$tunnel_pid" >/dev/null 2>&1 || true; wait "$tunnel_pid" >/dev/null 2>&1 || true; }
  [ -z "$node_pid" ] || { kill "$node_pid" >/dev/null 2>&1 || true; wait "$node_pid" >/dev/null 2>&1 || true; }
  remote_cleanup || cleanup_status=1
  assert_preexisting_ids_survive || cleanup_status=1
  [ -z "$local_root" ] || rm -rf -- "$local_root"
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

id=$(date +%s)-$$
builder=covalent-remote-drill-$id image=covalent-remote-drill:$id container=covalent-remote-drill-$id
remote_root=$(ssh $opts "$ssh_host" 'mktemp -d /tmp/covalent-remote-drill.XXXXXX')
valid_remote_root "$remote_root" || { echo "unsafe remote temporary path" >&2; exit 1; }
preexisting_containers="$local_root/preexisting-container-ids"
preexisting_images="$local_root/preexisting-image-ids"
ssh $opts "$ssh_host" 'docker ps -aq | sort -u' > "$preexisting_containers"
ssh $opts "$ssh_host" 'docker image ls -aq | sort -u' > "$preexisting_images"
ssh $opts "$ssh_host" "mkdir -p '$remote_root/source' '$remote_root/config' '$remote_root/data' '$remote_root/secrets'"
ssh $opts "$ssh_host" "docker image ls --no-trunc -aq | sort -u > '$remote_root/preexisting-full-image-ids'"
cat "$archive" | ssh $opts "$ssh_host" "tar -xzf - -C '$remote_root/source'"
scp -q $opts "$manifest" "$ssh_host:$remote_root/source-manifest.sha256"

# A dedicated docker-container BuildKit instance prevents this build from
# sharing the daemon builder/cache. Unsupported resource options must fail.
ssh $opts "$ssh_host" sh -s -- "$remote_root" "$builder" "$image" <<'SH'
set -eu
root=$1 builder=$2 image=$3
docker buildx create --name "$builder" --driver docker-container \
  --driver-opt image=moby/buildkit:v0.22.0 --driver-opt memory=4294967296 --driver-opt memory-swap=4294967296 \
  --driver-opt cpu-quota=100000 --driver-opt cpu-period=100000 --driver-opt cpuset-cpus=0 >/dev/null
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
  --file "$root/source/packaging/docker/Dockerfile" "$root/source"
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

ssh $opts "$ssh_host" sh -s -- "$remote_root" "$container" "$image" "$remote_ip" "$remote_peer" "$remote_api" <<'SH'
set -eu
root=$1 container=$2 image=$3 ip=$4 peer=$5 api=$6
uid=$(id -u)
gid=$(id -g)
docker run -d --name "$container" --user "$uid:$gid" --read-only --cap-drop ALL --security-opt no-new-privileges \
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
sleep 1; kill -0 "$tunnel_pid" >/dev/null || { echo "SSH management tunnel failed" >&2; exit 1; }

source_dir="$local_root/source" state_dir="$local_root/state" restore_dir="$local_root/restore"
mkdir -p "$source_dir/nested/empty" "$state_dir" "$restore_dir"
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
"$node_bin" serve --listen "127.0.0.1:$local_api" --peer-listen "$local_ip:$local_peer" \
  --advertised-peer-address "$local_ip:$local_peer" --data-dir "$state_dir" --device-name 'Mac remote drill' \
  --key-encryption-key-file "$local_root/local-kek" --api-token-file "$local_token" > "$local_root/node.log" 2>&1 &
node_pid=$!
for n in $(seq 1 90); do "$node_bin" healthcheck --url "http://127.0.0.1:$local_api/healthz" >/dev/null 2>&1 && break; sleep 1; done
"$node_bin" healthcheck --url "http://127.0.0.1:$local_api/healthz" >/dev/null || { sed -n '1,120p' "$local_root/node.log" >&2; exit 1; }

SSH_HOST=$ssh_host REMOTE_CONTAINER=$container \
LOCAL_API=$local_api REMOTE_API=$forward LOCAL_TOKEN=$local_token REMOTE_TOKEN=$remote_token REMOTE_CA=$remote_ca \
SOURCE_DIR=$source_dir RESTORE_DIR=$restore_dir LOCAL_ROOT=$local_root LOCAL_PEER=$local_ip:$local_peer REMOTE_PEER=$remote_ip:$remote_peer python3 - <<'PY'
import concurrent.futures,hashlib,json,os,ssl,subprocess,time,urllib.error,urllib.request,uuid
from pathlib import Path
tokens={k:Path(os.environ[k]).read_text().strip() for k in ("LOCAL_TOKEN","REMOTE_TOKEN")}
base={"local":"http://127.0.0.1:"+os.environ["LOCAL_API"],"remote":"https://localhost:"+os.environ["REMOTE_API"]}
ctx={"local":None,"remote":ssl.create_default_context(cafile=os.environ["REMOTE_CA"])}
class NodeError(RuntimeError):
 def __init__(self,node,path,status,response):
  self.code=response.get("code")
  super().__init__(node+" "+path+" HTTP "+str(status)+" "+json.dumps(response))
def call(node,path,body=None):
 d=None if body is None else json.dumps(body).encode()
 h={"Accept":"application/json","Authorization":"Bearer "+tokens[node.upper()+"_TOKEN"]}
 if d:h["Content-Type"]="application/json"
 try:
  with urllib.request.urlopen(urllib.request.Request(base[node]+path,data=d,headers=h,method="GET" if d is None else "POST"),timeout=120,context=ctx[node]) as r:return json.loads(r.read() or b"null")
 except urllib.error.HTTPError as e:raise NodeError(node,path,e.code,json.loads(e.read())) from e
for _ in range(30):
 try:
  if call("local","/api/v1/status")["state"]=="ready" and call("remote","/api/v1/status")["state"]=="ready":break
 except (OSError,urllib.error.URLError,RuntimeError):time.sleep(1)
else:raise SystemExit("node readiness failed")
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
if backup["selectedProviders"]!=1 or backup["degradedFailures"]!=0:raise SystemExit("replication failed")
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
echo "remote drill: complete; trap removes only drill-owned resources"
