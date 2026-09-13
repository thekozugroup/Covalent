#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-macos-ui.XXXXXX")
node_pid=""
real_responder_pid=""
app_token_directory=""
app_token_file=""
built_app=""
managed_state_root="$HOME/Library/Containers/life.michaelwong.covalent.macos/Data/Library/Application Support/Covalent"
managed_keychain_service="life.michaelwong.covalent.node-key-encryption"
managed_keychain_account="managed-node-kek-hierarchy"
managed_state_was_absent=""
managed_keychain_was_absent=""
managed_app_may_have_started=""
artifact_root=${COVALENT_TEST_ARTIFACT_DIR:-$test_root}
mkdir -p "$artifact_root"

run_bounded() {
  local limit_seconds=$1
  shift
  python3 - "$limit_seconds" "$@" <<'PY'
import os
import signal
import subprocess
import sys

limit = int(sys.argv[1])
process = subprocess.Popen(sys.argv[2:], start_new_session=True)
try:
    raise SystemExit(process.wait(timeout=limit))
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGINT)
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    print(f"Command exceeded {limit} seconds: {' '.join(sys.argv[2:])}", file=sys.stderr)
    raise SystemExit(124)
PY
}

cleanup() {
  local command_status=$?
  local cleanup_failed=0
  trap - EXIT INT TERM
  # The token file is unique. Managed production state is removed below only
  # after the preflight proved it absent and its exact helper has stopped.
  if [[ -n "$app_token_file" && -n "$app_token_directory" && "$app_token_file" == "$app_token_directory/"* ]]; then
    rm -f -- "$app_token_file" || cleanup_failed=1
  fi
  for pid in "$real_responder_pid" "$node_pid"; do
    if [[ -n "$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done

  local managed_cleanup_safe=1
  if [[ -n "$built_app" ]]; then
    for expected in \
      "$built_app/Contents/MacOS/Covalent" \
      "$built_app/Contents/MacOS/covalent-node" \
      "$built_app/Contents/MacOS/covalent-engine-guardian" \
      "$built_app/Contents/MacOS/covalent-rclone"
    do
      if ! stop_exact_executable "$expected"; then
        print -u2 -- "managed UI-test process did not stop: ${expected:t}"
        managed_cleanup_safe=0
        cleanup_failed=1
      fi
    done
  fi
  if (( managed_cleanup_safe )) \
    && [[ "$managed_app_may_have_started" == 1 \
      && "$managed_state_was_absent" == 1 \
      && -d "$managed_state_root" \
      && ! -L "$managed_state_root" ]]; then
    rm -r -- "$managed_state_root" || cleanup_failed=1
  fi
  if (( managed_cleanup_safe )) \
    && [[ "$managed_keychain_was_absent" == 1 && "$managed_app_may_have_started" == 1 ]]; then
    local keychain_status=0
    security delete-generic-password \
      -s "$managed_keychain_service" -a "$managed_keychain_account" >/dev/null 2>&1 || keychain_status=$?
    if (( keychain_status != 0 && keychain_status != 44 )); then
      print -u2 -- "managed UI-test Keychain item could not be removed safely"
      cleanup_failed=1
    fi
  fi
  if (( managed_cleanup_safe )) \
    && [[ "$test_root" == *covalent-macos-ui.* && -d "$test_root" ]]; then
    rm -r "$test_root" || cleanup_failed=1
  elif (( ! managed_cleanup_safe )); then
    print -u2 -- "managed UI-test cleanup preserved its owned files because a process remained active"
  fi
  if (( cleanup_failed )); then
    exit 1
  fi
  exit "$command_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

prepare_private_ui_token_directory() {
  python3 - "$1" <<'PY'
import os
import stat
import sys

path = sys.argv[1]
try:
    os.makedirs(path, mode=0o700, exist_ok=True)
    metadata = os.lstat(path)
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid():
        raise OSError
    os.chmod(path, 0o700)
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise OSError
except OSError:
    print("private UI-test token directory provisioning failed", file=sys.stderr)
    raise SystemExit(64)
PY
}

pids_for_exact_executable() {
  python3 - "$1" <<'PY'
import subprocess
import sys

expected = sys.argv[1]
for line in subprocess.run(
    ["ps", "-axo", "pid=,command="], check=True, text=True, capture_output=True
).stdout.splitlines():
    fields = line.strip().split(None, 1)
    if len(fields) == 2 and (fields[1] == expected or fields[1].startswith(expected + " ")):
        print(fields[0])
PY
}

stop_exact_executable() {
  local expected=$1
  local pids pid
  pids=$(pids_for_exact_executable "$expected" 2>/dev/null) || return 1
  for pid in ${(f)pids}; do
    kill "$pid" 2>/dev/null || true
  done
  for _ in {1..50}; do
    pids=$(pids_for_exact_executable "$expected" 2>/dev/null) || return 1
    [[ -z "$pids" ]] && return 0
    sleep 0.1
  done
  for pid in ${(f)pids}; do
    kill -KILL "$pid" 2>/dev/null || true
  done
  for _ in {1..20}; do
    pids=$(pids_for_exact_executable "$expected" 2>/dev/null) || return 1
    [[ -z "$pids" ]] && return 0
    sleep 0.1
  done
  return 1
}

console_session=$(ioreg -n Root -d1)
if [[ "$console_session" == *'"CGSSessionScreenIsLocked"=Yes'* ]]; then
  print -u2 -- "macOS UI tests require an unlocked headed login session; the current session is locked."
  exit 75
fi

if [[ -e "$managed_state_root" || -L "$managed_state_root" ]]; then
  print -u2 -- "macOS managed-node UI test refuses pre-existing Covalent application state."
  exit 73
fi
managed_state_was_absent=1
keychain_status=0
security find-generic-password \
  -s "$managed_keychain_service" -a "$managed_keychain_account" >/dev/null 2>&1 || keychain_status=$?
if (( keychain_status == 0 )); then
  print -u2 -- "macOS managed-node UI test refuses a pre-existing managed-node Keychain item."
  exit 73
elif (( keychain_status != 44 )); then
  print -u2 -- "macOS managed-node UI test could not establish an absent, available Keychain item."
  exit 73
fi
managed_keychain_was_absent=1

# LocalNodeManager has fixed production listeners. Refuse a conflicting host
# instead of changing its signed endpoint or terminating an unrelated service.
python3 - <<'PY'
import socket

checks = (
    (socket.SOCK_DGRAM, ("0.0.0.0", 8787), "UDP 8787"),
    (socket.SOCK_STREAM, ("0.0.0.0", 8789), "TCP 8789"),
)
sockets = []
try:
    for kind, address, label in checks:
        candidate = socket.socket(socket.AF_INET, kind)
        try:
            candidate.bind(address)
            if kind == socket.SOCK_STREAM:
                candidate.listen(1)
        except OSError as error:
            raise SystemExit(f"macOS managed-node UI test requires unused {label}: {error}")
        sockets.append(candidate)
finally:
    for candidate in sockets:
        candidate.close()
PY

port=$(python3 - <<'PY'
import socket
while True:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    if s.getsockname()[1] not in (8787, 8789):
        print(s.getsockname()[1])
        s.close()
        break
    s.close()
PY
)
data_dir="$test_root/node"
mkdir -p "$data_dir"

cargo build --locked -p covalent-node --manifest-path "$repo_root/Cargo.toml"
node_binary="$repo_root/target/debug/covalent-node"
key_file="$test_root/node-kek"
token_file="$test_root/test-api-token"
token_nonce=$(uuidgen | tr -d '-' | tr '[:upper:]' '[:lower:]')
ui_token_relative_path="ui-token-$token_nonce"
ui_token_filename="$ui_token_relative_path"
"$node_binary" provision-key --key-file "$key_file" --key-version 1 \
  >"$test_root/provision-key.log"
[[ "$(stat -f '%Lp' "$key_file")" == "600" ]]
test_settings="$test_root/TestSecrets.xcconfig"
python3 - "$token_file" "$test_settings" "$port" "$ui_token_relative_path" <<'PY'
import base64
import os
import secrets
import sys

token = base64.urlsafe_b64encode(secrets.token_bytes(48)).rstrip(b"=")
for path, value in (
    (sys.argv[1], token + b"\n"),
    (sys.argv[2], f"COVALENT_UI_TEST_PORT = {sys.argv[3]}\nCOVALENT_UI_TEST_TOKEN_FILE = {sys.argv[4]}\n".encode()),
):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        output.write(value)
PY
"$node_binary" serve \
  --listen "127.0.0.1:$port" \
  --peer-listen "127.0.0.1:$port" \
  --data-dir "$data_dir" \
  --device-name "Apple UI Test Node" \
  --platform-tier tier1 \
  --key-encryption-key-file "$key_file" \
  --key-encryption-key-version 1 \
  --api-token-file "$token_file" \
  >"$test_root/node.log" 2>&1 &
node_pid=$!

for _ in {1..100}; do
  if curl --fail --silent "http://127.0.0.1:$port/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
if ! curl --fail --silent "http://127.0.0.1:$port/healthz" >/dev/null; then
  sed -n '1,200p' "$test_root/node.log"
  exit 1
fi

# The target is sandboxed, so the test-root token cannot be launched into it.
# Copy only this run's owner-only credential into the exact application-support
# container and launch with a relative, non-secret filename.
app_token_directory="$HOME/Library/Containers/life.michaelwong.covalent.macos/Data/Library/Application Support/CovalentUITests"
prepare_private_ui_token_directory "$app_token_directory"
app_token_file="$app_token_directory/$ui_token_filename"
python3 "$script_dir/copy-owner-only-token.py" "$token_file" "$app_token_file"


cd "$apple_dir"
xcodegen generate --quiet
derived_data="$test_root/DerivedData"
build_log="$artifact_root/build-for-testing.log"
ui_log="$artifact_root/ui-test.log"
result_bundle="$artifact_root/MacUITests.xcresult"

# A failed run usually retains its summary and audit attachments. Print the
# small text attachments so the plain job log identifies each failed element.
# This helper is diagnostic-only and cannot mask xcodebuild's failure.
emit_failed_result_details() {
  if [[ -r "$result_bundle/Info.plist" ]]; then
    print -u2 -- "--- xcresult test-results summary ---"
    if ! xcrun xcresulttool get test-results summary --compact --path "$result_bundle" >&2; then
      print -u2 -- "xcresult test-results summary could not be read."
    fi

    local attachment_root="$test_root/failed-xcresult-attachments"
    if ! xcrun xcresulttool export attachments \
      --path "$result_bundle" \
      --output-path "$attachment_root" >/dev/null 2>&1; then
      print -u2 -- "xcresult attachments could not be exported."
      return 0
    fi
    local manifest="$attachment_root/manifest.json"
    if [[ ! -f "$manifest" || -L "$manifest" ]]; then
      print -u2 -- "xcresult attachment manifest is unavailable."
      return 0
    fi

    local count=0
    local name file bytes
    while IFS= read -r name; do
      [[ -n "$name" && "$name" != */* && "$name" != *..* ]] || continue
      file="$attachment_root/$name"
      [[ -f "$file" && ! -L "$file" ]] || continue
      bytes=$(stat -f '%z' "$file" 2>/dev/null || print 0)
      (( bytes > 0 && bytes <= 65536 )) || continue
      LC_ALL=C grep -Eq '^(Accessibility audit:|UI test failure:)' "$file" || continue
      if (( count == 0 )); then
        print -u2 -- "--- accessibility audit elements from xcresult ---"
      fi
      sed -n '1,80p' "$file" >&2
      (( count += 1 ))
      (( count < 64 )) || break
    done < <(
      jq -r '
        .[]?.attachments[]?
        | .exportedFileName
      ' "$manifest" 2>/dev/null
    )
    if (( count == 0 )); then
      print -u2 -- "No text accessibility audit attachments were exported."
      print -u2 -- "--- bounded xcresult attachment manifest metadata ---"
      jq -r '
        .[0:16][]?
        | .testIdentifier as $test
        | .attachments[0:16][]?
        | [$test, .suggestedHumanReadableName, .exportedFileName, .isAssociatedWithFailure]
        | @tsv
      ' "$manifest" 2>/dev/null | sed -n '1,64p' >&2
    fi
  fi
  return 0
}
if ! run_bounded 600 xcodebuild \
  -quiet \
  -project Covalent.xcodeproj \
  -scheme CovalentMac \
  -configuration Debug \
  -xcconfig "$test_settings" \
  -derivedDataPath "$derived_data" \
  -destination 'platform=macOS,arch=arm64' \
  ARCHS=arm64 \
  EXCLUDED_ARCHS=x86_64 \
  -destination-timeout 30 \
  -parallel-testing-enabled NO \
  -maximum-parallel-testing-workers 1 \
  -only-testing:CovalentMacUITests \
  build-for-testing >"$build_log" 2>&1; then
  tail -200 "$build_log" >&2
  exit 1
fi

# The source is the built sandboxed app and its production LocalNodeManager.
# Keep one unsandboxed responder so the test can confirm pairing, accept the
# destination, and inspect the copied bytes from the other side.
built_app="$derived_data/Build/Products/Debug/Covalent.app"
built_macos="$built_app/Contents/MacOS"
built_engine_resources="$built_app/Contents/Resources/CovalentSyncEngine"
for required in \
  "$node_binary" \
  "$built_macos/covalent-rclone" \
  "$built_macos/covalent-engine-guardian" \
  "$built_engine_resources/manifest.json" \
  "$built_engine_resources/notices-index.txt"
do
  [[ -f "$required" && ! -L "$required" ]] || {
    print -u2 -- "real macOS folder-link fixture is missing a packaged engine input"
    exit 1
  }
done
if ! codesign -d --verbose=4 "$built_app" 2>&1 | grep -Fq 'Signature=adhoc'; then
  print -u2 -- "managed-node UI cleanup supports only this hosted ad-hoc Debug app"
  exit 1
fi

real_fixture="$test_root/real-folder-link"
real_responder_contents="$real_fixture/responder/Covalent.app/Contents"
mkdir -p "$real_responder_contents/MacOS" "$real_responder_contents/Resources"
ditto "$node_binary" "$real_responder_contents/MacOS/covalent-node"
ditto "$built_macos/covalent-rclone" "$real_responder_contents/MacOS/covalent-rclone"
ditto "$built_macos/covalent-engine-guardian" "$real_responder_contents/MacOS/covalent-engine-guardian"
ditto "$built_engine_resources" "$real_responder_contents/Resources/CovalentSyncEngine"
chmod 755 "$real_responder_contents/MacOS/covalent-node" \
  "$real_responder_contents/MacOS/covalent-rclone" \
  "$real_responder_contents/MacOS/covalent-engine-guardian"

# Production helpers inherit the app sandbox. The responder intentionally runs
# under a raw unsandboxed node, so replace only its copied helpers' signatures
# and record their new signed-byte hashes in the copied manifest.
codesign --force --sign - \
  --identifier life.michaelwong.covalent.ui-test.engine-guardian \
  --options runtime --timestamp=none \
  "$real_responder_contents/MacOS/covalent-engine-guardian"
codesign --force --sign - \
  --identifier life.michaelwong.covalent.ui-test.rclone \
  --options runtime --timestamp=none \
  "$real_responder_contents/MacOS/covalent-rclone"

for binary in \
  "$real_responder_contents/MacOS/covalent-engine-guardian" \
  "$real_responder_contents/MacOS/covalent-rclone"
do
  codesign --verify --strict "$binary"
  if ! fixture_entitlements=$(codesign -d --entitlements - "$binary" 2>/dev/null); then
    print -u2 -- "could not inspect test fixture helper entitlements"
    exit 1
  fi
  if [[ -n "$fixture_entitlements" ]]; then
    print -u2 -- "real macOS folder-link fixture helper retained entitlements"
    exit 1
  fi
done

guardian_sha=$(shasum -a 256 "$real_responder_contents/MacOS/covalent-engine-guardian" | awk '{print $1}')
worker_sha=$(shasum -a 256 "$real_responder_contents/MacOS/covalent-rclone" | awk '{print $1}')
python3 - \
  "$real_responder_contents/Resources/CovalentSyncEngine/manifest.json" \
  "$guardian_sha" "$worker_sha" <<'PY'
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
for name, digest in (
    ("covalent-engine-guardian", guardian_sha),
    ("covalent-rclone", worker_sha),
):
    record = executables.get(name)
    if not isinstance(record, dict) or not isinstance(record.get("signedSha256"), str):
        raise SystemExit(f"copied sync-engine manifest has no signed hash for {name}")
    record["signedSha256"] = digest

restored = copy.deepcopy(updated)
for name in ("covalent-engine-guardian", "covalent-rclone"):
    previous = original["executables"][name]["signedSha256"]
    if updated["executables"][name]["signedSha256"] == previous:
        raise SystemExit("test fixture retained a production helper signature")
    restored["executables"][name]["signedSha256"] = previous
if restored != original:
    raise SystemExit("test-only manifest rewrite changed production provenance")

temporary = f"{path}.{os.getpid()}.tmp"
descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
try:
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
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
PY

real_responder_port=$(python3 - <<'PY'
import socket
while True:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    if sock.getsockname()[1] not in (8787, 8789):
        print(sock.getsockname()[1])
        sock.close()
        break
    sock.close()
PY
)
real_responder_data="$real_fixture/responder-data"
real_responder_runtime="$real_fixture/responder-runtime"
real_source_root="$real_fixture/source-folder"
real_destination_root="$real_fixture/destination-folder"
runtime_path_bytes=$(LC_ALL=C print -rn -- "$real_responder_runtime" | wc -c | tr -d '[:space:]')
[[ "$real_responder_runtime" == /* && "$runtime_path_bytes" -le 900 ]] || {
  print -u2 -- "real macOS folder-link runtime path exceeds the packaged host bound"
  exit 1
}
mkdir -m 700 "$real_responder_data" "$real_responder_runtime" \
  "$real_source_root" "$real_destination_root"
print -n -- 'packaged-rclone-forward-content' > "$real_source_root/forward.txt"
print -n -- 'destination-must-not-write-back' > "$real_destination_root/destination-only.txt"

real_responder_key="$real_fixture/responder-kek"
real_responder_token="$real_fixture/responder-token"
"$node_binary" provision-key --key-file "$real_responder_key" --key-version 1 \
  >"$real_fixture/responder-provision-key.log"
python3 - "$real_responder_token" <<'PY'
import base64
import os
import secrets
import sys

descriptor = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(descriptor, "wb") as output:
    output.write(base64.urlsafe_b64encode(secrets.token_bytes(48)).rstrip(b"=") + b"\n")
PY

# The responder stays on loopback. Its signed address must match the address
# the app dials. The sandboxed app source advertises 127.0.0.1:8787 through its
# one fixture-specific launch variable.
COVALENT_SYNC_RUNTIME_DIR="$real_responder_runtime" \
  "$real_responder_contents/MacOS/covalent-node" serve \
  --listen "127.0.0.1:$real_responder_port" \
  --peer-listen "127.0.0.1:$real_responder_port" \
  --advertised-peer-address "127.0.0.1:$real_responder_port" \
  --data-dir "$real_responder_data" \
  --device-name "Responder UI Peer" \
  --platform-tier tier1 \
  --key-encryption-key-file "$real_responder_key" \
  --key-encryption-key-version 1 \
  --api-token-file "$real_responder_token" \
  >"$real_fixture/responder-node.log" 2>&1 &
real_responder_pid=$!

for _ in {1..150}; do
  if curl --fail --silent "http://127.0.0.1:$real_responder_port/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
if ! curl --fail --silent "http://127.0.0.1:$real_responder_port/healthz" >/dev/null; then
  print -u2 -- "real macOS folder-link fixture node did not become ready"
  sed -n '1,160p' "$real_fixture/responder-node.log" >&2
  exit 1
fi

# Shell exports are not the UI test host contract. Put these values into the
# generated xctestrun target's EnvironmentVariables dictionary, then execute
# that exact test run file below. Xcode emits either the documented format-v2
# configuration array or the earlier format-v1 top-level target dictionary.
xctestrun_files=("$derived_data"/Build/Products/*.xctestrun(N))
(( ${#xctestrun_files} == 1 )) || {
  print -u2 -- "expected exactly one generated macOS xctestrun file"
  exit 1
}
xctestrun_file=${xctestrun_files[1]}
[[ -f "$xctestrun_file" && ! -L "$xctestrun_file" ]] || {
  print -u2 -- "generated macOS xctestrun file is unsafe"
  exit 1
}
python3 - "$xctestrun_file" \
  "$real_responder_port" "$real_responder_token" \
  "$real_source_root" "$real_destination_root" <<'PY'
import json
import os
import plistlib
import stat
import sys
import tempfile

path = sys.argv[1]
values = dict(zip(
    (
        "COVALENT_REAL_UI_RESPONDER_PORT",
        "COVALENT_REAL_UI_RESPONDER_TOKEN_FILE",
        "COVALENT_REAL_UI_SOURCE_ROOT",
        "COVALENT_REAL_UI_DESTINATION_ROOT",
    ),
    sys.argv[2:],
    strict=True,
))
with open(path, "rb") as source:
    document = plistlib.load(source)
if not isinstance(document, dict):
    raise SystemExit("generated xctestrun root is malformed")

metadata = document.get("__xctestrun_metadata__")
if metadata is None:
    format_version = 1
elif isinstance(metadata, dict):
    format_version = metadata.get("FormatVersion")
else:
    raise SystemExit("generated xctestrun has unsupported metadata")
if format_version == 1:
    target = document.get("CovalentMacUITests")
    targets = [target] if isinstance(target, dict) else []
elif format_version == 2:
    targets = [
        target
        for configuration in document.get("TestConfigurations", [])
        if isinstance(configuration, dict) and configuration.get("IsEnabled", True)
        for target in configuration.get("TestTargets", [])
        if isinstance(target, dict) and target.get("BlueprintName") == "CovalentMacUITests"
    ]
else:
    raise SystemExit("generated xctestrun has unsupported metadata")
if len(targets) != 1:
    # Keep failure diagnostics structural: never print environment dictionaries,
    # command arguments, tokens, or full filesystem paths.
    diagnostic = {
        "formatVersion": format_version,
        "topLevelKeys": sorted(str(key)[:80] for key in document)[:16],
        "format2Blueprints": [
            str(target.get("BlueprintName"))[:80]
            for configuration in document.get("TestConfigurations", [])[:16]
            if isinstance(configuration, dict)
            for target in configuration.get("TestTargets", [])[:16]
            if isinstance(target, dict)
        ],
    }
    print("xctestrun target structure: " + json.dumps(diagnostic, sort_keys=True), file=sys.stderr)
    raise SystemExit("generated xctestrun does not contain exactly one CovalentMacUITests target")
environment = targets[0].setdefault("EnvironmentVariables", {})
if not isinstance(environment, dict):
    raise SystemExit("generated xctestrun test environment is malformed")
environment.update(values)
metadata = os.stat(path, follow_symlinks=False)
descriptor, temporary = tempfile.mkstemp(prefix=".covalent-xctestrun.", dir=os.path.dirname(path))
try:
    with os.fdopen(descriptor, "wb") as output:
        plistlib.dump(document, output, fmt=plistlib.FMT_BINARY, sort_keys=False)
        output.flush()
        os.fsync(output.fileno())
    os.chmod(temporary, stat.S_IMODE(metadata.st_mode))
    os.replace(temporary, path)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
PY

# Xcode 26 can sign the generated macOS UI-test runner before its embedded
# test bundle is finalized. Re-seal the complete runner, verify it, and then
# execute without rebuilding so testmanagerd can attach to a valid worker.
runner="$derived_data/Build/Products/Debug/CovalentMacUITests-Runner.app"
test -d "$runner"
codesign \
  --force \
  --deep \
  --sign - \
  --timestamp=none \
  --preserve-metadata=identifier,entitlements,requirements,flags \
  "$runner"
codesign --verify --deep --strict --verbose=2 "$runner"

# A hang detector, not a quality gate: it decides when to kill a wedged run,
# not whether the app is fast enough. CI run 32461742319 executed the
# then-three-test suite in 43s on a real runner; the six-test suite remains
# within the same deliberately generous 480s hang detector. 900s was
# set when this lane had never passed and nothing had been measured.
managed_app_may_have_started=1
if ! run_bounded 480 xcodebuild \
  -quiet \
  -xctestrun "$xctestrun_file" \
  -destination 'platform=macOS,arch=arm64' \
  -destination-timeout 30 \
  -parallel-testing-enabled NO \
  -maximum-parallel-testing-workers 1 \
  -only-testing:CovalentMacUITests \
  -resultBundlePath "$result_bundle" \
  -test-timeouts-enabled YES \
  -default-test-execution-time-allowance 120 \
  -maximum-test-execution-time-allowance 240 \
  test-without-building >"$ui_log" 2>&1; then
  # `tail` alone is not enough. A failing run ends with several hundred lines
  # of codesign and launch chatter, so the failures themselves — and the audit
  # findings the accessibility test prints — scroll off the end of any tail
  # worth reading. Pull them out by name first.
  emit_failed_result_details
  print -u2 -- "--- audit findings and test failures ---"
  grep -n -A3 -E 'COVALENT-AUDIT-FINDING|error: -\[|XCTAssert' "$ui_log" | tail -200 >&2 || true
  print -u2 -- "--- last 240 lines ---"
  tail -240 "$ui_log" >&2
  codesign --verify --deep --strict --verbose=4 "$runner" >&2 || true
  pgrep -alf 'xcodebuild|CovalentMacUITests|testmanagerd' >&2 || true
  exit 1
fi

# Xcode can return success after leaving only a partial result directory. Do
# not let a missing or unreadable report turn an aborted UI run into a pass.
if [[ ! -f "$result_bundle/Info.plist" ]]; then
  print -u2 -- "macOS UI test result bundle is incomplete: missing Info.plist."
  exit 1
fi
if ! summary=$(xcrun xcresulttool get test-results summary --compact --path "$result_bundle"); then
  print -u2 -- "macOS UI test result bundle summary could not be parsed."
  exit 1
fi
if ! tests=$(xcrun xcresulttool get test-results tests --compact --path "$result_bundle"); then
  print -u2 -- "macOS UI test result bundle test list could not be parsed."
  exit 1
fi
if ! jq -e '
  .result == "Passed" and
  .totalTestCount == 6 and
  .passedTests == 6 and
  .failedTests == 0 and
  .skippedTests == 0
' <<<"$summary" >/dev/null; then
  print -u2 -- "macOS UI test result did not prove exactly six passing, unskipped tests."
  print -u2 -- "$summary"
  exit 1
fi
for expected_test in \
  'testFirstLaunchChoiceCanSetUpAndRecoveryCancelLeavesChoiceVisible()' \
  'testTierOneNavigationAndPrimaryWorkflowsAreReachable()' \
  'testStatusPassesSystemAccessibilityAudit()' \
  'testLinksPassesSystemAccessibilityAudit()' \
  'testNativeMenuBarQuickActionsAreReachable()' \
  'testNativeManualFolderLinkTransfersOneWayAndUpdatesMenuBar()'
do
  if ! jq -e --arg expected_test "$expected_test" '
    [.. | objects | select(.nodeType == "Test Case") | .name] |
    any(. == $expected_test)
  ' <<<"$tests" >/dev/null; then
    print -u2 -- "macOS UI test result is missing expected test: $expected_test"
    exit 1
  fi
done
cat "$ui_log"
