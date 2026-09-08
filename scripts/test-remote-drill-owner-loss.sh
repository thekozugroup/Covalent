#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
drill="$repo_root/scripts/test-remote-drill.sh"
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-owner-loss-contract.XXXXXX")
outside=$(mktemp "${TMPDIR:-/tmp}/covalent-owner-loss-outside.XXXXXX")
node_pid='' sentinel_pid='' remote_fixture='' child_shutdown_checks=100
cleanup() {
  [ -z "$node_pid" ] || { kill -KILL "$node_pid" >/dev/null 2>&1 || true; wait "$node_pid" >/dev/null 2>&1 || true; }
  [ -z "$sentinel_pid" ] || { kill -KILL "$sentinel_pid" >/dev/null 2>&1 || true; wait "$sentinel_pid" >/dev/null 2>&1 || true; }
  [ -z "$remote_fixture" ] || rm -rf "$remote_fixture"
  rm -rf "$fixture_root"
  rm -f "$outside"
}
trap cleanup EXIT INT TERM

dry_run=$($drill --owner-loss)
printf '%s\n' "$dry_run" | grep -Fq '[--owner-loss] --execute'

identity_function=$(awk '/^child_process_identity\(\)/,/^}/' "$drill")
uncaptured_running_function=$(awk '/^uncaptured_child_is_running\(\)/,/^}/' "$drill")
stop_uncaptured_function=$(awk '/^stop_uncaptured_child\(\)/,/^}/' "$drill")
running_function=$(awk '/^owned_child_is_running\(\)/,/^}/' "$drill")
stop_owned_function=$(awk '/^stop_owned_child\(\)/,/^}/' "$drill")
stop_function=$(awk '/^stop_local_node\(\)/,/^}/' "$drill")
remove_function=$(awk '/^remove_owner_fixture_path\(\)/,/^}/' "$drill")
eval "$identity_function"
eval "$uncaptured_running_function"
eval "$stop_uncaptured_function"
eval "$running_function"
eval "$stop_owned_function"
eval "$stop_function"
eval "$remove_function"

python3 -c 'import signal,sys,time; signal.signal(signal.SIGTERM,lambda *_:sys.exit(0)); time.sleep(3600)' &
node_pid=$!
node_identity=$(child_process_identity "$node_pid")
sleep 1
stop_local_node true
test -z "$node_pid"

python3 -c 'import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(3600)' &
node_pid=$!
sleep 1
child_shutdown_checks=2
if stop_uncaptured_child "$node_pid" 'uncaptured fixture child' 2> "$fixture_root/uncaptured-shutdown.log"; then
  echo "uncaptured child cleanup did not preserve failure status" >&2
  exit 1
fi
grep -Fq 'exact newly spawned child was reaped' "$fixture_root/uncaptured-shutdown.log"
if kill -0 "$node_pid" >/dev/null 2>&1; then
  echo "uncaptured fixture child survived bounded cleanup" >&2
  exit 1
fi
node_pid=''

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

local_root="$fixture_root"
mkdir "$local_root/source"
remove_owner_fixture_path source
test ! -e "$local_root/source"
ln -s "$outside" "$local_root/state"
if remove_owner_fixture_path state >/dev/null 2>&1; then
  echo "owner-loss deletion followed a symlink" >&2
  exit 1
fi
test -L "$local_root/state"
test -f "$outside"
if remove_owner_fixture_path ../outside >/dev/null 2>&1; then
  echo "owner-loss deletion accepted an unlisted path" >&2
  exit 1
fi

stop_line=$(grep -n '^  stop_local_node true$' "$drill" | cut -d: -f1)
state_line=$(grep -n '^  remove_owner_fixture_path state$' "$drill" | cut -d: -f1)
recover_line=$(grep -n '^  start_local_recover$' "$drill" | cut -d: -f1)
phase_two_line=$(grep -n "python3 - <<'PYRECOVERY'" "$drill" | cut -d: -f1)
test "$stop_line" -lt "$state_line"
test "$state_line" -lt "$recover_line"
test "$recover_line" -lt "$phase_two_line"
if grep -Eq '(^|[^[:alnum:]_])(pkill|killall)([^[:alnum:]_]|$)' "$drill"; then
  echo "remote drill must stop only its captured child PID" >&2
  exit 1
fi
if grep -Eq 'wait "\$(node_pid|tunnel_pid)"' "$drill"; then
  echo "identity-capture failure still contains an unbounded child wait" >&2
  exit 1
fi
grep -Fq 'nonce=$(openssl rand -hex 16)' "$drill"
grep -Fq 'life.michaelwong.covalent.remote-drill' "$drill"
grep -Fq 'builder-owned' "$drill"
grep -Fq 'container-owned' "$drill"
grep -Fq 'image-owned' "$drill"
grep -Fq '! docker buildx inspect "$builder"' "$drill"
grep -Fq '! docker image inspect "$image"' "$drill"
grep -Fq '! docker container inspect "$container"' "$drill"
grep -Fq 'stop_tunnel || cleanup_status=1' "$drill"
grep -Fq 'stop_local_node false || cleanup_status=1' "$drill"
grep -Fq 'remote_cleanup || cleanup_status=1' "$drill"
if grep -Fq 'buildkit_image=' "$drill"; then
  echo "remote cleanup still attempts to own the shared BuildKit image" >&2
  exit 1
fi

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
  info:|container:inspect|buildx:inspect|image:inspect) exit 0 ;;
  *) exit 77 ;;
