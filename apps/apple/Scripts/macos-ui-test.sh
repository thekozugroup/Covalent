#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-macos-ui.XXXXXX")
node_pid=""
app_token_directory=""
app_token_file=""
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
  # Only remove this run's unique file. Never reset or delete the app
  # container, which may hold a developer's unrelated sandboxed state.
  if [[ -n "$app_token_file" && -n "$app_token_directory" && "$app_token_file" == "$app_token_directory/"* ]]; then
    rm -f -- "$app_token_file"
  fi
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  if [[ "$test_root" == *covalent-macos-ui.* && -d "$test_root" ]]; then
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

console_session=$(ioreg -n Root -d1)
if [[ "$console_session" == *'"CGSSessionScreenIsLocked"=Yes'* ]]; then
  print -u2 -- "macOS UI tests require an unlocked headed login session; the current session is locked."
  exit 75
fi

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
# not whether the app is fast enough. CI run 32461742319 executed all three
# tests in 43s on a real runner, so 480s is roughly ten times the observed
# cost and still kills a hang inside a job's patience. 900s was set when this
# lane had never passed and nothing had been measured.
if ! run_bounded 480 xcodebuild \
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
  -resultBundlePath "$result_bundle" \
  -test-timeouts-enabled YES \
  -default-test-execution-time-allowance 120 \
  -maximum-test-execution-time-allowance 240 \
  test-without-building >"$ui_log" 2>&1; then
  # `tail` alone is not enough. A failing run ends with several hundred lines
  # of codesign and launch chatter, so the failures themselves — and the audit
  # findings the accessibility test prints — scroll off the end of any tail
  # worth reading. Pull them out by name first.
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
  .totalTestCount == 3 and
  .passedTests == 3 and
  .failedTests == 0 and
  .skippedTests == 0
' <<<"$summary" >/dev/null; then
  print -u2 -- "macOS UI test result did not prove exactly three passing, unskipped tests."
  print -u2 -- "$summary"
  exit 1
fi
for expected_test in \
  'testTierOneNavigationAndPrimaryWorkflowsAreReachable()' \
  'testOverviewPassesSystemAccessibilityAudit()' \
  'testNativeMenuBarQuickActionsAreReachable()'
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
