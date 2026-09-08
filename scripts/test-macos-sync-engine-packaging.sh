#!/bin/sh
# Fast, offline source contract for the macOS sync-engine bundle builder.
# Set SYNCTHING_SOURCE_DIR and COVALENT_GO_BIN to run the complete operation.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
package_script="$repo_root/apps/apple/Scripts/package-sync-engine.sh"
sign_script="$repo_root/apps/apple/Scripts/sign-sync-engine-bundle.sh"
build_script="$repo_root/scripts/build-macos-sync-engine.sh"
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
provenance="$repo_root/apps/apple/SyncEngine/PROVENANCE.txt"

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

for script in "$package_script" "$sign_script" "$build_script"; do
  test -x "$script" || fail "sync-engine packaging script is not executable"
  sh -n "$script"
done

test "$(shasum -a 256 "$guardian_source" | awk '{print $1}')" = \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579 ||
  fail "reviewed engine guardian source changed"

for contract in \
  v2.1.3 \
  946e2b83a1f6c6ae119427c09e0a5802940b82ff \
  dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959 \
  eb60efd57d1662af75ffb2f7b89abab7200362c654838486138a34bee00fed29 \
  4df1dea892fac3c9d1c0e822827c9a4c57711dcddaaf56a61edaab41d5337bbb \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579 \
  Syncthing-LICENSE.txt \
  Syncthing-AUTHORS.txt \
  THIRD-PARTY-NOTICES.txt \
  source-build.json \
  'xcrun clang' \
  '-arch arm64' \
  '-mmacosx-version-min=15.0' \
  'sign-sync-engine-bundle.sh'
do
  grep -Fq -- "$contract" "$package_script" "$sign_script" "$build_script" ||
    fail "missing package contract: $contract"
done

for contract in \
  'com.apple.security.inherit' \
  'signedSha256' \
  'json.dumps(value, sort_keys=True' \
  '--timestamp=none' \
  '--options runtime'
do
  grep -Fq -- "$contract" "$sign_script" "$repo_root/apps/apple/Config/CovalentNode.entitlements" ||
    fail "missing signing contract: $contract"
done

grep -Fq 'Mozilla Public License 2.0' "$provenance" ||
  fail "sync-engine provenance must state the upstream license"
grep -Fq 'Corresponding source:' "$provenance" ||
  fail "sync-engine provenance must retain a corresponding-source location"

if rg -n 'global|local discovery|relay|upgrad|browser|telemetry|crash' \
  "$package_script" "$sign_script" >/dev/null 2>&1; then
  fail "packaging must not add an engine network or updater launch path"
fi

if [ -n "${SYNCTHING_SOURCE_DIR:-}" ] && [ -n "${COVALENT_GO_BIN:-}" ]; then
  if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    fail "the complete sync-engine package test requires Apple Silicon macOS"
  fi
  test_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-sync-engine-test.XXXXXX")
  cleanup() {
    python3 - "$test_root" <<'PY'
import shutil
import sys
shutil.rmtree(sys.argv[1], ignore_errors=True)
PY
  }
  trap cleanup EXIT HUP INT TERM

  # The output directory is intentionally retained for diagnostics, but no
  # extracted toolchain, module cache, or source export may survive even an
  # error that occurs before the Go executable can be validated.
  printf 'not the pinned Go archive\n' > "$test_root/invalid-go.tar.gz"
  if COVALENT_GO_ARCHIVE="$test_root/invalid-go.tar.gz" \
    "$build_script" "$SYNCTHING_SOURCE_DIR" "$test_root/early-failure" \
      >"$test_root/early-failure.stdout" 2>"$test_root/early-failure.stderr"; then
    fail "source builder accepted an invalid Go archive"
  fi
  test -d "$test_root/early-failure" ||
    fail "source builder did not create its owned diagnostic output"
  test ! -e "$test_root/early-failure/.private-build" &&
    test ! -L "$test_root/early-failure/.private-build" ||
    fail "source builder retained private state after an early failure"
  test -z "$(find "$test_root/early-failure" -mindepth 1 -print -quit)" ||
    fail "source builder retained unexpected output after an early failure"

  COVALENT_SYNCTHING_SOURCE_DIR=$SYNCTHING_SOURCE_DIR \
    COVALENT_GO_BIN=$COVALENT_GO_BIN \
    DERIVED_FILE_DIR="$test_root/derived" \
    "$package_script" "$test_root/MacOS" "$test_root/Resources/CovalentSyncEngine"
  for binary in "$test_root/MacOS/covalent-engine-guardian" \
    "$test_root/MacOS/covalent-syncthing"; do
    test "$(xcrun lipo -archs "$binary")" = arm64 || fail "packaged binary is not arm64"
    codesign --verify --strict "$binary"
  done
  python3 - "$test_root" <<'PY'
import hashlib
import json
import pathlib
import sys
root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "Resources/CovalentSyncEngine/manifest.json").read_bytes())
for name, record in manifest["executables"].items():
    digest = hashlib.sha256((root / "MacOS" / name).read_bytes()).hexdigest()
    if digest != record["signedSha256"]:
        raise SystemExit("post-sign manifest digest mismatch")
PY

  COVALENT_SYNCTHING_SOURCE_DIR=$SYNCTHING_SOURCE_DIR \
    COVALENT_GO_BIN=$COVALENT_GO_BIN \
    DERIVED_FILE_DIR="$test_root/repeat-derived" \
    "$package_script" "$test_root/repeat/MacOS" \
      "$test_root/repeat/Resources/CovalentSyncEngine" >/dev/null
  cmp "$test_root/MacOS/covalent-engine-guardian" \
    "$test_root/repeat/MacOS/covalent-engine-guardian"
  cmp "$test_root/MacOS/covalent-syncthing" \
    "$test_root/repeat/MacOS/covalent-syncthing"
  cmp "$test_root/Resources/CovalentSyncEngine/manifest.json" \
    "$test_root/repeat/Resources/CovalentSyncEngine/manifest.json"

  mkdir -p "$test_root/unsafe/MacOS" "$test_root/unsafe/Resources/CovalentSyncEngine"
  printf '%s' preserved > "$test_root/sentinel"
  ln -s "$test_root/sentinel" "$test_root/unsafe/MacOS/covalent-syncthing"
  if COVALENT_SYNCTHING_SOURCE_DIR=$SYNCTHING_SOURCE_DIR \
    COVALENT_GO_BIN=$COVALENT_GO_BIN \
    DERIVED_FILE_DIR="$test_root/unsafe-derived" \
    "$package_script" "$test_root/unsafe/MacOS" \
      "$test_root/unsafe/Resources/CovalentSyncEngine" >/dev/null 2>&1; then
    fail "packager accepted a symlinked executable output"
  fi
  test "$(cat "$test_root/sentinel")" = preserved ||
    fail "packager changed a symlink target"
fi

echo "macOS sync-engine packaging contract: ok"