esac
SH
chmod +x "$fake_bin/docker"
remote_fixture=$(mktemp -d /tmp/covalent-remote-drill.XXXXXX)
fixture_nonce=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
docker_log="$fixture_root/docker.log"
if PATH="$fake_bin:$PATH" DRILL_DOCKER_LOG="$docker_log" sh "$fixture_root/remote-cleanup.sh" \
    "$remote_fixture" "$fixture_nonce" "covalent-remote-drill-$fixture_nonce" \
    "covalent-remote-drill:$fixture_nonce" "covalent-remote-drill-$fixture_nonce" \
    > "$fixture_root/remote-cleanup.out" 2> "$fixture_root/remote-cleanup.err"; then
  echo "unowned pre-existing Docker resources were accepted as a clean result" >&2
  exit 1
fi
test -d "$remote_fixture"
grep -Fq 'lacks drill ownership proof and was retained' "$fixture_root/remote-cleanup.err"
grep -Fq 'retaining drill root for ownership diagnosis' "$fixture_root/remote-cleanup.err"
if grep -Eq '(^| )(rm|prune)( |$)' "$docker_log"; then
  echo "cleanup mutated a Docker resource without ownership proof" >&2
  exit 1
fi

SCRIPT_ROOT="$repo_root/scripts" FIXTURE_ROOT="$fixture_root" PYTHONDONTWRITEBYTECODE=1 python3 - <<'PY'
import base64,json,os,stat,sys,threading,traceback
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
sys.path.insert(0,os.environ["SCRIPT_ROOT"])
from remote_drill_api import DrillClient,NodeError,PROTOCOL_VERSION,ResponseTooLarge,decode_recovery_export,write_private

root=Path(os.environ["FIXTURE_ROOT"])
token=root/"token"
token.write_text("A"*64)
token.chmod(0o600)
class Handler(BaseHTTPRequestHandler):
 redirect_followed=False
 def do_GET(self):
  if self.headers.get("Authorization")!="Bearer "+"A"*64:self.send_error(401);return
  if self.path=="/redirect":
   self.send_response(302);self.send_header("Location","/redirect-target");self.end_headers();return
  if self.path=="/redirect-target":
   Handler.redirect_followed=True
  if self.path=="/declared":
   self.send_response(200);self.send_header("Content-Length","20");self.end_headers();return
  if self.path=="/chunked":payload=b'{"value":"too large"}'
  elif self.path=="/error":payload=json.dumps({"code":"fixture_error","message":"SECRET-MUST-NOT-PRINT"}).encode()
  elif self.path=="/unsafe-error":payload=json.dumps({"code":"SECRET-MUST-NOT-PRINT"}).encode()
  else:payload=b'{"ok":true}'
  self.send_response(409 if self.path in ("/error","/unsafe-error") else 200)
  if self.path!="/chunked":self.send_header("Content-Length",str(len(payload)))
  self.end_headers();self.wfile.write(payload)
 def log_message(self,*args):pass
server=ThreadingHTTPServer(("127.0.0.1",0),Handler)
thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
client=DrillClient({"local":("http://127.0.0.1:"+str(server.server_port),token,None)})
assert client.call("local","/small",maximum_bytes=16)=={"ok":True}
for path in ("/declared","/chunked"):
 try:client.call("local",path,maximum_bytes=8)
 except ResponseTooLarge:pass
 else:raise AssertionError(path+" was not bounded")
try:client.call("local","/error")
except NodeError as error:
 assert error.code=="fixture_error" and "SECRET-MUST-NOT-PRINT" not in str(error)
else:raise AssertionError("HTTP error was accepted")
try:client.call("local","/unsafe-error")
except NodeError as error:
 assert error.code is None and "SECRET-MUST-NOT-PRINT" not in str(error)
else:raise AssertionError("unsafe HTTP error code was accepted")
try:client.call("local","/redirect")
except NodeError as error:assert "HTTP 302 code=unknown" in str(error)
else:raise AssertionError("HTTP redirect was accepted")
assert not Handler.redirect_followed
server.shutdown();server.server_close();thread.join()
encoded=base64.urlsafe_b64encode(b"private kit").decode().rstrip("=")
encoded_key=base64.urlsafe_b64encode(b"B"*32).decode().rstrip("=")
kit,key=decode_recovery_export({"protocolVersion":PROTOCOL_VERSION,"recoveryKit":encoded,"recoveryKey":encoded_key})
destination=root/"kit"
write_private(destination,kit)
assert destination.read_bytes()==b"private kit" and stat.S_IMODE(destination.stat().st_mode)==0o600
try:write_private(destination,key)
except FileExistsError:pass
else:raise AssertionError("private recovery file was overwritten")
for bad in (dict(protocolVersion=True,recoveryKit=encoded,recoveryKey=encoded_key),dict(protocolVersion=PROTOCOL_VERSION,recoveryKit=encoded,recoveryKey="B"*43)):
 try:decode_recovery_export(bad)
 except RuntimeError:pass
 else:raise AssertionError("non-canonical recovery export was accepted")
secret_kit="SECRET-BEARER-MUST-NOT-APPEAR-\N{SNOWMAN}"
try:decode_recovery_export({"protocolVersion":PROTOCOL_VERSION,"recoveryKit":secret_kit,"recoveryKey":encoded_key})
except RuntimeError:
 assert secret_kit not in traceback.format_exc()
else:raise AssertionError("non-ASCII recovery kit was accepted")
PY

echo "Remote owner-loss drill fixtures: ok"
