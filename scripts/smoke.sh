#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-smoke.XXXXXX")
node_pid=""

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
  --listen 127.0.0.1:18787 \
  --data-dir "$temporary_root/node" \
  --device-name "Foundation smoke node" \
  >"$temporary_root/node.log" 2>&1 &
node_pid=$!

attempt=0
until cargo run --quiet -p covalent-node -- healthcheck --url http://127.0.0.1:18787/healthz >/dev/null 2>&1; do
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

echo "daemon health and restore-boundary smoke: ok"
