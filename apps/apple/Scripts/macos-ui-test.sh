#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-macos-ui.XXXXXX")
node_pid=""

cleanup() {
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  if [[ "$test_root" == *covalent-macos-ui.* && -d "$test_root" ]]; then
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
  --platform-tier tier1 \
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

cd "$apple_dir"
xcodegen generate --quiet
xcodebuild \
  -quiet \
  -project Covalent.xcodeproj \
  -scheme CovalentMac \
  -configuration Debug \
  -xcconfig "$test_settings" \
  -destination 'platform=macOS' \
  -only-testing:CovalentMacUITests \
  test
