#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-ios-ui.XXXXXX")
node_pid=""
app_token_directory=""
app_token_file=""
simulator_id=""
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
  # Remove only this run's unique file, never the simulator app container or
  # its broader data directory.
  if [[ -n "$app_token_file" && -n "$app_token_directory" && "$app_token_file" == "$app_token_directory/"* ]]; then
    rm -f -- "$app_token_file"
  fi
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  if [[ "$test_root" == *covalent-ios-ui.* && -d "$test_root" ]]; then
    rm -r "$test_root"
  fi
}
trap cleanup EXIT INT TERM

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

port=$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
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
  --platform-tier tier2 \
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

requested_destination=${COVALENT_IOS_DESTINATION:-platform=iOS Simulator,name=iPhone 17 Pro,OS=latest}
simulator_id=$(python3 - "$requested_destination" <<'PY'
import json
import re
import subprocess
import sys

requested = sys.argv[1]
parts = {}
for item in requested.split(","):
    if "=" not in item:
        raise SystemExit("iOS UI-test destination is malformed")
    key, value = item.split("=", 1)
    parts[key.strip().lower()] = value.strip()
if parts.get("platform") != "iOS Simulator":
    raise SystemExit("iOS UI-test destination must explicitly name platform=iOS Simulator")

devices = json.loads(subprocess.check_output(
    ["xcrun", "simctl", "list", "devices", "available", "--json"], text=True
))["devices"]
available = [
    (runtime, device)
    for runtime, runtime_devices in devices.items()
    if ".iOS-" in runtime
    for device in runtime_devices
    if device.get("isAvailable")
]
requested_id = parts.get("id")
if requested_id:
    matches = [(runtime, device) for runtime, device in available if device.get("udid") == requested_id]
    if len(matches) != 1:
        raise SystemExit("iOS UI-test destination does not identify one available simulator")
    runtime, device = matches[0]
    if "name" in parts and device.get("name") != parts["name"]:
        raise SystemExit("iOS UI-test simulator id does not match the requested name")
else:
    name = parts.get("name")
    if not name:
        raise SystemExit("iOS UI-test destination must provide an explicit simulator id or name")
    matches = [(runtime, device) for runtime, device in available if device.get("name") == name]
    requested_os = parts.get("os", "latest")
    if requested_os != "latest":
        suffix = ".iOS-" + requested_os.replace(".", "-")
        matches = [(runtime, device) for runtime, device in matches if runtime.endswith(suffix)]
    if not matches:
        raise SystemExit("no available iOS simulator matches the requested destination")
    def version_key(item):
        version = item[0].rsplit(".iOS-", 1)[1]
        return tuple(int(piece) for piece in re.findall(r"\d+", version))
    matches.sort(key=lambda item: (version_key(item), item[1]["udid"]), reverse=True)
    runtime, device = matches[0]

print(device["udid"])
PY
)
destination="platform=iOS Simulator,id=$simulator_id"
only_testing=${COVALENT_IOS_ONLY_TESTING:-CovalentIOSUITests}

cd "$apple_dir"
xcodegen generate --quiet
derived_data="$test_root/DerivedData"
build_log="$artifact_root/build-for-testing.log"
ui_log="$artifact_root/ui-test.log"
result_bundle="$artifact_root/IOSUITests.xcresult"

# Resolve the caller's allowed destination to one exact UDID before any simctl
# operation. This prevents a mutable name/latest selector from provisioning a
# credential into a different simulator than the test runner uses.
xcrun simctl boot "$simulator_id" >/dev/null 2>&1 || true
xcrun simctl bootstatus "$simulator_id" -b >/dev/null

if ! run_bounded 600 xcodebuild \
  -quiet \
  -project Covalent.xcodeproj \
  -scheme CovalentIOS \
  -configuration Debug \
  -xcconfig "$test_settings" \
  -derivedDataPath "$derived_data" \
  -destination "$destination" \
  -only-testing:"$only_testing" \
  -destination-timeout 30 \
  -parallel-testing-enabled NO \
  -maximum-parallel-testing-workers 1 \
  CODE_SIGNING_ALLOWED=NO \
  build-for-testing >"$build_log" 2>&1; then
  tail -200 "$build_log" >&2
  exit 1
fi

# Install exactly the app build the test runner will launch, obtain only that
# app's data container, and provision the relative path recorded in the test
# bundle. The token itself never enters an xcconfig, build setting, or process
# environment.
app_bundle="$derived_data/Build/Products/Debug-iphonesimulator/Covalent.app"
if [[ ! -d "$app_bundle" ]]; then
  print -u2 -- "iOS UI-test app bundle was not built."
  exit 1
fi
xcrun simctl install "$simulator_id" "$app_bundle"
app_data_container=$(xcrun simctl get_app_container "$simulator_id" life.michaelwong.covalent.ios data | tr -d '\r')
if [[ ! -d "$app_data_container" ]]; then
  print -u2 -- "iOS UI-test app data container is unavailable."
  exit 1
fi
app_token_directory="$app_data_container/Library/Application Support/CovalentUITests"
prepare_private_ui_token_directory "$app_token_directory"
app_token_file="$app_token_directory/$ui_token_filename"
python3 "$script_dir/copy-owner-only-token.py" "$token_file" "$app_token_file"

# Bound the test phase generously. This is a hang detector, not a quality
# gate: the 240s it used to carry was expiring during cold simulator install
# and launch on a contended runner, failing runs whose code was fine -- no
# "Testing started" ever appeared in those logs. The real assertions are the
# xcresult checks below, which are untouched.
# The surrounding job allows 45 minutes.
if ! run_bounded 900 xcodebuild \
  -quiet \
  -project Covalent.xcodeproj \
  -scheme CovalentIOS \
  -configuration Debug \
  -xcconfig "$test_settings" \
  -derivedDataPath "$derived_data" \
  -destination "$destination" \
  -only-testing:"$only_testing" \
  -destination-timeout 30 \
  -parallel-testing-enabled NO \
  -maximum-parallel-testing-workers 1 \
  -resultBundlePath "$result_bundle" \
  -test-timeouts-enabled YES \
  -default-test-execution-time-allowance 240 \
  -maximum-test-execution-time-allowance 360 \
  CODE_SIGNING_ALLOWED=NO \
  test-without-building >"$ui_log" 2>&1; then
  tail -240 "$ui_log" >&2
  exit 1
fi

if [[ ! -f "$result_bundle/Info.plist" ]]; then
  print -u2 -- "iOS UI test result bundle is incomplete: missing Info.plist."
  exit 1
fi
summary=$(xcrun xcresulttool get test-results summary --compact --path "$result_bundle")
tests=$(xcrun xcresulttool get test-results tests --compact --path "$result_bundle")
if ! jq -e '
  .result == "Passed" and
  .totalTestCount == 2 and
  .passedTests == 2 and
  .failedTests == 0 and
  .skippedTests == 0
' <<<"$summary" >/dev/null; then
  print -u2 -- "iOS UI test result did not prove exactly two passing, unskipped tests."
  print -u2 -- "$summary"
  exit 1
fi
for expected_test in \
  'testTierTwoPrimaryWorkflowsAreReachable()' \
  'testHomePassesSystemAccessibilityAudit()'
do
  if ! jq -e --arg expected_test "$expected_test" '
    [.. | objects | select(.nodeType == "Test Case") | .name] |
    any(. == $expected_test)
  ' <<<"$tests" >/dev/null; then
    print -u2 -- "iOS UI test result is missing expected test: $expected_test"
    exit 1
  fi
done
cat "$ui_log"
