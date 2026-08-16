#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

required="cargo rustc rustfmt"
for command_name in $required; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "missing required command: $command_name" >&2
    exit 1
  fi
done

rustc_version=$(rustc --version)
case "$rustc_version" in
  "rustc 1.97.1 "*) ;;
  *)
    echo "expected Rust 1.97.1, found: $rustc_version" >&2
    exit 1
    ;;
esac

cargo fetch --locked

if command -v xcodegen >/dev/null 2>&1; then
  (cd apps/apple && xcodegen generate --quiet)
fi

echo "Covalent bootstrap complete. No secrets or hosted account used."
