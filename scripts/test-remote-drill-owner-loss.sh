#!/bin/sh
# Historical filename retained because validate-foundation invokes it directly.
# The drill no longer has an owner-loss or legacy backup mode.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
drill="$repo_root/scripts/test-remote-drill.sh"
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-remote-drill-contract.XXXXXX")
node_pid='' sentinel_pid='' remote_fixture='' child_shutdown_checks=100
cleanup() {
  [ -z "$node_pid" ] || { kill -KILL "$node_pid" >/dev/null 2>&1 || true; wait "$node_pid" >/dev/null 2>&1 || true; }
  [ -z "$sentinel_pid" ] || { kill -KILL "$sentinel_pid" >/dev/null 2>&1 || true; wait "$sentinel_pid" >/dev/null 2>&1 || true; }
  [ -z "$remote_fixture" ] || rm -rf "$remote_fixture"
  rm -rf "$fixture_root"
}
trap cleanup EXIT INT TERM

sh -n "$drill"
dry_run=$($drill)
printf '%s\n' "$dry_run" | grep -Fq -- '--mac-app /path/to/Covalent.app'
printf '%s\n' "$dry_run" | grep -Fq -- '--mac-build-receipt /path/to/build-receipt.json'
printf '%s\n' "$dry_run" | grep -Fq -- '--prior-cleanup-confirmed'

for required in \
  'verify-apple-silicon-bundle.sh' \
  'Mac build receipt does not bind the requested source' \
  'life.michaelwong.covalent.remote-drill.node' \
  'test-only manifest rewrite changed production provenance' \
  'node_bin="$runtime_contents/MacOS/covalent-node"' \
  'remote_listeners=$(ssh ' \
  'pinned BuildKit image is not pre-existing' \
  'git -C "$root" archive --format=tar "$commit"' \
  'docker-source-fingerprint.sh' \
  '--build-arg "VCS_REF=$commit"' \
  '--build-arg "COVALENT_SOURCE_FINGERPRINT=$fingerprint"' \
  '--mount "type=bind,source=$root/sync,target=/sync"' \
  '--mount "type=bind,source=$root/source,target=/source,readonly"' \
  '--mount "type=bind,source=$root/boot-source,target=/boot-source,readonly"' \
  '--publish "$ip:$sync:8789/tcp"' \
  '--env COVALENT_SYNC_ADVERTISED_ADDRESS="$ip:$sync"' \
  '"/api/v1/sync/folders"' \
  '"/api/v1/sync/accept"' \
  '"/api/v1/sync/run"' \
  'synthetic quiesced appdata export through /source read-only' \
  'readable boot fixture through /boot-source read-only'
do
  grep -Fq -- "$required" "$drill" || { echo "remote drill is missing current contract: $required" >&2; exit 1; }
done
for obsolete in \
  '--owner-loss' \
  'target/debug/covalent-node' \
  '/api/v1/backups' \
  '/api/v1/restores' \
  '/api/v1/providers/connect' \
  'backup_failure_diagnostics' \
  'start_local_recover'
do
  if grep -Fq -- "$obsolete" "$drill"; then
    echo "remote drill retained obsolete flow: $obsolete" >&2
    exit 1
  fi
done
receipt_line=$(grep -n '^python3 - "$mac_build_receipt" ' "$drill" | cut -d: -f1)
first_remote_line=$(grep -n '^probe=$(ssh ' "$drill" | cut -d: -f1)
test -n "$receipt_line" && test -n "$first_remote_line" && test "$receipt_line" -lt "$first_remote_line"
grep -Fq 'packaged engine only, not the installable Mac app, Keychain, or security-scoped bookmark lifecycle' "$drill"
if grep -Eq '(^|[[:space:]])docker([[:space:]].*)?[[:space:]]prune([[:space:]]|$)|(^|[[:space:]])(pkill|killall)([[:space:]]|$)' "$drill"; then
  echo "remote drill contains a broad cleanup command" >&2
  exit 1
fi

