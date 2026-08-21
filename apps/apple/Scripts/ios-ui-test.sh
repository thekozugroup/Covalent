#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-ios-ui.XXXXXX")
node_pid=""
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
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  if [[ "$test_root" == *covalent-ios-ui.* && -d "$test_root" ]]; then
    rm -r "$test_root"
  fi
}
trap cleanup EXIT INT TERM

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
"$repo_root/target/debug/covalent-node" serve \
  --listen "127.0.0.1:$port" \
  --peer-listen "127.0.0.1:$port" \
  --data-dir "$data_dir" \
  --device-name "Apple UI Test Node" \
  --platform-tier tier2 \
  >"$test_root/node.log" 2>&1 &
node_pid=$!

for _ in {1..100}; do
  if [[ -s "$data_dir/local-api-token" ]] && curl --fail --silent "http://127.0.0.1:$port/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
if [[ ! -s "$data_dir/local-api-token" ]]; then
  sed -n '1,200p' "$test_root/node.log"
  exit 1
fi

token=$(tr -d '\r\n' < "$data_dir/local-api-token")
test_settings="$test_root/TestSecrets.xcconfig"
print -r -- "COVALENT_UI_TEST_PORT = $port" > "$test_settings"
print -r -- "COVALENT_UI_TEST_TOKEN = $token" >> "$test_settings"
destination=${COVALENT_IOS_DESTINATION:-platform=iOS Simulator,name=iPhone 17 Pro,OS=latest}
only_testing=${COVALENT_IOS_ONLY_TESTING:-CovalentIOSUITests}

cd "$apple_dir"
xcodegen generate --quiet
derived_data="$test_root/DerivedData"
build_log="$artifact_root/build-for-testing.log"
ui_log="$artifact_root/ui-test.log"
result_bundle="$artifact_root/IOSUITests.xcresult"

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
  -default-test-execution-time-allowance 60 \
  -maximum-test-execution-time-allowance 120 \
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
