#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-openapi-routes.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM

# Copy the tracked repository without build products, then overlay this working
# tree's checker and the deliberately tiny Android sources below. Initializing a
# fresh index keeps the checker's fail-closed `git ls-files` inventory active.
git -C "$repo_root" checkout-index --all --prefix="$fixture_root/"
cp "$repo_root/scripts/check-openapi-routes.mjs" "$fixture_root/scripts/check-openapi-routes.mjs"
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

echo "OpenAPI route handoff fixtures: ok"
