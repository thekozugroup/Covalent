#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-openapi-routes.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM

# Copy current tracked files, including unstaged changes, without build products.
# checkout-index would silently test stale staged bytes during local validation.
# A missing tracked file is an error. The fresh index keeps the checker's
# fail-closed `git ls-files` inventory active in every fixture below.
python3 - "$repo_root" "$fixture_root" <<'PY_COPY'
import os
from pathlib import Path
import shutil
import subprocess
import sys

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
tracked = subprocess.check_output(["git", "-C", str(source), "ls-files", "-z"])
for raw in tracked.split(b"\0"):
    if not raw:
        continue
    relative = Path(os.fsdecode(raw))
    if relative.is_absolute() or ".." in relative.parts:
        raise SystemExit("Invalid tracked path in route fixture")
    target = destination / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source / relative, target, follow_symlinks=False)
PY_COPY
mkdir -p "$fixture_root/fixtures/openapi-routes"
cp "$repo_root"/fixtures/openapi-routes/*.kt "$fixture_root/fixtures/openapi-routes/"
git -C "$fixture_root" init --quiet
git -C "$fixture_root" add -f -- .

journal_target="$fixture_root/apps/android/app/src/main/java/life/michaelwong/covalent/data/DurableTransferJournal.kt"
bridge_target="$fixture_root/apps/android/app/src/main/java/life/michaelwong/covalent/data/SafTransferBridge.kt"

run_fixture() {
  journal_fixture=$1
  bridge_fixture=$2
  cp "$repo_root/fixtures/openapi-routes/$journal_fixture" "$journal_target"
  cp "$repo_root/fixtures/openapi-routes/$bridge_fixture" "$bridge_target"
  git -C "$fixture_root" add -f -- "$journal_target" "$bridge_target"
  node "$fixture_root/scripts/check-openapi-routes.mjs"
}

run_fixture DurableTransferJournal.valid.kt SafTransferBridge.valid.kt >/dev/null

unknown_output="$fixture_root/unknown-path.out"
if run_fixture DurableTransferJournal.unknown.kt SafTransferBridge.valid.kt >"$unknown_output" 2>&1; then
  echo "route checker accepted an unknown durable path constant" >&2
  exit 1
fi
grep -q 'HANDOFF /api/v1/backups/archive-unknown.*no router path matches' "$unknown_output"

method_output="$fixture_root/wrong-method.out"
if run_fixture DurableTransferJournal.valid.kt SafTransferBridge.wrong-method.kt >"$method_output" 2>&1; then
  echo "route checker accepted the wrong method for an indirect durable path" >&2
  exit 1
fi
grep -q 'GET /api/v1/backups/archive.*the router serves only POST there' "$method_output"

issuer_output="$fixture_root/missing-issuer.out"
if run_fixture DurableTransferJournal.valid.kt SafTransferBridge.missing-backup.kt >"$issuer_output" 2>&1; then
  echo "route checker allowed another client platform to satisfy an Android handoff" >&2
  exit 1
fi
grep -q '/api/v1/backups/archive.*Android client.*stored as a path constant but the client never issues it' "$issuer_output"

run_fixture DurableTransferJournal.valid.kt SafTransferBridge.valid.kt >/dev/null
python3 - "$fixture_root/packaging/web/folder-sync-flow.js" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
source = path.read_text()
expected = 'options.api(path, { method: "POST", body: JSON.stringify(body) })'
assert source.count(expected) == 1
path.write_text(source.replace(expected, expected.replace('"POST"', '"GET"')))
PY
web_method_output="$fixture_root/web-wrong-method.out"
if node "$fixture_root/scripts/check-openapi-routes.mjs" >"$web_method_output" 2>&1; then
  echo "route checker accepted a GET folder mutation" >&2
  exit 1
fi
grep -q 'GET /api/v1/sync/folders.*the router serves only POST there' "$web_method_output"

echo "OpenAPI route handoff fixtures: ok"
