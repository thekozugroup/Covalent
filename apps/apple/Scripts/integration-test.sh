#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-apple-integration.XXXXXX")
node_pid=""
probe_pid=""

cleanup() {
  if [[ -n "$probe_pid" ]]; then
    kill "$probe_pid" 2>/dev/null || true
    wait "$probe_pid" 2>/dev/null || true
  fi
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  if [[ "$test_root" == *covalent-apple-integration.* && -d "$test_root" ]]; then
    rm -r "$test_root"
  fi
}
trap cleanup EXIT INT TERM

run_expected_failure() {
  local log_file=$1
  shift
  "$@" >"$log_file" 2>&1 &
  probe_pid=$!
  for _ in {1..100}; do
    if ! kill -0 "$probe_pid" 2>/dev/null; then
      local exit_code
      set +e
      wait "$probe_pid"
      exit_code=$?
      set -e
      probe_pid=""
      if (( exit_code == 0 )); then
        print -u2 -- "Expected command to fail closed, but it exited successfully."
        return 1
      fi
      return 0
    fi
    sleep 0.05
  done
  kill "$probe_pid" 2>/dev/null || true
  wait "$probe_pid" 2>/dev/null || true
  probe_pid=""
  print -u2 -- "Expected command did not fail closed within five seconds."
  return 1
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
source_dir="$test_root/source"
restore_dir="$test_root/restore"
mkdir -p "$data_dir" "$source_dir" "$restore_dir"

cargo build --locked -p covalent-node --manifest-path "$repo_root/Cargo.toml"
node_binary="$repo_root/target/debug/covalent-node"
key_file="$test_root/node-kek"
wrong_key_file="$test_root/wrong-node-kek"
token_file="$test_root/test-api-token"
python3 - "$token_file" <<'PY'
import base64
import os
import secrets
import sys

token = base64.urlsafe_b64encode(secrets.token_bytes(48)).rstrip(b"=") + b"\n"
descriptor = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(descriptor, "wb") as output:
    output.write(token)
PY

# Mandatory key protection must fail before creating local identity state.
run_expected_failure "$test_root/missing-key.log" \
  env -u COVALENT_KEY_ENCRYPTION_KEY_FILE -u COVALENT_KEY_ENCRYPTION_KEY_VERSION \
  "$node_binary" serve \
  --listen "127.0.0.1:0" \
  --peer-listen "127.0.0.1:0" \
  --data-dir "$test_root/missing-key-node" \
  --device-name "Missing Key Probe" \
  --platform-tier tier1
grep -q "key protection is locked" "$test_root/missing-key.log"
[[ ! -e "$test_root/missing-key-node/identity.json" ]]

"$node_binary" provision-key --key-file "$key_file" --key-version 1 \
  >"$test_root/provision-key.log"
[[ "$(stat -f '%Lp' "$key_file")" == "600" ]]

start_node() {
  "$node_binary" serve \
  --listen "127.0.0.1:$port" \
  --peer-listen "127.0.0.1:$port" \
  --data-dir "$data_dir" \
  --device-name "Apple Test Node" \
  --platform-tier tier1 \
  --key-encryption-key-file "$key_file" \
  --key-encryption-key-version 1 \
  --api-token-file "$token_file" \
  >"$test_root/node.log" 2>&1 &
  node_pid=$!

  for _ in {1..100}; do
    if curl --fail --silent "http://127.0.0.1:$port/healthz" >/dev/null; then
      return 0
    fi
    sleep 0.1
  done

  sed -n '1,200p' "$test_root/node.log"
  print -u2 -- "the protected node did not become healthy"
  return 1
}

stop_node() {
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
    node_pid=""
  fi
}

# Create wrapped state, then prove a different valid 32-byte key cannot open it.
start_node
stop_node
"$node_binary" provision-key --key-file "$wrong_key_file" --key-version 1 \
  >"$test_root/provision-wrong-key.log"
[[ "$(stat -f '%Lp' "$wrong_key_file")" == "600" ]]
run_expected_failure "$test_root/wrong-key.log" \
  "$node_binary" serve \
  --listen "127.0.0.1:$port" \
  --peer-listen "127.0.0.1:$port" \
  --data-dir "$data_dir" \
  --device-name "Wrong Key Probe" \
  --platform-tier tier1 \
  --key-encryption-key-file "$wrong_key_file" \
  --key-encryption-key-version 1
grep -q "cryptographic authentication failed" "$test_root/wrong-key.log"

# The failed attempt must not damage state encrypted by the correct key.
start_node

# One name, used both to select the test and to prove it ran. Two copies of
# the string would let a rename break the selector while the proof still
# looked for the old name.
integration_test=realDaemonBackupVerifyAndRestore
test_log="$test_root/swift-test.log"

set +e
COVALENT_INTEGRATION_BASE_URL="http://127.0.0.1:$port" \
COVALENT_INTEGRATION_TOKEN_FILE="$token_file" \
COVALENT_INTEGRATION_SOURCE="$source_dir" \
COVALENT_INTEGRATION_RESTORE="$restore_dir" \
swift test --package-path "$apple_dir" --filter "$integration_test" 2>&1 | tee "$test_log"
swift_test_status=${pipestatus[1]}
set -e
if (( swift_test_status != 0 )); then
  exit "$swift_test_status"
fi

# `swift test --filter` exits 0 when the pattern matches nothing: it prints
# "Test run with 0 tests in 0 suites passed" and returns success. One rename
# and this script would prove nothing while staying green. The test is also
# `.enabled(if:)`-gated on the environment set above, so a variable that
# stopped being exported would make it *skip* — which also exits 0.
#
# An exit code is therefore not evidence here. Demand the positive statement.
if ! grep -q "Test ${integration_test}() passed" "$test_log"; then
  print -u2 -- "swift test exited 0 but never reported ${integration_test} as passing."
  print -u2 -- "Either the filter matched nothing (a rename), or the test was skipped."
  exit 1
fi
if grep -q "Test ${integration_test}() skipped" "$test_log"; then
  print -u2 -- "${integration_test} was skipped, so this run proved nothing about the daemon."
  exit 1
fi
