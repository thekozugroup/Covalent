#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-smoke.XXXXXX")
node_pid=""

ports=$(python3 - <<'PY'
import socket

sockets = [socket.socket(), socket.socket()]
try:
    for sock in sockets:
        sock.bind(("127.0.0.1", 0))
    print(" ".join(str(sock.getsockname()[1]) for sock in sockets))
finally:
    for sock in sockets:
        sock.close()
PY
)
set -- $ports
api_port=$1
peer_port=$2

cleanup() {
  if [ -n "$node_pid" ]; then
    kill "$node_pid" >/dev/null 2>&1 || true
    wait "$node_pid" >/dev/null 2>&1 || true
  fi
  rm -rf "$temporary_root"
}
trap cleanup EXIT INT TERM

cd "$repo_root"
cargo run --quiet -p covalent-node -- serve \
  --listen "127.0.0.1:$api_port" \
  --peer-listen "127.0.0.1:$peer_port" \
  --data-dir "$temporary_root/node" \
  --device-name "Foundation smoke node" \
  >"$temporary_root/node.log" 2>&1 &
node_pid=$!

attempt=0
until cargo run --quiet -p covalent-node -- healthcheck --url "http://127.0.0.1:$api_port/healthz" >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then
    sed -n '1,160p' "$temporary_root/node.log" >&2
    exit 1
  fi
  sleep 1
done

cargo run --quiet -p covalent-cli -- validate-restore-path \
  --root "$temporary_root" \
  --relative "restore/nested.txt" \
  >/dev/null

kill "$node_pid"
wait "$node_pid"
node_pid=""

mkdir -p "$temporary_root/source/nested/empty" "$temporary_root/restore"
printf '%s\n' "real encrypted backup content" >"$temporary_root/source/nested/file.txt"
cp "$temporary_root/source/nested/file.txt" "$temporary_root/expected.txt"
backup_json=$(cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  backup \
  --source "$temporary_root/source" \
  --name "CLI smoke backup" \
  --snapshot-id "0001" \
  --job-id "cli-smoke-backup")
backup_id=$(printf '%s\n' "$backup_json" | sed -n 's/.*"backupId": "\([^"]*\)".*/\1/p')
if [ -z "$backup_id" ]; then
  printf '%s\n' "$backup_json" >&2
  exit 1
fi

verify_json=$(cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  verify \
  --backup-id "$backup_id" \
  --snapshot-id "0001")
printf '%s\n' "$verify_json" | grep '"intact": true' >/dev/null

cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  restore-preview \
  --backup-id "$backup_id" \
  --snapshot-id "0001" \
  --target "$temporary_root/restore" \
  --job-id "cli-smoke-restore" \
  --output "$temporary_root/restore-plan.json"
rm -rf "$temporary_root/source"
cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  restore-execute \
  --plan "$temporary_root/restore-plan.json" \
  >/dev/null
cmp "$temporary_root/expected.txt" "$temporary_root/restore/nested/file.txt"
test -d "$temporary_root/restore/nested/empty"

cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  config-export \
  --output "$temporary_root/settings.json"
if grep -Ei 'private|secret|contentKey' "$temporary_root/settings.json" >/dev/null; then
  echo "settings export leaked a private field" >&2
  exit 1
fi
if cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  config-import \
  --input "$temporary_root/settings.json" \
  >/dev/null 2>&1; then
  echo "settings import succeeded without confirmation" >&2
  exit 1
fi
cargo run --quiet -p covalent-cli -- \
  --data-dir "$temporary_root/node" \
  config-import \
  --input "$temporary_root/settings.json" \
  --confirm \
  >/dev/null

echo "daemon, CLI backup/verify/restore, and safe settings smoke: ok"