DRILL="$drill" OUTPUT="$fixture_root/validate-receipt.py" PYTHONDONTWRITEBYTECODE=1 python3 - <<'PYRECEIPTEXTRACT'
import os
from pathlib import Path
text=Path(os.environ["DRILL"]).read_text()
anchor='  "$app_binary" "$packaged_node" \\\n'
start=text.index(anchor)
start=text.index("<<'PYRECEIPT'\n",start)+len("<<'PYRECEIPT'\n")
end=text.index("\nPYRECEIPT\n",start)
body=text[start:end]
compile(body,os.environ["DRILL"]+":receipt-python","exec")
Path(os.environ["OUTPUT"]).write_text(body)
PYRECEIPTEXTRACT
python3 - "$fixture_root" <<'PYRECEIPTFIXTURE'
import hashlib,json,pathlib,subprocess,sys
root=pathlib.Path(sys.argv[1])
paths={name:root/name for name in ("app","node","worker","guardian","manifest")}
for name,path in paths.items(): path.write_bytes((name+" fixture\n").encode())
def descriptor(path):
    payload=path.read_bytes()
    return {"bytes":len(payload),"sha256":hashlib.sha256(payload).hexdigest()}
receipt=root/"receipt.json"
value={"schemaVersion":1,"sourceCommit":"a"*40,"sourceFingerprint":"b"*64,"releaseVersion":"0.2.0","archive":{"bytes":1,"sha256":"c"*64},"components":{"appExecutable":descriptor(paths["app"]),"covalent-node":descriptor(paths["node"]),"covalent-rclone":descriptor(paths["worker"]),"covalent-engine-guardian":descriptor(paths["guardian"]),"engineManifest":descriptor(paths["manifest"])}}
receipt.write_text(json.dumps(value))
args=[sys.executable,str(root/"validate-receipt.py"),str(receipt),"a"*40,"b"*64,"0.2.0",*(str(paths[name]) for name in ("app","node","worker","guardian","manifest"))]
subprocess.run(args,check=True)
paths["worker"].write_bytes(b"changed\n")
if subprocess.run(args,capture_output=True).returncode==0:
    raise SystemExit("receipt accepted changed packaged bytes")
PYRECEIPTFIXTURE

DRILL="$drill" OUTPUT="$fixture_root/rewrite-manifest.py" PYTHONDONTWRITEBYTECODE=1 python3 - <<'PYMANIFESTEXTRACT'
import os
from pathlib import Path
text=Path(os.environ["DRILL"]).read_text()
start=text.index("<<'PYMANIFESTREWRITE'\n")+len("<<'PYMANIFESTREWRITE'\n")
end=text.index("\nPYMANIFESTREWRITE\n",start)
body=text[start:end]
compile(body,os.environ["DRILL"]+":manifest-python","exec")
Path(os.environ["OUTPUT"]).write_text(body)
PYMANIFESTEXTRACT
python3 - "$fixture_root" <<'PYMANIFESTFIXTURE'
import json,pathlib,subprocess,sys
root=pathlib.Path(sys.argv[1]); path=root/"engine-manifest.json"
original={"schema":1,"engine":{"name":"rclone"},"executables":{"covalent-engine-guardian":{"signedSha256":"a"*64,"architecture":"arm64"},"covalent-rclone":{"signedSha256":"b"*64,"architecture":"arm64"}}}
path.write_text(json.dumps(original))
subprocess.run([sys.executable,str(root/"rewrite-manifest.py"),str(path),"c"*64,"d"*64],check=True)
updated=json.loads(path.read_text())
assert updated["executables"]["covalent-engine-guardian"]["signedSha256"]=="c"*64
assert updated["executables"]["covalent-rclone"]["signedSha256"]=="d"*64
updated["executables"]["covalent-engine-guardian"]["signedSha256"]="a"*64
updated["executables"]["covalent-rclone"]["signedSha256"]="b"*64
assert updated==original
PYMANIFESTFIXTURE

