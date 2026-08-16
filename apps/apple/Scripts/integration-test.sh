#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
apple_dir=${script_dir:h}
repo_root=${apple_dir:h:h}
test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-apple-integration.XXXXXX")
node_pid=""

cleanup() {
  if [[ -n "$node_pid" ]]; then
    kill "$node_pid" 2>/dev/null || true
    wait "$node_pid" 2>/dev/null || true
  fi
  if [[ "$test_root" == *covalent-apple-integration.* && -d "$test_root" ]]; then
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
source_dir="$test_root/source"
restore_dir="$test_root/restore"
mkdir -p "$data_dir" "$source_dir" "$restore_dir"

cargo build --locked -p covalent-node --manifest-path "$repo_root/Cargo.toml"
"$repo_root/target/debug/covalent-node" serve \
  --listen "127.0.0.1:$port" \
  --peer-listen "127.0.0.1:$port" \
  --data-dir "$data_dir" \
  --device-name "Apple Test Node" \
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
  echo "local API token was not created" >&2
  exit 1
fi

token=$(tr -d '\r\n' < "$data_dir/local-api-token")
COVALENT_INTEGRATION_BASE_URL="http://127.0.0.1:$port" \
COVALENT_INTEGRATION_TOKEN="$token" \
COVALENT_INTEGRATION_SOURCE="$source_dir" \
COVALENT_INTEGRATION_RESTORE="$restore_dir" \
swift test --package-path "$apple_dir" --filter realDaemonBackupVerifyAndRestore
