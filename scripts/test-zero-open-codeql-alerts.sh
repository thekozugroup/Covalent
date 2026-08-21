#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
checker="${root}/scripts/check-zero-open-codeql-alerts.sh"
temporary=$(mktemp -d)
trap 'rm -rf "${temporary}"' EXIT HUP INT TERM

printf '[[], []]\n' | "${checker}"
printf '[{"number": 1, "rule": {"id": "java/example"}, "most_recent_instance": {"location": {"path": "Example.kt", "start_line": 7}}}]\n' > "${temporary}/open.json"
if "${checker}" < "${temporary}/open.json"; then
  echo "CodeQL alert policy accepted an open alert" >&2
  exit 1
fi

echo "CodeQL alert policy fixture: ok"