identity_function=$(awk '/^child_process_identity\(\)/,/^}/' "$drill")
uncaptured_running_function=$(awk '/^uncaptured_child_is_running\(\)/,/^}/' "$drill")
stop_uncaptured_function=$(awk '/^stop_uncaptured_child\(\)/,/^}/' "$drill")
running_function=$(awk '/^owned_child_is_running\(\)/,/^}/' "$drill")
stop_owned_function=$(awk '/^stop_owned_child\(\)/,/^}/' "$drill")
stop_function=$(awk '/^stop_local_node\(\)/,/^}/' "$drill")
cleanup_function=$(awk '/^cleanup\(\)/,/^}/' "$drill")
eval "$identity_function"
eval "$uncaptured_running_function"
eval "$stop_uncaptured_function"
eval "$running_function"
eval "$stop_owned_function"
eval "$stop_function"

python3 -c 'import signal,sys,time; signal.signal(signal.SIGTERM,lambda *_:sys.exit(0)); time.sleep(3600)' &
node_pid=$!
node_identity=$(child_process_identity "$node_pid")
sleep 1
stop_local_node true
test -z "$node_pid"

python3 -c 'import time; time.sleep(3600)' &
sentinel_pid=$!
python3 -c 'import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(3600)' &
node_pid=$!
node_identity=$(child_process_identity "$node_pid")
sleep 1
child_shutdown_checks=2
if stop_local_node false 2> "$fixture_root/forced-shutdown.log"; then
  echo "TERM-ignoring child did not report forced shutdown" >&2
  exit 1
fi
grep -Fq 'ignored TERM and required KILL' "$fixture_root/forced-shutdown.log"
test -z "$node_pid"
kill -0 "$sentinel_pid"
kill -TERM "$sentinel_pid"
wait "$sentinel_pid" >/dev/null 2>&1 || true
sentinel_pid=''

preserved="$fixture_root/preserved-after-failed-stop"
mkdir "$preserved"
(
  eval "$cleanup_function"
  stop_tunnel() { return 0; }
  stop_local_node() { return 1; }
  remote_cleanup() { return 0; }
  assert_preexisting_ids_survive() { return 0; }
  node_pid='' tunnel_pid='' local_root="$preserved"
  cleanup
) >/dev/null 2> "$fixture_root/failed-local-cleanup.err" && {
  echo "failed local shutdown returned a clean result" >&2
  exit 1
}
test -d "$preserved"
grep -Fq 'retaining private fixture for safe diagnosis' "$fixture_root/failed-local-cleanup.err"

port_block=$(awk '/^local_tcp=\$\(netstat /,/^fi$/' "$drill")
cat > "$fixture_root/netstat" <<'SH'
#!/bin/sh
exit 71
SH
chmod +x "$fixture_root/netstat"
if PATH="$fixture_root:$PATH" sh -c "local_api=28085 forward=28089 local_sync=8789 local_peer=28086; $port_block" \
    > /dev/null 2> "$fixture_root/netstat-failure.err"; then
  echo "local port inventory failure was accepted" >&2
  exit 1
fi
grep -Fq 'could not inspect local TCP ports' "$fixture_root/netstat-failure.err"

DRILL="$drill" OUTPUT="$fixture_root/remote-cleanup.sh" PYTHONDONTWRITEBYTECODE=1 python3 - <<'PYEXTRACT'
import os
from pathlib import Path
text=Path(os.environ["DRILL"]).read_text()
anchor='  if ! ssh $opts "$ssh_host" sh -s -- "$remote_root" "$nonce" "$builder" "$image" "$container" <<\'SH\'\n'
start=text.index(anchor)+len(anchor)
end=text.index('\nSH\n',start)
Path(os.environ["OUTPUT"]).write_text(text[start:end]+'\n')
PYEXTRACT
fake_bin="$fixture_root/fake-bin"
mkdir "$fake_bin"
cat > "$fake_bin/docker" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >> "$DRILL_DOCKER_LOG"
case "$1:$2" in
  info:) exit 0 ;;
  ps:*) printf '%s\n' fixture-container-id; exit 0 ;;
  buildx:ls) printf '%s\n' "$DRILL_BUILDER"; exit 0 ;;
  image:ls) printf '%s\n' fixture-image-id; exit 0 ;;
  container:inspect)
    [ "${DRILL_FAIL_INSPECT:-false}" != true ] || exit 78
    exit 0
    ;;
  image:inspect) exit 0 ;;
  *) exit 77 ;;
esac
SH
chmod +x "$fake_bin/docker"
remote_fixture=$(mktemp -d /tmp/covalent-remote-drill.XXXXXX)
fixture_nonce=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
docker_log="$fixture_root/docker.log"
if PATH="$fake_bin:$PATH" DRILL_DOCKER_LOG="$docker_log" DRILL_BUILDER="covalent-remote-drill-$fixture_nonce" sh "$fixture_root/remote-cleanup.sh" \
    "$remote_fixture" "$fixture_nonce" "covalent-remote-drill-$fixture_nonce" \
    "covalent-remote-drill:$fixture_nonce" "covalent-remote-drill-$fixture_nonce" \
    > "$fixture_root/remote-cleanup.out" 2> "$fixture_root/remote-cleanup.err"; then
  echo "unowned pre-existing Docker resources were accepted as a clean result" >&2
  exit 1
fi

rm -rf "$remote_fixture"
remote_fixture=$(mktemp -d /tmp/covalent-remote-drill.XXXXXX)
if PATH="$fake_bin:$PATH" DRILL_DOCKER_LOG="$docker_log" DRILL_BUILDER="covalent-remote-drill-$fixture_nonce" DRILL_FAIL_INSPECT=true \
    sh "$fixture_root/remote-cleanup.sh" "$remote_fixture" "$fixture_nonce" \
    "covalent-remote-drill-$fixture_nonce" "covalent-remote-drill:$fixture_nonce" \
    "covalent-remote-drill-$fixture_nonce" > "$fixture_root/inspect-failure.out" 2> "$fixture_root/inspect-failure.err"; then
  echo "Docker inspection failure was accepted as clean absence" >&2
  exit 1
fi
test -d "$remote_fixture"
grep -Fq 'could not inspect the drill container during cleanup' "$fixture_root/inspect-failure.err"
grep -Fq 'retaining drill root for ownership diagnosis' "$fixture_root/inspect-failure.err"
test -d "$remote_fixture"
grep -Fq 'lacks drill ownership proof and was retained' "$fixture_root/remote-cleanup.err"
grep -Fq 'retaining drill root for ownership diagnosis' "$fixture_root/remote-cleanup.err"
if grep -Eq '(^| )(rm|prune)( |$)' "$docker_log"; then
  echo "cleanup mutated a Docker resource without ownership proof" >&2
  exit 1
fi

DRILL="$drill" OUTPUT="$fixture_root/inner.py" PYTHONDONTWRITEBYTECODE=1 python3 - <<'PYINNER'
import os
from pathlib import Path
text=Path(os.environ["DRILL"]).read_text()
anchor="PYTHONPATH=$root/scripts PYTHONDONTWRITEBYTECODE=1 python3 - <<'PY'\n"
start=text.index(anchor)+len(anchor)
end=text.index("\nPY\n",start)
body=text[start:end]
compile(body,os.environ["DRILL"]+":inner-python","exec")
Path(os.environ["OUTPUT"]).write_text(body)
PYINNER

grep -Fq 'cadence":{"mode":"manual"}' "$fixture_root/inner.py"
grep -Fq 'expectedGeneration' "$fixture_root/inner.py"
grep -Fq 'source mount accepted a write' "$fixture_root/inner.py"
grep -Fq 'destination-only content was not retained' "$fixture_root/inner.py"
grep -Fq 'read-only append probe changed the appdata source' "$fixture_root/inner.py"

echo "Remote one-way drill contract fixtures: ok"
